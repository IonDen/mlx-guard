#![allow(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use crate::{SignalNumber, SignalResult};

/// A PID bound to the kernel process-start token observed for that PID.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProcessIdentity {
    pub pid: i32,
    pub start_abstime: u64,
}

/// One identity-validated process observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessObservation {
    pub identity: ProcessIdentity,
    pub parent_pid: i32,
    pub process_group_id: i32,
    pub footprint_bytes: Option<u64>,
    pub exited: bool,
}

/// Stable reason one process could not be observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationFailureKind {
    Disappeared,
    PermissionDenied,
    StaleIdentity,
    MalformedData,
    Unsupported,
    EnumerationFailed,
    Unavailable,
}

/// A per-PID or whole-enumeration observation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationFailure {
    pub pid: Option<i32>,
    pub kind: ObservationFailureKind,
}

/// One non-atomic native enumeration result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub observations: Vec<ProcessObservation>,
    pub failures: Vec<ObservationFailure>,
}

/// Failure returned by direct identity inspection or validated signalling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityUnavailable {
    Disappeared,
    PermissionDenied,
    Stale,
    MalformedData,
    Unsupported,
    Unavailable,
}

impl fmt::Display for IdentityUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Disappeared => "process disappeared",
            Self::PermissionDenied => "process observation was denied",
            Self::Stale => "process identity changed",
            Self::MalformedData => "process metadata was malformed",
            Self::Unsupported => "process inventory is unsupported",
            Self::Unavailable => "process inventory is unavailable",
        })
    }
}

impl Error for IdentityUnavailable {}

/// Whole-enumeration failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotError {
    pub kind: ObservationFailureKind,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("process enumeration failed")
    }
}

impl Error for SnapshotError {}

/// Aggregate result that cannot conceal missing members or arithmetic overflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AggregateFootprint {
    Complete(u64),
    Incomplete {
        known_bytes: u64,
        missing_identities: Vec<ProcessIdentity>,
    },
    Overflow,
}

/// Evidence produced while comparing two non-atomic process snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainmentEvent {
    Disappeared(ProcessIdentity),
    IdentityChanged {
        pid: i32,
        previous_start_abstime: u64,
        observed_start_abstime: u64,
    },
    Reparented {
        identity: ProcessIdentity,
        previous_parent_pid: Option<i32>,
        observed_parent_pid: i32,
    },
    LeftOwnedGroup {
        identity: ProcessIdentity,
        observed_group: i32,
    },
    ObservationFailed(ObservationFailure),
}

/// One derived frame for policy sampling and final evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackingFrame {
    pub owned_members: Vec<ProcessObservation>,
    pub escape_observed: bool,
    pub events: Vec<ContainmentEvent>,
    pub aggregate_footprint: AggregateFootprint,
    pub observation_failures: Vec<ObservationFailure>,
    pub root_exit_observed: bool,
}

#[derive(Clone, Copy, Debug)]
struct TrackedProcess {
    identity: ProcessIdentity,
    parent_pid: i32,
    /// Sticky per-identity mark that an escape was already counted for this identity.
    ///
    /// The mark rides on the per-sample tracked set, so it stays bounded by the live members
    /// while remaining independent of the capped `escaped` evidence list. Consequence: an
    /// identity a sample fails to observe loses its mark, so past the evidence cap, where the
    /// retained set no longer remembers it either, a later re-observation counts it again.
    /// Observation gaps are typed outcomes here, so that drift is real rather than theoretical.
    /// The alternative is unbounded per-identity memory, which the supervisor does not keep.
    escaped: bool,
}

const MAX_ESCAPED_EVIDENCE: usize = 64;

/// Stateful comparison of identity-bound process snapshots.
#[derive(Debug)]
pub struct IdentityTracker {
    root: ProcessIdentity,
    owned_group: i32,
    tracked: BTreeMap<ProcessIdentity, TrackedProcess>,
    last_by_pid: BTreeMap<i32, ProcessIdentity>,
    escaped: BTreeSet<ProcessIdentity>,
    escaped_count: u64,
    root_exit_observed: bool,
}

impl IdentityTracker {
    /// Create a tracker for one positive root identity and matching positive PGID.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityUnavailable::MalformedData`] for zero or negative identity fields.
    pub fn new(root: ProcessIdentity, owned_group: i32) -> Result<Self, IdentityUnavailable> {
        if root.pid <= 0 || root.start_abstime == 0 || owned_group <= 0 {
            return Err(IdentityUnavailable::MalformedData);
        }
        Ok(Self {
            root,
            owned_group,
            tracked: BTreeMap::new(),
            last_by_pid: BTreeMap::new(),
            escaped: BTreeSet::new(),
            escaped_count: 0,
            root_exit_observed: false,
        })
    }

    /// Count of identities observed outside the owned group.
    ///
    /// At least one increment per distinct escaped identity, and not bounded by the retained
    /// escape evidence: identities past the evidence cap are still counted. Above that cap the
    /// counted mark lives only on the tracked set, so an identity a sample misses and later
    /// re-observes can add a further increment. Read this as bounded evidence of distinct
    /// escapes, not an exact census.
    #[must_use]
    pub const fn escaped_count(&self) -> u64 {
        self.escaped_count
    }

    /// Revalidate a non-atomic snapshot and derive only currently bound live members.
    #[must_use]
    pub fn update(&mut self, snapshot: ProcessSnapshot) -> TrackingFrame {
        let (observations, mut events, relevant_failures) = self.prepare_snapshot(snapshot);

        let previous = self.tracked.clone();
        let mut next_tracked = BTreeMap::new();
        let mut owned_members = Vec::new();

        for observation in observations.values().copied() {
            if observation.identity.pid <= 0 || observation.identity.start_abstime == 0 {
                continue;
            }
            let was_tracked = previous.get(&observation.identity).copied();
            let descendant = descends_from_root(observation, &observations, self.root.pid);
            let in_owned_group = observation.process_group_id == self.owned_group;
            let is_root = observation.identity == self.root;
            if !is_root && was_tracked.is_none() && !descendant && !in_owned_group {
                continue;
            }

            if let Some(tracked) = was_tracked {
                if tracked.parent_pid != observation.parent_pid {
                    events.push(ContainmentEvent::Reparented {
                        identity: observation.identity,
                        previous_parent_pid: Some(tracked.parent_pid),
                        observed_parent_pid: observation.parent_pid,
                    });
                }
            } else if !is_root && in_owned_group && !descendant {
                events.push(ContainmentEvent::Reparented {
                    identity: observation.identity,
                    previous_parent_pid: None,
                    observed_parent_pid: observation.parent_pid,
                });
            }

            // An exited process reports group zero, so it must never read as an escape.
            let escaping = !is_root && !in_owned_group && !observation.exited;
            let counted_before = was_tracked.is_some_and(|tracked| tracked.escaped)
                || self.escaped.contains(&observation.identity);
            next_tracked.insert(
                observation.identity,
                TrackedProcess {
                    identity: observation.identity,
                    parent_pid: observation.parent_pid,
                    escaped: counted_before || escaping,
                },
            );
            if escaping {
                if !counted_before {
                    self.escaped_count = self.escaped_count.saturating_add(1);
                }
                if self.escaped.len() < MAX_ESCAPED_EVIDENCE
                    && self.escaped.insert(observation.identity)
                {
                    events.push(ContainmentEvent::LeftOwnedGroup {
                        identity: observation.identity,
                        observed_group: observation.process_group_id,
                    });
                }
            } else if !observation.exited {
                owned_members.push(observation);
            }
        }

        owned_members.sort_by_key(|item| item.identity);
        self.root_exit_observed |= observations
            .get(&self.root.pid)
            .is_some_and(|observation| observation.identity == self.root && observation.exited);
        let root_was_observed = observations
            .get(&self.root.pid)
            .is_some_and(|observation| observation.identity == self.root);
        let aggregate_footprint = aggregate(
            &owned_members,
            &relevant_failures,
            &previous,
            self.root,
            root_was_observed || self.root_exit_observed,
        );
        self.tracked = next_tracked;
        self.last_by_pid = self
            .tracked
            .keys()
            .map(|identity| (identity.pid, *identity))
            .collect();
        self.last_by_pid.entry(self.root.pid).or_insert(self.root);

        TrackingFrame {
            root_exit_observed: self.root_exit_observed,
            owned_members,
            escape_observed: self.escaped_count > 0,
            events,
            aggregate_footprint,
            observation_failures: relevant_failures,
        }
    }

    fn prepare_snapshot(
        &self,
        snapshot: ProcessSnapshot,
    ) -> (
        BTreeMap<i32, ProcessObservation>,
        Vec<ContainmentEvent>,
        Vec<ObservationFailure>,
    ) {
        let mut observations = BTreeMap::new();
        for observation in snapshot.observations {
            observations
                .entry(observation.identity.pid)
                .or_insert(observation);
        }
        let mut events = Vec::new();
        for observation in observations.values() {
            if let Some(previous) = self.last_by_pid.get(&observation.identity.pid)
                && *previous != observation.identity
            {
                events.push(ContainmentEvent::IdentityChanged {
                    pid: observation.identity.pid,
                    previous_start_abstime: previous.start_abstime,
                    observed_start_abstime: observation.identity.start_abstime,
                });
            }
        }
        for tracked in self.tracked.values() {
            if !observations
                .values()
                .any(|current| current.identity == tracked.identity)
            {
                events.push(ContainmentEvent::Disappeared(tracked.identity));
            }
        }
        let failures: Vec<_> = snapshot
            .failures
            .into_iter()
            .filter(|failure| {
                failure.kind != ObservationFailureKind::Disappeared
                    || failure.pid.is_none()
                    || failure.pid == Some(self.root.pid)
                    || failure.pid.is_some_and(|pid| {
                        self.tracked
                            .values()
                            .any(|tracked| tracked.identity.pid == pid)
                    })
            })
            .collect();
        events.extend(
            failures
                .iter()
                .copied()
                .map(ContainmentEvent::ObservationFailed),
        );
        (observations, events, failures)
    }

    fn owned_group_exists(&self) -> bool {
        // SAFETY: `owned_group` is positive, so its negation cannot target PID or PGID zero.
        unsafe {
            libc::kill(-self.owned_group, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }

    fn snapshot_scope(&self) -> (ProcessIdentity, i32, Vec<i32>) {
        (
            self.root,
            self.owned_group,
            self.tracked.keys().map(|identity| identity.pid).collect(),
        )
    }
}

fn descends_from_root(
    observation: ProcessObservation,
    observations: &BTreeMap<i32, ProcessObservation>,
    root_pid: i32,
) -> bool {
    let mut parent = observation.parent_pid;
    let mut visited = BTreeSet::new();
    while parent > 0 && visited.insert(parent) {
        if parent == root_pid {
            return true;
        }
        let Some(next) = observations.get(&parent) else {
            return false;
        };
        parent = next.parent_pid;
    }
    false
}

fn aggregate(
    members: &[ProcessObservation],
    failures: &[ObservationFailure],
    previous: &BTreeMap<ProcessIdentity, TrackedProcess>,
    root: ProcessIdentity,
    root_was_observed: bool,
) -> AggregateFootprint {
    let mut known_bytes = 0_u64;
    let mut missing = Vec::new();
    for member in members {
        let Some(bytes) = member.footprint_bytes else {
            missing.push(member.identity);
            continue;
        };
        let Some(total) = known_bytes.checked_add(bytes) else {
            return AggregateFootprint::Overflow;
        };
        known_bytes = total;
    }
    for failure in failures {
        if let Some(pid) = failure.pid {
            missing.extend(
                previous
                    .keys()
                    .filter(|identity| identity.pid == pid)
                    .copied(),
            );
            if pid == root.pid {
                missing.push(root);
            }
        } else {
            missing.extend(previous.keys().copied());
            missing.push(root);
        }
    }
    if !root_was_observed {
        missing.push(root);
    }
    missing.sort_unstable();
    missing.dedup();
    if missing.is_empty() && failures.is_empty() {
        AggregateFootprint::Complete(known_bytes)
    } else {
        AggregateFootprint::Incomplete {
            known_bytes,
            missing_identities: missing,
        }
    }
}

/// Final owned-group cleanup evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupReport {
    pub owned_group_empty: bool,
    pub complete: bool,
    pub survivors: Vec<ProcessIdentity>,
    pub escaped_identities: Vec<ProcessIdentity>,
    pub observation_failures: Vec<ObservationFailure>,
}

/// Native process inventory for the current Unix test or Darwin runtime host.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeProcessInventory;

impl NativeProcessInventory {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Probe whether this host exposes the required Darwin physical-footprint capability.
    ///
    /// # Errors
    ///
    /// Returns a typed unavailable result instead of inferring support from the operating system
    /// name or from topology-only process inspection.
    pub fn probe_footprint(&self) -> Result<(), IdentityUnavailable> {
        native::probe_footprint()
    }

    /// Inspect a positive PID and bind its current start token.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityUnavailable`] when the PID cannot be safely inspected.
    pub fn inspect(&self, pid: i32) -> Result<ProcessObservation, IdentityUnavailable> {
        native::inspect(pid)
    }

    /// Inspect and require an exact `(pid, start_abstime)` match.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityUnavailable::Stale`] if the PID now names another process.
    pub fn inspect_expected(
        &self,
        expected: ProcessIdentity,
    ) -> Result<ProcessObservation, IdentityUnavailable> {
        let observation = self.inspect(expected.pid)?;
        if observation.identity != expected {
            return Err(IdentityUnavailable::Stale);
        }
        Ok(observation)
    }

    /// Enumerate a best-effort non-atomic system snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] only when the PID list itself cannot be obtained.
    pub fn snapshot(&self) -> Result<ProcessSnapshot, SnapshotError> {
        let pids = native::list_pids()?;
        Ok((*self).snapshot_pids(pids))
    }

    pub(crate) fn snapshot_for_tracker(
        self,
        tracker: &IdentityTracker,
    ) -> Result<ProcessSnapshot, SnapshotError> {
        let (root, owned_group, tracked_pids) = tracker.snapshot_scope();
        let pids = native::list_relevant_pids(root.pid, owned_group, &tracked_pids)?;
        Ok(self.snapshot_pids(pids))
    }

    fn snapshot_pids(self, pids: Vec<i32>) -> ProcessSnapshot {
        let mut observations = Vec::new();
        let mut failures = Vec::new();
        for pid in pids {
            match self.inspect(pid) {
                Ok(observation) => observations.push(observation),
                Err(error) => failures.push(ObservationFailure {
                    pid: Some(pid),
                    kind: failure_kind(error),
                }),
            }
        }
        ProcessSnapshot {
            observations,
            failures,
        }
    }

    /// Signal only after exact identity revalidation.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityUnavailable`] for stale, exited, missing, denied, or failed targets.
    pub fn signal_identity(
        &self,
        expected: ProcessIdentity,
        signal: SignalNumber,
    ) -> Result<SignalResult, IdentityUnavailable> {
        let observation = self.inspect_expected(expected)?;
        if observation.exited {
            return Err(IdentityUnavailable::Disappeared);
        }
        // SAFETY: identity was revalidated immediately above and its PID is positive.
        if unsafe { libc::kill(expected.pid, i32::from(signal.get())) } == 0 {
            return Ok(SignalResult::Delivered);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Err(IdentityUnavailable::Disappeared),
            Some(libc::EPERM | libc::EACCES) => Err(IdentityUnavailable::PermissionDenied),
            _ => Err(IdentityUnavailable::Unavailable),
        }
    }
}

fn failure_kind(error: IdentityUnavailable) -> ObservationFailureKind {
    match error {
        IdentityUnavailable::Disappeared => ObservationFailureKind::Disappeared,
        IdentityUnavailable::PermissionDenied => ObservationFailureKind::PermissionDenied,
        IdentityUnavailable::Stale => ObservationFailureKind::StaleIdentity,
        IdentityUnavailable::MalformedData => ObservationFailureKind::MalformedData,
        IdentityUnavailable::Unsupported => ObservationFailureKind::Unsupported,
        IdentityUnavailable::Unavailable => ObservationFailureKind::Unavailable,
    }
}

/// Poll real native snapshots until the owned group is empty or the deadline expires.
#[must_use]
pub fn wait_for_owned_group_empty(
    inventory: &NativeProcessInventory,
    tracker: &mut IdentityTracker,
    timeout: Duration,
    poll_interval: Duration,
) -> CleanupReport {
    let deadline = Instant::now() + timeout;
    loop {
        match inventory.snapshot_for_tracker(tracker) {
            Ok(snapshot) => {
                let frame = tracker.update(snapshot);
                let survivors: Vec<_> = frame
                    .owned_members
                    .iter()
                    .map(|member| member.identity)
                    .collect();
                let group_exists = tracker.owned_group_exists();
                let owned_group_empty = survivors.is_empty() && !group_exists;
                if owned_group_empty || Instant::now() >= deadline {
                    let mut observation_failures = frame.observation_failures;
                    if group_exists && survivors.is_empty() {
                        observation_failures.push(ObservationFailure {
                            pid: None,
                            kind: ObservationFailureKind::EnumerationFailed,
                        });
                    }
                    let escaped_identities: Vec<_> = tracker.escaped.iter().copied().collect();
                    let complete = owned_group_empty
                        && escaped_identities.is_empty()
                        && observation_failures.is_empty();
                    return CleanupReport {
                        owned_group_empty,
                        complete,
                        survivors,
                        escaped_identities,
                        observation_failures,
                    };
                }
            }
            Err(error) => {
                return CleanupReport {
                    owned_group_empty: false,
                    complete: false,
                    survivors: Vec::new(),
                    escaped_identities: Vec::new(),
                    observation_failures: vec![ObservationFailure {
                        pid: None,
                        kind: error.kind,
                    }],
                };
            }
        }
        thread::sleep(poll_interval.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::collections::{BTreeSet, VecDeque};
    use std::ffi::c_void;
    use std::mem::{MaybeUninit, size_of};
    use std::ptr;

    use super::{IdentityUnavailable, ProcessIdentity, ProcessObservation, SnapshotError};
    use crate::ObservationFailureKind;

    const PROC_PIDTBSDINFO: i32 = 3;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RUsageInfoV0 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        package_idle_wakeups: u64,
        interrupt_wakeups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        physical_footprint: u64,
        process_start_abstime: u64,
        process_exit_abstime: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcBsdInfo {
        scalar_prefix: [u32; 12],
        pbi_comm: [u8; 16],
        pbi_name: [u8; 32],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    unsafe extern "C" {
        fn proc_listallpids(buffer: *mut c_void, buffersize: i32) -> i32;
        fn proc_listpgrppids(pgrpid: i32, buffer: *mut c_void, buffersize: i32) -> i32;
        fn proc_listchildpids(ppid: i32, buffer: *mut c_void, buffersize: i32) -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut c_void,
            buffersize: i32,
        ) -> i32;
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut c_void) -> i32;
    }

    pub(super) fn probe_footprint() -> Result<(), IdentityUnavailable> {
        // SAFETY: getpid has no preconditions and returns the current positive process identifier.
        let current_pid = unsafe { libc::getpid() };
        let observation = inspect(current_pid)?;
        observation
            .footprint_bytes
            .map(|_| ())
            .ok_or(IdentityUnavailable::Unsupported)
    }

    pub(super) fn list_pids() -> Result<Vec<i32>, SnapshotError> {
        // SAFETY: a null buffer asks libproc for the current capacity estimate.
        let estimate = unsafe { proc_listallpids(ptr::null_mut(), 0) };
        if estimate <= 0 {
            return Err(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            });
        }
        let capacity = usize::try_from(estimate)
            .ok()
            .and_then(|value| value.checked_add(128))
            .ok_or(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            })?;
        let mut pids = vec![0_i32; capacity];
        let bytes = i32::try_from(capacity * size_of::<i32>()).map_err(|_| SnapshotError {
            kind: ObservationFailureKind::EnumerationFailed,
        })?;
        // SAFETY: the buffer holds `capacity` i32 values and `bytes` describes its full size.
        let count = unsafe { proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
        if count < 0 {
            return Err(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            });
        }
        pids.truncate(usize::try_from(count).unwrap_or(0));
        pids.retain(|pid| *pid > 0);
        Ok(pids)
    }

    pub(super) fn list_relevant_pids(
        root_pid: i32,
        owned_group: i32,
        tracked_pids: &[i32],
    ) -> Result<Vec<i32>, SnapshotError> {
        let mut relevant: BTreeSet<i32> = tracked_pids
            .iter()
            .copied()
            .filter(|pid| *pid > 0)
            .collect();
        relevant.insert(root_pid);
        relevant.extend(list_related_pids(proc_listpgrppids, owned_group)?);

        let mut visited = BTreeSet::new();
        let mut pending: VecDeque<_> = relevant.iter().copied().collect();
        while let Some(parent_pid) = pending.pop_front() {
            if !visited.insert(parent_pid) {
                continue;
            }
            for child_pid in list_related_pids(proc_listchildpids, parent_pid)? {
                if relevant.insert(child_pid) {
                    pending.push_back(child_pid);
                }
            }
        }
        Ok(relevant.into_iter().collect())
    }

    type RelatedPidList = unsafe extern "C" fn(i32, *mut c_void, i32) -> i32;

    fn list_related_pids(
        function: RelatedPidList,
        identifier: i32,
    ) -> Result<Vec<i32>, SnapshotError> {
        // SAFETY: a null buffer asks libproc for the current capacity estimate.
        let estimate = unsafe { function(identifier, ptr::null_mut(), 0) };
        if estimate < 0 {
            return Err(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            });
        }
        if estimate == 0 {
            return Ok(Vec::new());
        }
        let capacity = usize::try_from(estimate)
            .ok()
            .and_then(|value| value.checked_add(16))
            .ok_or(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            })?;
        let mut pids = vec![0_i32; capacity];
        let bytes = i32::try_from(capacity * size_of::<i32>()).map_err(|_| SnapshotError {
            kind: ObservationFailureKind::EnumerationFailed,
        })?;
        // SAFETY: the buffer holds `capacity` i32 values and the function only writes that buffer.
        let count = unsafe { function(identifier, pids.as_mut_ptr().cast::<c_void>(), bytes) };
        if count < 0 || usize::try_from(count).is_ok_and(|count| count >= capacity) {
            return Err(SnapshotError {
                kind: ObservationFailureKind::EnumerationFailed,
            });
        }
        pids.truncate(usize::try_from(count).unwrap_or(0));
        pids.retain(|pid| *pid > 0);
        Ok(pids)
    }

    pub(super) fn inspect(pid: i32) -> Result<ProcessObservation, IdentityUnavailable> {
        if pid <= 0 {
            return Err(IdentityUnavailable::MalformedData);
        }
        let first = rusage(pid)?;
        if first.process_exit_abstime != 0 {
            return Ok(ProcessObservation {
                identity: ProcessIdentity {
                    pid,
                    start_abstime: first.process_start_abstime,
                },
                parent_pid: 0,
                process_group_id: 0,
                footprint_bytes: Some(first.physical_footprint),
                exited: true,
            });
        }
        let mut bsd = MaybeUninit::<ProcBsdInfo>::zeroed();
        let bsd_size = i32::try_from(size_of::<ProcBsdInfo>())
            .map_err(|_| IdentityUnavailable::MalformedData)?;
        // SAFETY: buffer is writable and its independently checked repr(C) size is supplied.
        let returned = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTBSDINFO,
                0,
                bsd.as_mut_ptr().cast::<c_void>(),
                bsd_size,
            )
        };
        if returned != bsd_size {
            return Err(classify_errno());
        }
        // SAFETY: proc_pidinfo returned the complete requested structure size.
        let bsd = unsafe { bsd.assume_init() };
        let second = rusage(pid)?;
        if first.process_start_abstime != second.process_start_abstime {
            return Err(IdentityUnavailable::Stale);
        }
        let observed_pid =
            i32::try_from(bsd.scalar_prefix[3]).map_err(|_| IdentityUnavailable::MalformedData)?;
        if observed_pid != pid {
            return Err(IdentityUnavailable::Stale);
        }
        Ok(ProcessObservation {
            identity: ProcessIdentity {
                pid,
                start_abstime: second.process_start_abstime,
            },
            parent_pid: i32::try_from(bsd.scalar_prefix[4])
                .map_err(|_| IdentityUnavailable::MalformedData)?,
            process_group_id: i32::try_from(bsd.pbi_pgid)
                .map_err(|_| IdentityUnavailable::MalformedData)?,
            footprint_bytes: Some(second.physical_footprint),
            exited: second.process_exit_abstime != 0,
        })
    }

    fn rusage(pid: i32) -> Result<RUsageInfoV0, IdentityUnavailable> {
        let mut info = MaybeUninit::<RUsageInfoV0>::zeroed();
        // SAFETY: the V0 buffer has the SDK-verified layout and is writable.
        if unsafe { proc_pid_rusage(pid, 0, info.as_mut_ptr().cast::<c_void>()) } != 0 {
            return Err(classify_errno());
        }
        // SAFETY: proc_pid_rusage initializes the complete V0 structure on success.
        Ok(unsafe { info.assume_init() })
    }

    fn classify_errno() -> IdentityUnavailable {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => IdentityUnavailable::Disappeared,
            Some(libc::EPERM | libc::EACCES) => IdentityUnavailable::PermissionDenied,
            Some(libc::ENOTSUP | libc::ENOSYS) => IdentityUnavailable::Unsupported,
            _ => IdentityUnavailable::Unavailable,
        }
    }

    #[cfg(test)]
    mod tests {
        use std::mem::{offset_of, size_of};

        use super::{ProcBsdInfo, RUsageInfoV0};

        #[test]
        fn sdk_struct_layouts_match_the_verified_darwin_abi() {
            assert_eq!(size_of::<RUsageInfoV0>(), 96);
            assert_eq!(offset_of!(RUsageInfoV0, physical_footprint), 72);
            assert_eq!(offset_of!(RUsageInfoV0, process_start_abstime), 80);
            assert_eq!(offset_of!(RUsageInfoV0, process_exit_abstime), 88);
            assert_eq!(size_of::<ProcBsdInfo>(), 136);
            assert_eq!(offset_of!(ProcBsdInfo, pbi_pgid), 100);
        }
    }
}

#[cfg(target_os = "linux")]
mod native {
    use std::fs;

    use super::{IdentityUnavailable, ProcessIdentity, ProcessObservation, SnapshotError};
    use crate::ObservationFailureKind;

    pub(super) fn probe_footprint() -> Result<(), IdentityUnavailable> {
        Err(IdentityUnavailable::Unsupported)
    }

    pub(super) fn list_pids() -> Result<Vec<i32>, SnapshotError> {
        let entries = fs::read_dir("/proc").map_err(|_| SnapshotError {
            kind: ObservationFailureKind::EnumerationFailed,
        })?;
        Ok(entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
            .collect())
    }

    pub(super) fn list_relevant_pids(
        _root_pid: i32,
        _owned_group: i32,
        _tracked_pids: &[i32],
    ) -> Result<Vec<i32>, SnapshotError> {
        list_pids()
    }

    pub(super) fn inspect(pid: i32) -> Result<ProcessObservation, IdentityUnavailable> {
        if pid <= 0 {
            return Err(IdentityUnavailable::MalformedData);
        }
        let value =
            fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| classify_io(&error))?;
        let close = value.rfind(')').ok_or(IdentityUnavailable::MalformedData)?;
        let fields: Vec<_> = value[close + 1..].split_whitespace().collect();
        if fields.len() <= 19 {
            return Err(IdentityUnavailable::MalformedData);
        }
        let state = fields[0];
        let parent_pid = fields[1]
            .parse()
            .map_err(|_| IdentityUnavailable::MalformedData)?;
        let process_group_id = fields[2]
            .parse()
            .map_err(|_| IdentityUnavailable::MalformedData)?;
        let start_abstime = fields[19]
            .parse()
            .map_err(|_| IdentityUnavailable::MalformedData)?;
        Ok(ProcessObservation {
            identity: ProcessIdentity { pid, start_abstime },
            parent_pid,
            process_group_id,
            footprint_bytes: None,
            exited: matches!(state, "Z" | "X"),
        })
    }

    fn classify_io(error: &std::io::Error) -> IdentityUnavailable {
        match error.kind() {
            std::io::ErrorKind::NotFound => IdentityUnavailable::Disappeared,
            std::io::ErrorKind::PermissionDenied => IdentityUnavailable::PermissionDenied,
            _ => IdentityUnavailable::Unavailable,
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod native {
    use super::{IdentityUnavailable, ProcessObservation, SnapshotError};
    use crate::ObservationFailureKind;

    pub(super) fn probe_footprint() -> Result<(), IdentityUnavailable> {
        Err(IdentityUnavailable::Unsupported)
    }

    pub(super) fn list_pids() -> Result<Vec<i32>, SnapshotError> {
        Err(SnapshotError {
            kind: ObservationFailureKind::Unsupported,
        })
    }

    pub(super) fn list_relevant_pids(
        _root_pid: i32,
        _owned_group: i32,
        _tracked_pids: &[i32],
    ) -> Result<Vec<i32>, SnapshotError> {
        list_pids()
    }

    pub(super) fn inspect(_pid: i32) -> Result<ProcessObservation, IdentityUnavailable> {
        Err(IdentityUnavailable::Unsupported)
    }
}
