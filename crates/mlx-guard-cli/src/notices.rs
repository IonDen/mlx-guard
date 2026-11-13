//! Stderr lines written while a command is being supervised.
//!
//! The supervising thread only ever queues a finished line. One thread owns a duplicate of fd 2 and
//! writes each line in one `write_all`, so a blocking write of up to `PIPE_BUF` bytes stays whole
//! whoever else shares the pipe. That thread may block for as long as the reader does; nothing waits
//! on it until the report is final. It starts on the first line, which is always after the launch,
//! so it never exists across the fork. It never takes std's stderr lock, which the end-of-run lines
//! need on the main thread.
//!
//! A line that does not fit in the queue, or cannot be written, is dropped: the report holds the
//! facts. `O_NONBLOCK` is not an option here, because the flag lives on the open file description
//! the command shares, and its own blocking writes would start failing with `EAGAIN`.

use std::fmt;
use std::fs::File;
#[cfg(all(debug_assertions, target_os = "macos"))]
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::AsFd;
#[cfg(debug_assertions)]
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const NOTICE_QUEUE: usize = 8;
/// How long a clean exit gives one write that has begun and not returned: a stderr pipe nobody
/// reads must not hold it up.
const NOTICE_DRAIN_WAIT: Duration = Duration::from_millis(250);
/// The most a clean exit waits for the writer in all, however late it starts or however slowly it
/// writes. Eight times the drain bound: a writer that starts late still has time to write its
/// lines, and a clean exit is never held up by more than two seconds.
const NOTICE_DRAIN_CAP: Duration = Duration::from_secs(2);
const WRITER_STACK_BYTES: usize = 64 * 1024;

/// The stderr lines of one supervised command; a new value holds no thread and no descriptor.
#[derive(Default)]
pub struct Notices {
    state: State,
    trace: Trace,
}

enum State {
    /// Nothing sent yet. The writer is only ever present in tests; otherwise it is fd 2's duplicate.
    Idle(Option<Pending>),
    Running(WriterThread),
    Closed,
}

impl Default for State {
    fn default() -> Self {
        Self::Idle(None)
    }
}

type Open = Box<dyn FnOnce() -> io::Result<Box<dyn Write + Send>> + Send>;
type Spawn = fn(thread::Builder, Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>>;

/// How the writer thread starts: the stream it will own, how the thread is spawned, where its
/// events are traced, and in tests a gate it waits on before its first line, standing in for a
/// thread the machine schedules late.
struct Pending {
    open: Open,
    spawn: Spawn,
    start_gate: Option<Receiver<()>>,
    trace: Trace,
}

impl Pending {
    /// A duplicate of fd 2, written on a thread of its own.
    fn stderr() -> Self {
        Self {
            open: Box::new(duplicate_stderr),
            spawn: spawn_thread,
            start_gate: None,
            trace: Trace::from_env(),
        }
    }
}

/// Debug builds only: the environment variable naming a file that gets one line per writer event.
#[cfg(debug_assertions)]
const NOTICE_TRACE_ENV: &str = "MLX_GUARD_NOTICE_TRACE";

/// In debug builds, one line per writer event (a line queued or dropped, the stream or the thread
/// failing to start, the thread starting, a line written, `finish` returning and why), appended to
/// the file `MLX_GUARD_NOTICE_TRACE` names, so a test that sees a notice missing can say which of
/// those happened. Release builds hold nothing here and write nothing.
#[derive(Clone, Default)]
struct Trace {
    #[cfg(debug_assertions)]
    sink: Option<Arc<TraceSink>>,
}

#[cfg(debug_assertions)]
struct TraceSink {
    origin: Instant,
    out: Mutex<Box<dyn Write + Send>>,
}

impl Trace {
    fn from_env() -> Self {
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os(NOTICE_TRACE_ENV)
            && let Ok(file) = open_trace_file(Path::new(&path))
        {
            return Self::to(Box::new(file));
        }
        Self::default()
    }

    #[cfg(debug_assertions)]
    fn to(out: Box<dyn Write + Send>) -> Self {
        Self {
            sink: Some(Arc::new(TraceSink {
                origin: Instant::now(),
                out: Mutex::new(out),
            })),
        }
    }

    #[cfg_attr(not(debug_assertions), allow(clippy::unused_self))]
    fn event(&self, what: fmt::Arguments<'_>) {
        #[cfg(debug_assertions)]
        if let Some(sink) = &self.sink {
            let at = sink.origin.elapsed().as_secs_f64() * 1000.0;
            let line = format!("{at:.1} ms {what}\n");
            lock(&sink.out).write_all(line.as_bytes()).ok();
        }
        #[cfg(not(debug_assertions))]
        let _ = what;
    }
}

/// `O_NOFOLLOW` from macOS `<sys/fcntl.h>`; a unit test holds it to `libc`'s value, and libc is
/// only a test dependency of this crate.
#[cfg(all(debug_assertions, target_os = "macos"))]
const O_NOFOLLOW: i32 = 0x0100;

/// Open the trace file `MLX_GUARD_NOTICE_TRACE` names, for appending: never through a symlink,
/// and readable by its owner only when it is new. The supervisor runs on macOS only, so other
/// targets open no trace.
#[cfg(debug_assertions)]
fn open_trace_file(path: &Path) -> io::Result<File> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW)
            .open(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the writer thread is, for a clean exit to tell a writer it should wait for (not started
/// yet, or between two lines) from one blocked inside a write.
#[derive(Default)]
struct Progress {
    /// When the line in hand went into `write_all`; `None` before the first line and between two.
    writing_since: Option<Instant>,
    /// The thread has returned or unwound: everything in the queue was written or dropped.
    ended: bool,
}

#[derive(Default)]
struct Status {
    progress: Mutex<Progress>,
    changed: Condvar,
}

impl Status {
    fn update(&self, change: impl FnOnce(&mut Progress)) {
        change(&mut lock(&self.progress));
        self.changed.notify_all();
    }
}

/// Marks the writer ended when its thread returns or unwinds, so no wait outlives a dead writer.
struct EndOnDrop(Arc<Status>);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        self.0.update(|progress| {
            progress.writing_since = None;
            progress.ended = true;
        });
    }
}

/// Why a clean exit stopped waiting for the writer.
#[derive(Clone, Copy)]
enum Drained {
    Ended,
    Blocked,
    Cap,
}

impl fmt::Display for Drained {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ended => formatter.write_str("the writer ended"),
            Self::Blocked => write!(
                formatter,
                "a write was blocked for the {} ms drain bound",
                NOTICE_DRAIN_WAIT.as_millis()
            ),
            Self::Cap => write!(
                formatter,
                "the {} s cap was reached",
                NOTICE_DRAIN_CAP.as_secs_f64()
            ),
        }
    }
}

/// Wait from `started` until the writer ends, a single write has been blocked for the drain bound
/// (counted from when it began or from `started`, whichever is later), or the cap has passed.
fn wait_for_writer(status: &Status, started: Instant) -> Drained {
    let cap = started + NOTICE_DRAIN_CAP;
    let mut progress = lock(&status.progress);
    loop {
        if progress.ended {
            return Drained::Ended;
        }
        let now = Instant::now();
        let blocked_at = progress
            .writing_since
            .map(|since| since.max(started) + NOTICE_DRAIN_WAIT);
        if let Some(blocked_at) = blocked_at
            && now >= blocked_at
        {
            return Drained::Blocked;
        }
        if now >= cap {
            return Drained::Cap;
        }
        let until = blocked_at.map_or(cap, |blocked_at| blocked_at.min(cap));
        progress = status
            .changed
            .wait_timeout(progress, until.saturating_duration_since(now))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn spawn_thread(
    builder: thread::Builder,
    body: Box<dyn FnOnce() + Send>,
) -> io::Result<JoinHandle<()>> {
    builder.spawn(body)
}

struct WriterThread {
    queue: SyncSender<String>,
    status: Arc<Status>,
    thread: JoinHandle<()>,
    trace: Trace,
}

impl WriterThread {
    fn start(pending: Pending) -> Option<Self> {
        let Pending {
            open,
            spawn,
            start_gate,
            trace,
        } = pending;
        let mut writer = match open() {
            Ok(writer) => writer,
            Err(error) => {
                trace.event(format_args!("stderr duplicate failed: {error}"));
                return None;
            }
        };
        let (queue, lines) = mpsc::sync_channel::<String>(NOTICE_QUEUE);
        let status = Arc::new(Status::default());
        let ended = EndOnDrop(Arc::clone(&status));
        let builder = thread::Builder::new()
            .name("mlx-guard-notices".to_owned())
            .stack_size(WRITER_STACK_BYTES);
        let thread_trace = trace.clone();
        let spawned = spawn(
            builder,
            Box::new(move || {
                let ended = ended;
                if let Some(gate) = start_gate {
                    gate.recv().ok();
                }
                thread_trace.event(format_args!("writer started"));
                for line in lines {
                    ended
                        .0
                        .update(|progress| progress.writing_since = Some(Instant::now()));
                    match writer.write_all(line.as_bytes()) {
                        Ok(()) => thread_trace.event(format_args!("line written")),
                        Err(error) => thread_trace.event(format_args!("line not written: {error}")),
                    }
                    ended.0.update(|progress| progress.writing_since = None);
                }
                thread_trace.event(format_args!("writer ended"));
            }),
        );
        let thread = match spawned {
            Ok(thread) => thread,
            Err(error) => {
                trace.event(format_args!("writer thread spawn failed: {error}"));
                return None;
            }
        };
        Some(Self {
            queue,
            status,
            thread,
            trace,
        })
    }
}

fn duplicate_stderr() -> io::Result<Box<dyn Write + Send>> {
    let descriptor = io::stderr().as_fd().try_clone_to_owned()?;
    Ok(Box::new(File::from(descriptor)))
}

impl Notices {
    #[cfg(test)]
    pub(crate) fn with_writer(writer: Box<dyn Write + Send>) -> Self {
        Self::starting(Pending {
            open: Box::new(move || Ok(writer)),
            spawn: spawn_thread,
            start_gate: None,
            trace: Trace::default(),
        })
    }

    #[cfg(test)]
    fn starting(pending: Pending) -> Self {
        Self {
            state: State::Idle(Some(pending)),
            trace: Trace::default(),
        }
    }

    /// Queue one newline-ended line. Never blocks and never panics; a failure to start the writer
    /// disables the lines for good.
    pub(crate) fn send(&mut self, line: String) {
        if let State::Idle(pending) = &mut self.state {
            let pending = pending.take().unwrap_or_else(Pending::stderr);
            self.trace = pending.trace.clone();
            self.state = WriterThread::start(pending).map_or(State::Closed, State::Running);
        }
        match &self.state {
            State::Running(writer) => match writer.queue.try_send(line) {
                Ok(()) => self.trace.event(format_args!("line queued")),
                Err(_) => self
                    .trace
                    .event(format_args!("line dropped: the queue is full")),
            },
            State::Idle(_) | State::Closed => {
                self.trace.event(format_args!("line dropped: no writer"));
            }
        }
    }

    /// Close the queue once the report is final. With plain lines to follow, wait as long as the
    /// writer needs, so a blocked line cannot land inside them. Otherwise wait for a writer that
    /// has not started yet or is between two lines, since its lines go out once it runs, but give
    /// a write that has begun only the drain bound, and give up on everything at the cap: a blocked
    /// thread dies with the process and no reader was promised anything.
    pub(crate) fn finish(&mut self, plain_lines_follow: bool) {
        let State::Running(writer) = std::mem::replace(&mut self.state, State::Closed) else {
            return;
        };
        let WriterThread {
            queue,
            status,
            thread,
            trace,
        } = writer;
        drop(queue);
        if plain_lines_follow {
            thread.join().ok();
            trace.event(format_args!("finish(true) returned: the writer ended"));
        } else {
            let drained = wait_for_writer(&status, Instant::now());
            trace.event(format_args!("finish(false) returned: {drained}"));
        }
    }
}

impl Drop for Notices {
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// A writer standing in for a stderr nobody reads, shared with the summary's order test.
#[cfg(test)]
pub(crate) mod stuck_writer {
    use std::io::{self, Write};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub(crate) type Timeline = Arc<Mutex<Vec<String>>>;

    /// Every write blocks until the gate is released, and only then lands on the timeline.
    struct StuckWriter {
        entered: Sender<()>,
        released: Receiver<()>,
        timeline: Timeline,
    }

    impl Write for StuckWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).ok();
            self.released.recv().ok();
            self.timeline
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub(crate) struct Gate {
        entered: Receiver<()>,
        release: Sender<()>,
    }

    impl Gate {
        pub(crate) fn wait_for_a_blocked_write(&self) {
            self.entered
                .recv_timeout(Duration::from_secs(5))
                .expect("the writer must have reached its first write");
        }

        /// Open the gate for good: every write from now on goes straight through.
        pub(crate) fn release(self) {
            drop(self.release);
        }
    }

    pub(crate) fn stuck_writer(timeline: &Timeline) -> (Box<dyn Write + Send>, Gate) {
        let (entered_sender, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let writer = StuckWriter {
            entered: entered_sender,
            released,
            timeline: Arc::clone(timeline),
        };
        (Box::new(writer), Gate { entered, release })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};
    use std::time::{Duration, Instant};

    use super::stuck_writer::{Timeline, stuck_writer};
    use super::{Notices, Pending, State, Trace, spawn_thread};

    fn timeline() -> Timeline {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn written(timeline: &Timeline) -> Vec<String> {
        timeline.lock().unwrap().clone()
    }

    #[test]
    fn send_on_a_full_queue_returns_at_once() {
        // Bug: `send` on the channel instead of `try_send`, so a stuck reader stops the sampling loop.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("line 0\n".to_owned());
        gate.wait_for_a_blocked_write();

        let (returned, all_sent) = mpsc::channel();
        thread::spawn(move || {
            for number in 1..=20 {
                notices.send(format!("line {number}\n"));
            }
            returned.send(notices).ok();
        });
        let mut notices = all_sent
            .recv_timeout(Duration::from_secs(5))
            .expect("twenty sends must return while the writer is stuck");

        gate.release();
        notices.finish(true);
        // The one in flight and the eight the queue holds; the other twelve were dropped.
        assert_eq!(
            written(&lines),
            [
                "line 0\n", "line 1\n", "line 2\n", "line 3\n", "line 4\n", "line 5\n", "line 6\n",
                "line 7\n", "line 8\n",
            ]
        );
    }

    #[test]
    fn finish_without_plain_lines_returns_within_the_drain_bound_when_the_writer_is_stuck() {
        // Bug: an unbounded `join` on the clean-exit path, so an unread pipe holds up a clean exit.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("stuck\n".to_owned());
        gate.wait_for_a_blocked_write();

        let started = Instant::now();
        let (returned, finished) = mpsc::channel();
        thread::spawn(move || {
            notices.finish(false);
            returned.send(started.elapsed()).ok();
        });
        let waited = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("finish(false) must return while the writer is stuck");

        assert!(
            waited >= Duration::from_millis(250),
            "it gave a stuck writer only {waited:?}"
        );
        assert!(waited < Duration::from_secs(2), "it waited {waited:?}");
    }

    #[test]
    fn finish_without_plain_lines_gives_a_write_blocked_before_it_the_whole_drain_bound() {
        // Bug: the drain bound counts from when the write began, so a line blocked since the
        // launch gets no time at the end, even from a reader that is only slow.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("stuck\n".to_owned());
        gate.wait_for_a_blocked_write();
        thread::sleep(Duration::from_millis(300));

        let (waited, _) = finish_clean_exit(notices, &lines)
            .recv_timeout(Duration::from_secs(5))
            .expect("finish(false) must return while the writer is stuck");

        assert!(
            waited >= Duration::from_millis(250),
            "it gave a write blocked since before it only {waited:?}"
        );
        assert!(waited < Duration::from_secs(2), "it waited {waited:?}");
    }

    #[test]
    fn finish_waits_for_a_stuck_write_when_plain_lines_follow() {
        // Bug: `finish` returns early, so a blocked line races the end-of-run text and can land
        // inside it, or queued lines are lost.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("first\n".to_owned());
        gate.wait_for_a_blocked_write();
        notices.send("second\n".to_owned());
        notices.send("third\n".to_owned());

        let (returned, finished) = mpsc::channel();
        thread::spawn(move || {
            notices.finish(true);
            returned.send(()).ok();
        });

        // Twice the clean-exit bound, so a `finish` that gave up on a timer has already returned.
        assert_eq!(
            finished.recv_timeout(Duration::from_millis(500)),
            Err(RecvTimeoutError::Timeout),
            "finish(true) returned with a write still blocked"
        );
        assert!(written(&lines).is_empty());

        gate.release();
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("finish(true) must return once the write went through");
        assert_eq!(written(&lines), ["first\n", "second\n", "third\n"]);
    }

    /// Writes nothing, and says which thread let go of it.
    struct DropWitness(Sender<ThreadId>);

    impl Write for DropWitness {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.0.send(thread::current().id()).ok();
        }
    }

    #[test]
    fn an_unused_notices_starts_no_thread() {
        // Bug: the thread starts with the value instead of on the first line, so it exists across
        // the fork that launches the command.
        let (witness, dropped_on) = mpsc::channel();
        drop(Notices::with_writer(Box::new(DropWitness(witness))));

        assert_eq!(
            dropped_on.recv_timeout(Duration::from_secs(5)),
            Ok(thread::current().id()),
            "the writer was handed to another thread before any line was sent"
        );
        assert!(
            matches!(Notices::default().state, State::Idle(None)),
            "a new Notices must hold no thread and no descriptor"
        );
    }

    #[test]
    fn finish_without_plain_lines_delivers_what_a_healthy_writer_can_take() {
        // Bug: `finish(false)` does not wait at all, so the banner of a run that exits at once is
        // lost when the process ends.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("first\n".to_owned());
        gate.wait_for_a_blocked_write();
        notices.send("second\n".to_owned());

        // The gate stays shut while `finish(false)` starts, so nothing can be written before it
        // returns unless it waited; it opens well inside the drain bound.
        let (returned, finished) = mpsc::channel();
        let snapshot = Arc::clone(&lines);
        thread::spawn(move || {
            notices.finish(false);
            returned.send(written(&snapshot)).ok();
        });
        thread::sleep(Duration::from_millis(50));
        gate.release();

        let seen_on_return = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("finish(false) must return");
        assert_eq!(seen_on_return, ["first\n", "second\n"]);
    }

    /// A writer that goes straight through, recording each line.
    fn open_writer(timeline: &Timeline) -> Box<dyn Write + Send> {
        let (writer, gate) = stuck_writer(timeline);
        gate.release();
        writer
    }

    /// Spend `pause` on every write, well inside the drain bound, then record the line.
    struct SlowWriter {
        pause: Duration,
        timeline: Timeline,
    }

    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            thread::sleep(self.pause);
            self.timeline
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Run `finish(false)` on its own thread; the result is how long it took and what had been
    /// written when it returned.
    fn finish_clean_exit(
        mut notices: Notices,
        timeline: &Timeline,
    ) -> mpsc::Receiver<(Duration, Vec<String>)> {
        let snapshot = Arc::clone(timeline);
        let (returned, finished) = mpsc::channel();
        thread::spawn(move || {
            let started = Instant::now();
            notices.finish(false);
            returned.send((started.elapsed(), written(&snapshot))).ok();
        });
        finished
    }

    /// A writer thread that takes no line until `start_gate` is sent to or dropped.
    fn late_writer(
        writer: Box<dyn Write + Send>,
        start_gate: mpsc::Receiver<()>,
        trace: Trace,
    ) -> Notices {
        Notices::starting(Pending {
            open: Box::new(move || Ok(writer)),
            spawn: spawn_thread,
            start_gate: Some(start_gate),
            trace,
        })
    }

    #[cfg(debug_assertions)]
    fn failing_spawn(
        _: thread::Builder,
        _: Box<dyn FnOnce() + Send>,
    ) -> io::Result<thread::JoinHandle<()>> {
        Err(io::Error::other("injected spawn failure"))
    }

    /// A trace recorded on `timeline` in debug builds; release builds have none.
    #[cfg_attr(not(debug_assertions), allow(unused_variables))]
    fn test_trace(timeline: &Timeline) -> Trace {
        #[cfg(debug_assertions)]
        return Trace::to(open_writer(timeline));
        #[cfg(not(debug_assertions))]
        Trace::default()
    }

    /// The events of a trace recorded on `timeline`, without their times.
    #[cfg(debug_assertions)]
    fn events(timeline: &Timeline) -> Vec<String> {
        written(timeline)
            .iter()
            .map(|line| {
                line.split_once(" ms ")
                    .map_or(line.as_str(), |(_, event)| event)
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_failed_stderr_duplicate_drops_every_line_and_the_trace_says_so() {
        // A failed stderr duplicate loses the banner. Bug: the failure is silent, so a test log
        // cannot tell it from a writer the machine scheduled late.
        let traced = timeline();
        let mut notices = Notices::starting(Pending {
            open: Box::new(|| Err(io::Error::other("injected dup failure"))),
            spawn: spawn_thread,
            start_gate: None,
            trace: Trace::to(open_writer(&traced)),
        });
        notices.send("banner\n".to_owned());
        notices.send("fit line\n".to_owned());
        notices.finish(false);

        assert_eq!(
            events(&traced),
            [
                "stderr duplicate failed: injected dup failure",
                "line dropped: no writer",
                "line dropped: no writer",
            ]
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_writer_thread_that_fails_to_spawn_drops_every_line_and_the_trace_says_so() {
        // A failed thread spawn loses the banner. Bug: the failure is silent, so a test log
        // cannot tell it from a writer the machine scheduled late.
        let lines = timeline();
        let traced = timeline();
        let writer = open_writer(&lines);
        let mut notices = Notices::starting(Pending {
            open: Box::new(move || Ok(writer)),
            spawn: failing_spawn,
            start_gate: None,
            trace: Trace::to(open_writer(&traced)),
        });
        notices.send("banner\n".to_owned());
        notices.finish(true);

        assert!(written(&lines).is_empty());
        assert_eq!(
            events(&traced),
            [
                "writer thread spawn failed: injected spawn failure",
                "line dropped: no writer",
            ]
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_writer_that_starts_late_is_traced_from_the_queue_to_the_finish() {
        // A late start can lose the banner. Bug: the trace misses the late start or the reason
        // `finish` returned, so a test log cannot tell a late writer from a lost one.
        let lines = timeline();
        let traced = timeline();
        let (start, start_gate) = mpsc::channel::<()>();
        let mut notices = late_writer(
            open_writer(&lines),
            start_gate,
            Trace::to(open_writer(&traced)),
        );
        notices.send("banner\n".to_owned());
        notices.send("fit line\n".to_owned());

        let finished = finish_clean_exit(notices, &lines);
        thread::sleep(Duration::from_millis(400));
        drop(start);
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("finish(false) must return");

        assert_eq!(
            events(&traced),
            [
                "line queued",
                "line queued",
                "writer started",
                "line written",
                "line written",
                "writer ended",
                "finish(false) returned: the writer ended",
            ]
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_trace_says_finish_gave_up_on_a_blocked_write() {
        // Bug: the trace does not tell a write blocked on an unread pipe from a late writer.
        let lines = timeline();
        let traced = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::starting(Pending {
            open: Box::new(move || Ok(writer)),
            spawn: spawn_thread,
            start_gate: None,
            trace: Trace::to(open_writer(&traced)),
        });
        notices.send("stuck\n".to_owned());
        gate.wait_for_a_blocked_write();
        notices.finish(false);

        assert_eq!(
            events(&traced).last().map(String::as_str),
            Some("finish(false) returned: a write was blocked for the 250 ms drain bound")
        );
    }

    /// Panics inside its first write, as a writer thread that dies would.
    #[cfg(debug_assertions)]
    struct PanickingWriter;

    #[cfg(debug_assertions)]
    impl Write for PanickingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("injected writer panic");
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn finish_without_plain_lines_does_not_wait_on_a_writer_that_panicked() {
        // Bug: the writer's end is marked only on its normal path, so a writer that died inside
        // a write looks blocked and holds up a clean exit for the drain bound.
        let traced = timeline();
        let mut notices = Notices::starting(Pending {
            open: Box::new(|| Ok(Box::new(PanickingWriter) as Box<dyn Write + Send>)),
            spawn: spawn_thread,
            start_gate: None,
            trace: Trace::to(open_writer(&traced)),
        });
        notices.send("banner\n".to_owned());
        notices.finish(false);

        assert_eq!(
            events(&traced).last().map(String::as_str),
            Some("finish(false) returned: the writer ended")
        );
    }

    #[test]
    fn finish_without_plain_lines_delivers_the_lines_of_a_writer_that_starts_late() {
        // Bug: `finish(false)` gives up after the drain bound before the writer has begun a single
        // write, so a writer the machine schedules late loses the launch banner and the fit line.
        // Or it is not woken when the writer ends, so every clean exit waits out the 2 s cap.
        let lines = timeline();
        let (start, start_gate) = mpsc::channel::<()>();
        let mut notices = late_writer(open_writer(&lines), start_gate, Trace::default());
        notices.send("banner\n".to_owned());
        notices.send("fit line\n".to_owned());

        let finished = finish_clean_exit(notices, &lines);
        // Longer than the 250 ms drain bound, so a wait that only counts time has given up.
        thread::sleep(Duration::from_millis(400));
        drop(start);

        let (waited, seen_on_return) = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("finish(false) must return");
        assert_eq!(seen_on_return, ["banner\n", "fit line\n"]);
        // The writer ends at about 400 ms; a wait that is never woken returns at the 2 s cap.
        assert!(
            waited < Duration::from_millis(1500),
            "it was not woken when the writer ended: {waited:?}"
        );
    }

    #[test]
    fn finish_without_plain_lines_gives_up_at_the_cap_on_a_writer_that_never_starts() {
        // Bug: no overall cap, so a writer that is never scheduled holds up a clean exit for good;
        // or a cap no longer than the drain bound, so a late writer still loses its lines.
        let lines = timeline();
        let (start, start_gate) = mpsc::channel::<()>();
        let traced = timeline();
        let mut notices = late_writer(open_writer(&lines), start_gate, test_trace(&traced));
        notices.send("banner\n".to_owned());

        let (waited, seen_on_return) = finish_clean_exit(notices, &lines)
            .recv_timeout(Duration::from_secs(10))
            .expect("finish(false) must return while the writer never starts");
        drop(start);

        assert!(
            waited >= Duration::from_secs(2),
            "it gave a writer that had not started only {waited:?}"
        );
        assert!(waited < Duration::from_secs(3), "it waited {waited:?}");
        assert!(seen_on_return.is_empty());
        #[cfg(debug_assertions)]
        assert_eq!(
            events(&traced).last().map(String::as_str),
            Some("finish(false) returned: the 2 s cap was reached")
        );
    }

    #[test]
    fn finish_without_plain_lines_gives_up_at_the_cap_on_a_writer_that_keeps_writing() {
        // Bug: the cap restarts whenever the writer makes progress, so a late writer that then
        // writes eight slow lines holds up a clean exit until the last one (1.5 s + 8 x 100 ms),
        // or there is no cap at all. Either way all eight lines are out when it returns. At the
        // 2 s cap about four are out: line 1 lands 400 ms before it and line 8 300 ms after.
        // Each pause stays well under the 250 ms drain bound, or the first write would end the
        // wait.
        let lines = timeline();
        let (start, start_gate) = mpsc::channel::<()>();
        let writer = SlowWriter {
            pause: Duration::from_millis(100),
            timeline: Arc::clone(&lines),
        };
        let mut notices = late_writer(Box::new(writer), start_gate, Trace::default());
        for number in 1..=8 {
            notices.send(format!("line {number}\n"));
        }

        let finished = finish_clean_exit(notices, &lines);
        thread::sleep(Duration::from_millis(1500));
        drop(start);

        let (waited, seen_on_return) = finished
            .recv_timeout(Duration::from_secs(10))
            .expect("finish(false) must return while the writer keeps writing");
        assert!(waited < Duration::from_millis(3500), "it waited {waited:?}");
        assert!(
            !seen_on_return.is_empty() && seen_on_return.len() < 8,
            "it must return while the writer is part way through: {seen_on_return:?}"
        );
    }

    #[test]
    fn dropping_an_unfinished_notices_gives_up_on_a_stuck_write_before_the_cap() {
        // Bug: `Drop` joins without a bound, so a panic on the way out hangs on an unread pipe;
        // or it waits out the 2 s cap on a write the drain bound has already given up on.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("stuck\n".to_owned());
        gate.wait_for_a_blocked_write();

        let waited = drop_on_a_thread(notices)
            .recv_timeout(Duration::from_secs(5))
            .expect("dropping must return while the writer is stuck");
        assert!(waited < Duration::from_secs(2), "it waited {waited:?}");
    }

    #[test]
    fn dropping_notices_whose_writer_never_starts_gives_up_at_the_cap() {
        // Bug: `Drop` does not wait for a writer that has not started, so a panic on the way out
        // loses the lines a late writer would still deliver; or it waits for it without a cap.
        let lines = timeline();
        let (start, start_gate) = mpsc::channel::<()>();
        let mut notices = late_writer(open_writer(&lines), start_gate, Trace::default());
        notices.send("banner\n".to_owned());

        let waited = drop_on_a_thread(notices)
            .recv_timeout(Duration::from_secs(10))
            .expect("dropping must return while the writer never starts");
        drop(start);

        assert!(
            waited >= Duration::from_secs(2),
            "it gave a writer that had not started only {waited:?}"
        );
        assert!(waited < Duration::from_secs(3), "it waited {waited:?}");
    }

    /// Drop `notices` on its own thread; the result is how long the drop took.
    fn drop_on_a_thread(notices: Notices) -> mpsc::Receiver<Duration> {
        let (returned, dropped) = mpsc::channel();
        thread::spawn(move || {
            let started = Instant::now();
            drop(notices);
            returned.send(started.elapsed()).ok();
        });
        dropped
    }

    #[cfg(all(debug_assertions, target_os = "macos"))]
    mod trace_file {
        use std::fs;
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        use super::super::{O_NOFOLLOW, open_trace_file};

        #[test]
        fn the_no_follow_flag_is_the_one_macos_defines() {
            // Bug: a wrong literal, which may be another flag (0x0200 is `O_CREAT`) and follows
            // symlinks again.
            assert_eq!(O_NOFOLLOW, libc::O_NOFOLLOW);
        }

        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

        /// A fresh private directory, removed with everything in it when dropped.
        struct Scratch(PathBuf);

        impl Scratch {
            fn new() -> Self {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "mlx-guard-notices-{}-{nanos}-{sequence}",
                    std::process::id()
                ));
                fs::create_dir(&path).unwrap();
                Self(path)
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).ok();
            }
        }

        #[test]
        fn a_new_trace_file_is_readable_by_its_owner_only() {
            // Bug: the file is created with the default mode, 0o666 less the umask, so other
            // users on the Mac can read the trace.
            let scratch = Scratch::new();
            let path = scratch.0.join("trace.log");

            open_trace_file(&path).expect("a new trace file must open");

            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode {mode:o}");
        }

        #[test]
        fn a_trace_path_that_is_a_symlink_is_not_followed() {
            // Bug: the open follows a symlink, so the variable can make the supervisor create or
            // append to a file other than the one it names.
            let scratch = Scratch::new();
            let target = scratch.0.join("elsewhere.log");
            let link = scratch.0.join("trace.log");
            symlink(&target, &link).unwrap();

            assert!(
                open_trace_file(&link).is_err(),
                "a symlinked trace path must not open"
            );
            assert!(!target.exists(), "the symlink's target was created");
        }

        #[test]
        fn an_existing_trace_file_is_appended_to() {
            // Bug: the open truncates, or refuses a file that exists, so a second run loses the
            // trace of the first or gets none.
            let scratch = Scratch::new();
            let path = scratch.0.join("trace.log");
            fs::write(&path, "first run\n").unwrap();

            let mut file = open_trace_file(&path).expect("an existing trace file must open");
            std::io::Write::write_all(&mut file, b"second run\n").unwrap();

            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "first run\nsecond run\n"
            );
        }
    }
}
