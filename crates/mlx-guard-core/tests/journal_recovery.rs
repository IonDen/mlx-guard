#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    Capabilities, JournalDurability, JournalEntry, JournalHeader, JournalRecord, JournalRecovery,
    JournalRecoveryStatus, MAX_JOURNAL_RECORD_BYTES, PolicyState, PrivacyDefaults,
    ReportConfiguration, ReportMode, ReportV1, RunIdentity, SecureJournal, SignalNumber,
    SignalRecord, SignalResult, SignalTarget, StorageErrorKind, TransitionRecord,
};

struct TestDirectory(PathBuf);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-journal-{}-{unique}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
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

fn transition(sequence: u64) -> JournalRecord {
    JournalRecord::new(
        sequence,
        JournalEntry::Transition(TransitionRecord {
            at_ms: 10 + sequence,
            from: PolicyState::Warning,
            to: PolicyState::Terminating,
            aggregate_footprint_bytes: Some(101),
        }),
    )
}

fn signal(sequence: u64) -> JournalRecord {
    JournalRecord::new(
        sequence,
        JournalEntry::Signal(SignalRecord {
            at_ms: 10 + sequence,
            signal: SignalNumber::new(15).unwrap().get(),
            target: SignalTarget::OwnedProcessGroup,
            result: SignalResult::Delivered,
            reason: None,
        }),
    )
}

#[test]
fn versioned_checksummed_records_round_trip_in_sequence() {
    // Catches raw newline JSON, missing sequence checks, or a frame that cannot be recovered.
    let directory = TestDirectory::new();
    let mut journal = SecureJournal::initialize(&directory.0.join("report.json")).unwrap();
    journal
        .append(&transition(0), JournalDurability::Sync)
        .unwrap();
    journal.append(&signal(1), JournalDurability::Sync).unwrap();
    journal.flush().unwrap();

    let recovered = JournalRecovery::read(journal.journal_path()).unwrap();
    assert_eq!(recovered.status, JournalRecoveryStatus::Complete);
    assert_eq!(recovered.records, [transition(0), signal(1)]);
}

#[test]
fn decisive_transition_cannot_be_buffered_before_its_signal() {
    // Catches allowing the intervention state to remain volatile while its signal is delivered.
    let directory = TestDirectory::new();
    let mut journal = SecureJournal::initialize(&directory.0.join("report.json")).unwrap();
    let before = fs::metadata(journal.journal_path()).unwrap().len();

    let error = journal
        .append(&transition(0), JournalDurability::Buffered)
        .unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::InvalidRecord);
    assert_eq!(fs::metadata(journal.journal_path()).unwrap().len(), before);
}

#[test]
fn truncated_tail_recovers_only_the_last_valid_prefix() {
    // Catches all-or-nothing recovery or accepting a partial decisive record as durable evidence.
    let directory = TestDirectory::new();
    let path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&path).unwrap();
    journal
        .append(&transition(0), JournalDurability::Sync)
        .unwrap();
    journal.append(&signal(1), JournalDurability::Sync).unwrap();
    let journal_path = journal.journal_path().to_path_buf();
    drop(journal);
    let length = fs::metadata(&journal_path).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&journal_path)
        .unwrap()
        .set_len(length - 3)
        .unwrap();

    let recovered = JournalRecovery::read(&journal_path).unwrap();
    assert_eq!(recovered.status, JournalRecoveryStatus::Truncated);
    assert_eq!(recovered.records, [transition(0)]);
}

#[test]
fn corrupt_checksum_and_out_of_sequence_append_are_typed() {
    // Catches silently accepting modified evidence or ambiguous record ordering.
    let directory = TestDirectory::new();
    let path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&path).unwrap();
    journal
        .append(&transition(0), JournalDurability::Sync)
        .unwrap();
    assert_eq!(
        journal
            .append(&signal(2), JournalDurability::Sync)
            .unwrap_err()
            .kind(),
        StorageErrorKind::InvalidRecord
    );
    let journal_path = journal.journal_path().to_path_buf();
    drop(journal);

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&journal_path)
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[byte[0] ^ 0xff]).unwrap();
    file.sync_all().unwrap();
    let recovered = JournalRecovery::read(&journal_path).unwrap();
    assert_eq!(recovered.status, JournalRecoveryStatus::Corrupt);
    assert!(recovered.records.is_empty());
}

#[test]
fn oversized_record_is_rejected_before_any_bytes_are_appended() {
    // Catches allocating or writing an attacker-sized journal frame before enforcing its bound.
    let directory = TestDirectory::new();
    let path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&path).unwrap();
    let before = fs::metadata(journal.journal_path()).unwrap().len();
    let header = JournalHeader {
        schema_version: 1,
        package_version: "x".repeat(MAX_JOURNAL_RECORD_BYTES),
        run: RunIdentity::from_argv(
            "run_0123456789abcdef0123456789abcdef",
            &["worker".into()],
            None,
        )
        .unwrap(),
        capabilities: Capabilities {
            darwin_footprint: mlx_guard_core::Observed::Unknown,
            owned_process_group: mlx_guard_core::Observed::Unknown,
            checkpoint_channel: mlx_guard_core::Observed::Unknown,
        },
        configuration: ReportConfiguration {
            mode: ReportMode::Observe,
            max_footprint_bytes: None,
            warning_footprint_bytes: None,
            recovery_footprint_bytes: None,
            emergency_footprint_bytes: None,
            required_breach_samples: 1,
            max_missing_samples: 1,
            wall_time_ms: None,
            sample_interval_ms: 50,
            max_sample_age_ms: 100,
            max_sample_window_ms: 10,
            checkpoint_timeout_ms: None,
            term_grace_ms: 100,
        },
        privacy: PrivacyDefaults::default(),
    };

    let error = journal
        .append(
            &JournalRecord::new(0, JournalEntry::Header(Box::new(header))),
            JournalDurability::Buffered,
        )
        .unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::InvalidRecord);
    assert_eq!(fs::metadata(journal.journal_path()).unwrap().len(), before);
}

#[test]
fn unknown_record_fields_are_ignored_during_recovery() {
    // Catches a strict unknown-field mutant that turns a forward-compatible journal record into
    // an unrecoverable one instead of parsing the known fields and ignoring the rest.
    let directory = TestDirectory::new();
    let path = directory.0.join("report.json");
    let journal = SecureJournal::initialize(&path).unwrap();
    let journal_path = journal.journal_path().to_path_buf();
    drop(journal);
    let mut value = serde_json::to_value(transition(0)).unwrap();
    value["future_field"] = serde_json::Value::String("ignored".to_owned());
    let payload = serde_json::to_vec(&value).unwrap();
    let checksum = test_crc32(&payload);
    let mut file = OpenOptions::new().append(true).open(&journal_path).unwrap();
    file.write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    file.write_all(&payload).unwrap();
    file.write_all(&checksum.to_be_bytes()).unwrap();
    file.sync_all().unwrap();

    let recovered = JournalRecovery::read(&journal_path).unwrap();

    assert_eq!(recovered.status, JournalRecoveryStatus::Complete);
    assert_eq!(recovered.records, [transition(0)]);
}

#[test]
fn corrupt_tail_after_an_outcome_never_projects_a_complete_report() {
    // Catches accepting a valid-looking terminal prefix while ignoring later damaged evidence.
    let directory = TestDirectory::new();
    let path = directory.0.join("report.json");
    let mut journal = SecureJournal::initialize(&path).unwrap();
    let complete = ReportV1::from_json(include_str!("fixtures/report-v1.json")).unwrap();
    journal.append_report(&complete).unwrap();
    let journal_path = journal.journal_path().to_path_buf();
    drop(journal);
    let mut file = OpenOptions::new().append(true).open(&journal_path).unwrap();
    file.write_all(&[0, 0]).unwrap();
    file.sync_all().unwrap();

    let recovered = JournalRecovery::read(&journal_path).unwrap();

    assert_eq!(recovered.status, JournalRecoveryStatus::Truncated);
    assert_eq!(
        recovered.to_report().unwrap_err().kind(),
        StorageErrorKind::InvalidRecord
    );
}

fn test_crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
