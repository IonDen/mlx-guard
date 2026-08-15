#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    JournalEntry, JournalHeader, JournalRecord, JournalRecovery, JournalRecoveryStatus, ReportV1,
    RunIdentity, SecureJournal, StorageErrorKind,
};

struct TestDirectory(PathBuf);

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-finalize-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn report() -> ReportV1 {
    ReportV1::from_json(include_str!("fixtures/report-v1.json")).unwrap()
}

#[test]
fn terminal_sample_history_reset_projects_the_recent_window() {
    // Catches final reports remaining pinned to the first samples after the in-memory ring rotates.
    let original = report();
    let mut recent = original.samples[0].clone();
    recent.captured_at_ms = 14;
    recent.processed_at_ms = 14;
    let header = JournalHeader {
        schema_version: original.schema_version,
        package_version: original.package_version.clone(),
        run: original.run.clone(),
        capabilities: original.capabilities.clone(),
        configuration: original.configuration.clone(),
        privacy: original.privacy.clone(),
    };
    let entries = vec![
        JournalEntry::Header(Box::new(header)),
        JournalEntry::Sample(Box::new(original.samples[0].clone())),
        JournalEntry::SampleHistoryReset,
        JournalEntry::Sample(Box::new(recent.clone())),
        JournalEntry::Transition(original.transitions[0].clone()),
        JournalEntry::Signal(original.signals[0].clone()),
        JournalEntry::Checkpoint(original.checkpoint.clone()),
        JournalEntry::Escape(original.escape.clone()),
        JournalEntry::Outcome(original.outcome.clone()),
    ];
    let recovery = JournalRecovery {
        records: entries
            .into_iter()
            .enumerate()
            .map(|(sequence, entry)| JournalRecord::new(sequence as u64, entry))
            .collect(),
        status: JournalRecoveryStatus::Complete,
    };

    assert_eq!(recovery.to_report().unwrap().samples, [recent]);
}

#[test]
fn journal_replay_atomically_writes_the_schema_v1_golden_and_summary() {
    // Catches a second report projection, non-atomic output, or summary text containing raw inputs.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&report()).unwrap();
    let finalized = journal.finalize().unwrap();

    assert_eq!(
        fs::read_to_string(&report_path).unwrap(),
        report().to_json_pretty().unwrap()
    );
    assert_eq!(
        fs::metadata(&report_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(finalized.report, report());
    assert_eq!(
        finalized.summary,
        "mlx-guard: policy_intervention at 15ms; 1 sample, 1 signal\n"
    );
    assert!(!directory.0.join(".report.json.tmp").exists());
}

#[test]
fn a_durable_terminal_prefix_can_be_replayed_after_supervisor_abort() {
    // Catches final JSON being the only source from which a completed run can be recovered.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let journal_path = {
        let mut journal = SecureJournal::initialize(&report_path).unwrap();
        journal.append_report(&report()).unwrap();
        journal.journal_path().to_path_buf()
    };

    let recovered = JournalRecovery::read(&journal_path).unwrap();
    assert_eq!(recovered.to_report().unwrap(), report());
    assert!(!report_path.exists());
}

#[test]
fn temp_or_report_symlink_attack_is_rejected_without_touching_its_target() {
    // Catches following attacker-controlled output links during create or atomic replacement.
    let directory = TestDirectory::new();
    let outside = directory.0.join("outside");
    fs::write(&outside, b"unchanged").unwrap();
    let report_path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&report()).unwrap();
    symlink(&outside, directory.0.join(".report.json.tmp")).unwrap();

    let error = journal.finalize().unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnsafeTarget);
    assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
    assert!(!report_path.exists());
}

#[test]
fn report_symlink_attack_is_rejected_without_touching_its_target() {
    // Catches validating only the temporary name while replacing a linked final-report target.
    let directory = TestDirectory::new();
    let outside = directory.0.join("outside-report");
    fs::write(&outside, b"unchanged").unwrap();
    let report_path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&report()).unwrap();
    symlink(&outside, &report_path).unwrap();

    let error = journal.finalize().unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnsafeTarget);
    assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
}

#[test]
fn finalization_stays_anchored_when_the_parent_path_is_replaced() {
    // Catches reopening journal or output paths through a parent that changed after validation.
    let directory = TestDirectory::new();
    let original = directory.0.join("original");
    let moved = directory.0.join("moved");
    let attacker = directory.0.join("attacker");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&attacker).unwrap();
    fs::set_permissions(&original, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&attacker, fs::Permissions::from_mode(0o700)).unwrap();
    let report_path = original.join("report.json");
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&report()).unwrap();

    fs::rename(&original, &moved).unwrap();
    symlink(&attacker, &original).unwrap();
    let finalized = journal.finalize().unwrap();

    assert_eq!(finalized.report, report());
    assert_eq!(
        fs::read_to_string(moved.join("report.json")).unwrap(),
        report().to_json_pretty().unwrap()
    );
    assert!(!attacker.join("report.json").exists());
}

#[test]
fn an_owner_only_existing_report_is_atomically_replaced() {
    // Catches create-new-only finalization that cannot safely update an earlier valid report.
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    fs::write(&report_path, b"old report").unwrap();
    fs::set_permissions(&report_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&report()).unwrap();

    journal.finalize().unwrap();

    assert_eq!(
        fs::read_to_string(report_path).unwrap(),
        report().to_json_pretty().unwrap()
    );
}

#[test]
fn failed_finalization_never_claims_or_leaves_a_complete_report() {
    // Catches treating an unwritable final directory as success or leaving a partial JSON file.
    let directory = TestDirectory::new();
    if fs::metadata(&directory.0).unwrap().uid() == 0 {
        return;
    }
    let report_path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&report()).unwrap();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o500)).unwrap();

    let error = journal.finalize().unwrap_err();

    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(error.kind(), StorageErrorKind::FinalizeFailed);
    assert!(!report_path.exists());
    assert!(!directory.0.join(".report.json.tmp").exists());
}

#[test]
fn raw_command_canary_never_reaches_any_persisted_or_displayed_artifact() {
    // Catches persisting argv/path data before applying the schema-v1 redaction projection.
    const CANARY: &str = "SECRET_ARGV_ENV_OUTPUT_CANARY";
    let directory = TestDirectory::new();
    let report_path = directory.0.join("report.json");
    let mut redacted = report();
    redacted.run = RunIdentity::from_argv(
        "run_0123456789abcdef0123456789abcdef",
        &[
            OsString::from(format!("/private/{CANARY}/python")),
            OsString::from(format!("--token={CANARY}")),
        ],
        Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    let mut journal = SecureJournal::initialize(&report_path).unwrap();
    journal.append_report(&redacted).unwrap();

    let journal_bytes = fs::read(journal.journal_path()).unwrap();
    assert!(!String::from_utf8_lossy(&journal_bytes).contains(CANARY));
    journal.finalize().unwrap();
    assert!(!fs::read_to_string(report_path).unwrap().contains(CANARY));
}
