#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::thread;
use std::time::{Duration, Instant};

use mlx_guard_core::{
    AggregateFootprint, ContainmentEvent, IdentityTracker, IdentityUnavailable, LaunchOptions,
    NativeProcessInventory, ObservationFailure, ObservationFailureKind, OwnedProcess,
    ProcessIdentity, ProcessObservation, ProcessSnapshot, SignalNumber, StdioMode,
    wait_for_owned_group_empty,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_mlx-guard-fixture");

fn fixture(mode: &str, wall_ms: u64) -> LaunchOptions {
    LaunchOptions {
        command: vec![
            OsString::from(FIXTURE),
            OsString::from(mode),
            OsString::from("1"),
            OsString::from(wall_ms.to_string()),
        ],
        cwd: None,
        clear_env: false,
        env: BTreeMap::new(),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

fn observation(
    pid: i32,
    start: u64,
    parent_pid: i32,
    process_group_id: i32,
    footprint_bytes: Option<u64>,
) -> ProcessObservation {
    ProcessObservation {
        identity: ProcessIdentity {
            pid,
            start_abstime: start,
        },
        parent_pid,
        process_group_id,
        footprint_bytes,
        exited: false,
    }
}

fn snapshot(observations: Vec<ProcessObservation>) -> ProcessSnapshot {
    ProcessSnapshot {
        observations,
        failures: Vec::new(),
    }
}

fn parse_field(line: &str, name: &str) -> i32 {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{name}=")))
        .unwrap()
        .parse()
        .unwrap()
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero only checks the fixture identity for bounded cleanup assertions.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn wait_until_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!process_exists(pid));
}

#[test]
fn tracker_revalidates_identity_and_types_reparent_escape_and_missing_data() {
    // Catches cached-PID trust, lost escape evidence, or summing missing footprint as zero.
    let root = ProcessIdentity {
        pid: 100,
        start_abstime: 1,
    };
    let child = ProcessIdentity {
        pid: 101,
        start_abstime: 2,
    };
    let mut tracker = IdentityTracker::new(root, 100).unwrap();
    let missing_root = tracker.update(snapshot(Vec::new()));
    assert_eq!(
        missing_root.aggregate_footprint,
        AggregateFootprint::Incomplete {
            known_bytes: 0,
            missing_identities: vec![root],
        }
    );
    let first = tracker.update(snapshot(vec![
        observation(100, 1, 1, 100, Some(10)),
        observation(101, 2, 100, 100, Some(20)),
        observation(900, 10, 1, 900, Some(999)),
    ]));
    assert_eq!(first.aggregate_footprint, AggregateFootprint::Complete(30));
    assert_eq!(first.owned_members.len(), 2);

    let second = tracker.update(snapshot(vec![
        observation(100, 1, 1, 100, Some(10)),
        observation(101, 2, 1, 100, None),
        observation(102, 3, 100, 102, Some(7)),
        observation(103, 4, 100, 100, Some(5)),
        observation(900, 11, 1, 900, Some(999)),
    ]));
    assert!(second.events.contains(&ContainmentEvent::Reparented {
        identity: child,
        previous_parent_pid: Some(100),
        observed_parent_pid: 1,
    }));
    assert!(second.events.iter().any(|event| matches!(
        event,
        ContainmentEvent::LeftOwnedGroup { identity, observed_group: 102 }
            if identity.pid == 102
    )));
    assert_eq!(
        second.aggregate_footprint,
        AggregateFootprint::Incomplete {
            known_bytes: 15,
            missing_identities: vec![child],
        }
    );
    assert!(
        !second
            .events
            .iter()
            .any(|event| matches!(event, ContainmentEvent::IdentityChanged { pid: 900, .. }))
    );

    let third = tracker.update(ProcessSnapshot {
        observations: vec![
            observation(100, 1, 1, 100, Some(10)),
            observation(101, 99, 1, 999, Some(u64::MAX)),
        ],
        failures: vec![ObservationFailure {
            pid: Some(103),
            kind: ObservationFailureKind::PermissionDenied,
        }],
    });
    assert!(third.events.contains(&ContainmentEvent::IdentityChanged {
        pid: 101,
        previous_start_abstime: 2,
        observed_start_abstime: 99,
    }));
    assert!(
        !third
            .owned_members
            .iter()
            .any(|item| item.identity.pid == 101)
    );
    assert!(third.events.iter().any(|event| matches!(
        event,
        ContainmentEvent::Disappeared(identity) if identity.pid == 103
    )));
}

#[test]
fn native_inventory_binds_start_time_and_refuses_a_stale_direct_signal() {
    // Catches treating a live PID alone as identity before direct action.
    let inventory = NativeProcessInventory::new();
    let current = inventory.inspect(std::process::id().cast_signed()).unwrap();
    assert!(current.identity.start_abstime > 0);
    let stale = ProcessIdentity {
        start_abstime: current.identity.start_abstime.saturating_add(1),
        ..current.identity
    };
    let term = SignalNumber::new(u8::try_from(libc::SIGCONT).unwrap()).unwrap();
    assert_eq!(
        inventory.signal_identity(stale, term).unwrap_err(),
        IdentityUnavailable::Stale
    );
    assert!(process_exists(current.identity.pid));
}

#[test]
fn native_footprint_capability_is_probed_instead_of_assumed() {
    // Catches treating the Linux topology adapter as proof of Darwin footprint support.
    let inventory = NativeProcessInventory::new();
    #[cfg(target_os = "macos")]
    inventory.probe_footprint().unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(
        inventory.probe_footprint().unwrap_err(),
        IdentityUnavailable::Unsupported
    );
}

#[test]
fn real_churn_double_fork_and_root_zombie_keep_bound_identities() {
    // Catches synthetic topology tests, lost reparented members, or reaping the root before evidence.
    let inventory = NativeProcessInventory::new();

    let mut churn = OwnedProcess::launch(&fixture("spawn-churn", 2_000)).unwrap();
    let mut output = BufReader::new(churn.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "READY mode=spawn-churn\n");
    let root = inventory
        .inspect(churn.root_pid().cast_signed())
        .unwrap()
        .identity;
    let mut tracker = IdentityTracker::new(root, churn.process_group_id()).unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut saw_child = false;
    while Instant::now() < deadline && churn.try_wait_root().unwrap().is_none() {
        let frame = tracker.update(inventory.snapshot().unwrap());
        saw_child |= frame
            .owned_members
            .iter()
            .any(|member| member.identity != root);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(saw_child);
    drop(churn);

    let mut tree = OwnedProcess::launch(&fixture("double-fork", 2_000)).unwrap();
    let mut output = BufReader::new(tree.take_stdout().unwrap());
    line.clear();
    output.read_line(&mut line).unwrap();
    let grandchild_pid = parse_field(&line, "pid");
    let intermediate_pid = parse_field(&line, "intermediate_pid");
    let root = inventory
        .inspect(tree.root_pid().cast_signed())
        .unwrap()
        .identity;
    let mut tracker = IdentityTracker::new(root, tree.process_group_id()).unwrap();
    let frame = tracker.update(inventory.snapshot().unwrap());
    assert!(
        frame
            .owned_members
            .iter()
            .any(|member| member.identity.pid == grandchild_pid)
    );
    assert!(frame.events.iter().any(|event| matches!(
        event,
        ContainmentEvent::Reparented { identity, previous_parent_pid: None, .. }
            if identity.pid == grandchild_pid
    )));
    assert_ne!(
        inventory.inspect(grandchild_pid).unwrap().parent_pid,
        intermediate_pid
    );
    drop(tree);
    wait_until_gone(grandchild_pid);

    let fast = OwnedProcess::launch(&fixture("fast-root-exit", 2_000)).unwrap();
    let root = inventory
        .inspect(fast.root_pid().cast_signed())
        .unwrap()
        .identity;
    let deadline = Instant::now() + Duration::from_secs(1);
    let exited = loop {
        let observed = inventory.inspect_expected(root).unwrap();
        if observed.exited || Instant::now() >= deadline {
            break observed;
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert!(exited.exited);
    assert_eq!(exited.identity, root);
    drop(fast);
}

#[test]
fn setsid_escape_and_cleanup_survivors_are_reported_without_unrelated_signals() {
    // Catches claiming group cleanup removed an escape or declaring TERM-resistant work complete.
    let inventory = NativeProcessInventory::new();
    let mut escaped = OwnedProcess::launch(&fixture("setsid-parent", 2_000)).unwrap();
    let mut output = BufReader::new(escaped.take_stdout().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let escaped_pid = parse_field(&line, "pid");
    let root = inventory
        .inspect(escaped.root_pid().cast_signed())
        .unwrap()
        .identity;
    let escaped_identity = inventory.inspect(escaped_pid).unwrap().identity;
    let mut tracker = IdentityTracker::new(root, escaped.process_group_id()).unwrap();
    let frame = tracker.update(inventory.snapshot().unwrap());
    assert!(frame.events.iter().any(|event| matches!(
        event,
        ContainmentEvent::LeftOwnedGroup { identity, .. } if *identity == escaped_identity
    )));
    drop(escaped);
    assert!(process_exists(escaped_pid));
    let report = wait_for_owned_group_empty(
        &inventory,
        &mut tracker,
        Duration::from_millis(50),
        Duration::from_millis(5),
    );
    assert!(report.owned_group_empty);
    assert!(!report.complete);
    assert!(report.escaped_identities.contains(&escaped_identity));
    let kill = SignalNumber::new(u8::try_from(libc::SIGKILL).unwrap()).unwrap();
    inventory.signal_identity(escaped_identity, kill).unwrap();
    wait_until_gone(escaped_pid);

    let mut resistant = OwnedProcess::launch(&fixture("ignore-term", 2_000)).unwrap();
    let mut output = BufReader::new(resistant.take_stdout().unwrap());
    line.clear();
    output.read_line(&mut line).unwrap();
    assert!(line.starts_with("READY mode=ignore-term pid="));
    let root = inventory
        .inspect(resistant.root_pid().cast_signed())
        .unwrap()
        .identity;
    let mut tracker = IdentityTracker::new(root, resistant.process_group_id()).unwrap();
    resistant.terminate_group().unwrap();
    let report = wait_for_owned_group_empty(
        &inventory,
        &mut tracker,
        Duration::from_millis(50),
        Duration::from_millis(5),
    );
    assert!(!report.owned_group_empty);
    assert!(report.survivors.contains(&root));
    resistant.kill_group().unwrap();
    resistant.wait_root().unwrap();
    let report = wait_for_owned_group_empty(
        &inventory,
        &mut tracker,
        Duration::from_millis(500),
        Duration::from_millis(5),
    );
    assert!(report.owned_group_empty);
}
