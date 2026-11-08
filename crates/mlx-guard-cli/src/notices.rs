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

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const NOTICE_QUEUE: usize = 8;
const NOTICE_DRAIN_WAIT: Duration = Duration::from_millis(250);
const WRITER_STACK_BYTES: usize = 64 * 1024;

/// The stderr lines of one supervised command; a new value holds no thread and no descriptor.
#[derive(Default)]
pub struct Notices {
    state: State,
}

enum State {
    /// Nothing sent yet. The writer is only ever present in tests; otherwise it is fd 2's duplicate.
    Idle(Option<Box<dyn Write + Send>>),
    Running(WriterThread),
    Closed,
}

impl Default for State {
    fn default() -> Self {
        Self::Idle(None)
    }
}

struct WriterThread {
    queue: SyncSender<String>,
    /// Disconnects when the thread has written or dropped everything in the queue.
    finished: Receiver<()>,
    thread: JoinHandle<()>,
}

impl WriterThread {
    fn start(mut writer: Box<dyn Write + Send>) -> Option<Self> {
        let (queue, lines) = mpsc::sync_channel::<String>(NOTICE_QUEUE);
        let (finished_sender, finished) = mpsc::channel::<()>();
        let thread = thread::Builder::new()
            .name("mlx-guard-notices".to_owned())
            .stack_size(WRITER_STACK_BYTES)
            .spawn(move || {
                for line in lines {
                    writer.write_all(line.as_bytes()).ok();
                }
                drop(finished_sender);
            })
            .ok()?;
        Some(Self {
            queue,
            finished,
            thread,
        })
    }
}

fn duplicate_stderr() -> Option<Box<dyn Write + Send>> {
    let descriptor = std::io::stderr().as_fd().try_clone_to_owned().ok()?;
    Some(Box::new(File::from(descriptor)))
}

impl Notices {
    #[cfg(test)]
    pub(crate) fn with_writer(writer: Box<dyn Write + Send>) -> Self {
        Self {
            state: State::Idle(Some(writer)),
        }
    }

    /// Queue one newline-ended line. Never blocks and never panics; a failure to start the writer
    /// disables the lines for good.
    pub(crate) fn send(&mut self, line: String) {
        if let State::Idle(writer) = &mut self.state {
            self.state = writer
                .take()
                .or_else(duplicate_stderr)
                .and_then(WriterThread::start)
                .map_or(State::Closed, State::Running);
        }
        if let State::Running(writer) = &self.state {
            writer.queue.try_send(line).ok();
        }
    }

    /// Close the queue once the report is final. With plain lines to follow, wait as long as the
    /// writer needs, so a blocked line cannot land inside them. Otherwise wait at most the drain
    /// bound, because a blocked thread dies with the process and no reader was promised anything.
    pub(crate) fn finish(&mut self, plain_lines_follow: bool) {
        let State::Running(writer) = std::mem::replace(&mut self.state, State::Closed) else {
            return;
        };
        let WriterThread {
            queue,
            finished,
            thread,
        } = writer;
        drop(queue);
        if plain_lines_follow {
            thread.join().ok();
        } else {
            finished.recv_timeout(NOTICE_DRAIN_WAIT).ok();
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
    use super::{Notices, State};

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

    #[test]
    fn dropping_an_unfinished_notices_waits_no_longer_than_the_drain_bound() {
        // Bug: `Drop` joins without a bound, so a panic on the way out hangs on an unread pipe.
        let lines = timeline();
        let (writer, gate) = stuck_writer(&lines);
        let mut notices = Notices::with_writer(writer);
        notices.send("stuck\n".to_owned());
        gate.wait_for_a_blocked_write();

        let (returned, dropped) = mpsc::channel();
        thread::spawn(move || {
            drop(notices);
            returned.send(()).ok();
        });

        dropped
            .recv_timeout(Duration::from_secs(5))
            .expect("dropping must return while the writer is stuck");
    }
}
