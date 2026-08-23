#![cfg(unix)]

use mlx_guard_core::{
    JournalAppender, JournalDurability, JournalEntry, JournalRecord, PersistenceAttempt,
    ResilientJournal, StorageErrorKind, SupervisorOutcome,
};

#[derive(Debug)]
struct FaultAppender {
    calls: usize,
    fail_at: usize,
    failure: StorageErrorKind,
}

impl JournalAppender for FaultAppender {
    fn append_record(
        &mut self,
        _record: &JournalRecord,
        _durability: JournalDurability,
    ) -> Result<(), StorageErrorKind> {
        self.calls += 1;
        if self.calls == self.fail_at {
            Err(self.failure)
        } else {
            Ok(())
        }
    }
}

fn record(sequence: u64) -> JournalRecord {
    JournalRecord::new(
        sequence,
        JournalEntry::Outcome(mlx_guard_core::TerminalOutcome {
            at_ms: sequence,
            kind: mlx_guard_core::TerminalKind::PolicyIntervention,
            final_footprint_bytes: mlx_guard_core::Observed::Unavailable {
                reason: mlx_guard_core::UnavailableReason::NotApplicable,
            },
            child_status: None,
            owned_group_survivors: None,
        }),
    )
}

#[test]
fn enospc_write_failure_is_latched_while_safety_work_continues() {
    // Catches propagating a late artifact write failure out of the enforcement loop.
    let mut journal = ResilientJournal::new(FaultAppender {
        calls: 0,
        fail_at: 2,
        failure: StorageErrorKind::WriteFailed,
    });
    let mut safety_actions = 0;

    let first = journal.record(&record(0), JournalDurability::Buffered);
    safety_actions += 1;
    let failed = journal.record(&record(1), JournalDurability::Sync);
    safety_actions += 1;
    let disabled = journal.record(&record(2), JournalDurability::Sync);
    safety_actions += 1;

    assert_eq!(first, PersistenceAttempt::Persisted);
    assert_eq!(
        failed,
        PersistenceAttempt::Failed(StorageErrorKind::WriteFailed)
    );
    assert_eq!(
        disabled,
        PersistenceAttempt::Disabled(StorageErrorKind::WriteFailed)
    );
    assert_eq!(safety_actions, 3);
    assert_eq!(journal.failure_kind(), Some(StorageErrorKind::WriteFailed));
    assert_eq!(
        journal.supervisor_outcome(),
        Some(SupervisorOutcome::PartialArtifactFailure)
    );
    assert_eq!(SupervisorOutcome::PartialArtifactFailure.exit_code(), 74);
    assert_eq!(
        journal.stderr_notice(),
        Some(
            "mlx-guard: artifact write failed; safety enforcement continues; complete report unavailable"
        )
    );
}

#[test]
fn eio_sync_failure_disables_later_writes_without_false_recovery() {
    // Catches retrying through a poisoned stream or reporting a complete artifact after EIO.
    let mut journal = ResilientJournal::new(FaultAppender {
        calls: 0,
        fail_at: 1,
        failure: StorageErrorKind::SyncFailed,
    });

    assert_eq!(
        journal.record(&record(0), JournalDurability::Sync),
        PersistenceAttempt::Failed(StorageErrorKind::SyncFailed)
    );
    assert_eq!(
        journal.record(&record(0), JournalDurability::Sync),
        PersistenceAttempt::Disabled(StorageErrorKind::SyncFailed)
    );
    assert!(journal.into_inner().is_none());
}
