//! When a real sample can measure nobody.
//!
//! The runtime skips a complete sample whose member set is empty: it treats it as the window
//! after every process in the group has exited and before the root is reaped. That is safe only
//! if such a sample never happens while a group member is alive. This test holds a real group in
//! each phase of an exit, and keeps the exited root unreaped on purpose, which is the state the
//! supervisor sees for a few milliseconds at the end of a run. A process that left the group
//! (`setsid`, `setpgid`) is not a member, and this test says nothing about it.
#![cfg(target_os = "macos")]
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    FootprintSample, FootprintSampler, IdentityTracker, LaunchOptions, NativeProcessInventory,
    OwnedProcess, RootOutcome, SampleOutcome, SamplingConfig, StdioMode,
};

/// The root forks a child, names it, and exits when told to. A background job of a
/// non-interactive `sh` stays in the root's process group, the group the test owns, so the child
/// is a group member. It outlives the root, reparented to launchd, until the test kills it.
/// 30 s is well past a passing run's longest path to phase 3 (two phase deadlines and the root's
/// exit deadline, 11 s). A failing run does not wait for it: dropping the owned process kills
/// the whole group, this child included.
const SCRIPT: &str = "/bin/sleep 30 & echo \"CHILD $!\"; read line; exit 0";
/// Every phase must be seen in this many checked samples, or the test proves nothing about it.
const MIN_SAMPLES_PER_PHASE: usize = 5;
/// How long a phase may take to collect its samples. A phase counts samples, not time, so a slow
/// runner only takes longer. The cold hosted macos-15 runner took 4 samples in 150 ms, about
/// 40 ms each; 3 s leaves room for samples fifteen times slower than that.
const PHASE_DEADLINE: Duration = Duration::from_secs(3);
const DEADLINE: Duration = Duration::from_secs(5);

fn sh_root() -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from(SCRIPT),
        ],
        cwd: None,
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Piped,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Null,
    }
}

fn sampler_for(process: &OwnedProcess, inventory: NativeProcessInventory) -> FootprintSampler {
    let root = inventory
        .inspect(process.root_pid().cast_signed())
        .unwrap()
        .identity;
    let tracker = IdentityTracker::new(root, process.process_group_id()).unwrap();
    let config = SamplingConfig::new(
        Duration::from_millis(10),
        Duration::from_millis(100),
        Duration::from_millis(10),
        64,
    )
    .unwrap();
    FootprintSampler::new(config, tracker)
}

/// Whether `pid`, a child of this test process, has exited, without reaping it.
fn has_exited_unreaped(pid: i32) -> bool {
    // SAFETY: a zeroed siginfo_t is a valid out-buffer; WNOWAIT leaves the child waitable.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid.cast_unsigned(),
            &raw mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    assert_eq!(result, 0, "waitid: {}", std::io::Error::last_os_error());
    info.si_pid == pid
}

/// Whether any process, zombie or not, still holds `pid`.
fn pid_exists(pid: i32) -> bool {
    // SAFETY: signal zero sends nothing; it only checks that the pid is in use.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// A sample that measured somebody: a non-empty member set, and a complete sum above zero.
fn assert_measured(sample: &FootprintSample, phase: &str) {
    assert!(
        !sample.members.is_empty(),
        "{phase}: a sample measured nobody while a group member was alive: {sample:?}"
    );
    match sample.outcome {
        SampleOutcome::Complete { total_bytes } => assert!(
            total_bytes > 0,
            "{phase}: a complete sample summed to 0 B while a group member was alive: {sample:?}"
        ),
        ref outcome => panic!("{phase}: expected a complete sample, got {outcome:?}"),
    }
}

/// Fails the phase once it has run for [`PHASE_DEADLINE`] without enough checked samples.
#[track_caller]
fn assert_phase_in_time(started: Instant, seen: usize, phase: &str) {
    let elapsed = started.elapsed();
    assert!(
        elapsed < PHASE_DEADLINE,
        "{phase}: only {seen} of {MIN_SAMPLES_PER_PHASE} checked samples after {elapsed:?}"
    );
}

#[test]
fn an_empty_member_set_appears_only_after_every_group_member_is_gone() {
    // Bug: the inventory or the tracker drops a live group member once the root has exited
    // (for example, members kept only while they descend from the root, which a child
    // reparented to launchd at the root's exit no longer does). The runtime then skips samples
    // as "nobody left" while that member runs, and its footprint goes unwatched.
    let inventory = NativeProcessInventory::new();
    let mut process = OwnedProcess::launch(&sh_root()).unwrap();
    let mut stdin = process.take_stdin().unwrap();
    let mut stdout = BufReader::new(process.take_stdout().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let child: i32 = line
        .trim()
        .strip_prefix("CHILD ")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("the root must name its child: {line:?}"));
    let root = process.root_pid().cast_signed();
    // Precondition: the child is a member through the owned group, not through a parent link.
    // SAFETY: getpgid only reads the process group of a live pid.
    assert_eq!(
        unsafe { libc::getpgid(child) },
        process.process_group_id(),
        "the child must stay in the owned process group"
    );
    let mut sampler = sampler_for(&process, inventory);
    let epoch = Instant::now();

    // Phase 1: root and child alive. A sample counts only if the root was alive on both sides.
    let started = Instant::now();
    let mut seen = 0;
    while seen < MIN_SAMPLES_PER_PHASE {
        assert_phase_in_time(started, seen, "root alive");
        let alive_before = !has_exited_unreaped(root);
        let sample = sampler.sample_native(&inventory, epoch).clone();
        if alive_before && !has_exited_unreaped(root) {
            assert_measured(&sample, "root alive");
            seen += 1;
        }
        thread::sleep(Duration::from_millis(5));
    }

    // Phase 2: the root has exited and is held unreaped; the child is alive in the group.
    stdin.write_all(b"go\n").unwrap();
    drop(stdin);
    let deadline = Instant::now() + DEADLINE;
    while !has_exited_unreaped(root) {
        assert!(Instant::now() < deadline, "the root did not exit");
        thread::sleep(Duration::from_millis(1));
    }
    let started = Instant::now();
    let mut seen = 0;
    while seen < MIN_SAMPLES_PER_PHASE {
        assert_phase_in_time(started, seen, "root exited, child alive");
        let sample = sampler.sample_native(&inventory, epoch).clone();
        assert!(pid_exists(child), "the child must outlive this phase");
        assert_measured(&sample, "root exited, child alive");
        assert!(
            sample
                .members
                .iter()
                .any(|member| member.identity.pid == child),
            "the live child must be a member: {sample:?}"
        );
        seen += 1;
        thread::sleep(Duration::from_millis(5));
    }

    // Phase 3: the child is gone (launchd reaped it); only the unreaped root is left.
    // SAFETY: `child` was named by the root of this test's own group and is still alive.
    assert_eq!(unsafe { libc::kill(child, libc::SIGKILL) }, 0);
    let deadline = Instant::now() + DEADLINE;
    while pid_exists(child) {
        assert!(Instant::now() < deadline, "the child was not reaped");
        thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..MIN_SAMPLES_PER_PHASE {
        let sample = sampler.sample_native(&inventory, epoch).clone();
        assert!(sample.members.is_empty(), "nobody is left: {sample:?}");
        assert_eq!(
            sample.outcome,
            SampleOutcome::Complete { total_bytes: 0 },
            "the sum over nobody, with the root's exit seen"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(pid_exists(root), "the root must still be unreaped");
    assert_eq!(process.wait_root().unwrap(), RootOutcome::Exited(0));
}
