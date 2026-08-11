#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mlx_guard_core::{
    CheckpointHello, CheckpointNonce, CheckpointProtocol, CheckpointRequest, JournalDurability,
    JournalEntry, JournalRecord, JournalRecovery, PolicyState, ReportV1, SecureJournal,
    TransitionRecord,
};

const MAX_CASE_RUNTIME: Duration = Duration::from_secs(10);
const CHECKPOINT_CASES: usize = 4_096;
const REPORT_CASES: usize = 4_096;
const JOURNAL_CASES: usize = 256;
const MAX_CHECKPOINT_INPUT: usize = 256;
const MAX_REPORT_INPUT: usize = 16 * 1024;
const MAX_JOURNAL_INPUT: usize = 4 * 1024;

struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn index(&mut self, upper: usize) -> usize {
        usize::try_from(self.next()).unwrap_or(usize::MAX) % upper
    }

    fn bytes(&mut self, length: usize) -> Vec<u8> {
        (0..length).map(|_| self.next().to_le_bytes()[0]).collect()
    }
}

#[test]
fn checkpoint_parsers_reject_bounded_deterministic_fuzz_without_panicking() {
    let started = Instant::now();
    let mut generator = Generator(0x6d6c_782d_6775_6172);
    for _ in 0..CHECKPOINT_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let length = generator.index(MAX_CHECKPOINT_INPUT + 1);
        let input = generator.bytes(length);
        let _ = CheckpointHello::decode(&input);
        let _ = CheckpointRequest::decode(&input);

        let mut protocol = CheckpointProtocol::new(CheckpointNonce::from_bytes([0x5a; 32]));
        protocol
            .begin_request(1, Duration::from_nanos(1), Duration::from_nanos(2))
            .unwrap();
        let _ = protocol.ingest(Duration::from_nanos(1), &input);
        assert!(protocol.buffered_bytes() <= MAX_CHECKPOINT_INPUT);
    }
    assert!(started.elapsed() < MAX_CASE_RUNTIME);
}

#[test]
fn report_parser_handles_bounded_mutated_and_jsonish_inputs_without_panicking() {
    let started = Instant::now();
    let valid = include_bytes!("fixtures/report-v1.json");
    assert!(valid.len() <= MAX_REPORT_INPUT);
    let mut generator = Generator(0x7265_706f_7274_7631);
    for case in 0..REPORT_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let mut input = if case % 2 == 0 {
            valid.to_vec()
        } else {
            let alphabet = b"{}[],:\"0123456789truefalsenull abcXYZ_-/\\\n\t";
            let length = generator.index(MAX_REPORT_INPUT + 1);
            (0..length)
                .map(|_| alphabet[generator.index(alphabet.len())])
                .collect()
        };
        let mutations = 1 + generator.index(8);
        for _ in 0..mutations {
            if input.is_empty() {
                break;
            }
            let index = generator.index(input.len());
            input[index] = generator.next().to_le_bytes()[0];
        }
        if let Ok(text) = std::str::from_utf8(&input) {
            let _ = ReportV1::from_json(text);
        }
    }
    assert!(started.elapsed() < MAX_CASE_RUNTIME);
}

#[test]
fn journal_recovery_handles_bounded_mutated_records_without_panicking() {
    let started = Instant::now();
    let directory = TestDirectory::new();
    let mut journal = SecureJournal::initialize(&directory.0.join("report.json")).unwrap();
    journal
        .append(
            &JournalRecord::new(
                0,
                JournalEntry::Transition(TransitionRecord {
                    at_ms: 1,
                    from: PolicyState::Normal,
                    to: PolicyState::Warning,
                    aggregate_footprint_bytes: Some(1),
                }),
            ),
            JournalDurability::Sync,
        )
        .unwrap();
    let valid = fs::read(journal.journal_path()).unwrap();
    assert!(valid.len() <= MAX_JOURNAL_INPUT);

    let fuzz_path = directory.0.join("fuzz.journal");
    let mut fuzz_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&fuzz_path)
        .unwrap();
    fuzz_file
        .set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    let mut generator = Generator(0x6a6f_7572_6e61_6c31);
    for case in 0..JOURNAL_CASES {
        assert!(started.elapsed() < MAX_CASE_RUNTIME);
        let mut input = valid.clone();
        if case % 3 == 0 {
            input.truncate(generator.index(input.len() + 1));
        } else if case % 3 == 1 {
            let extra_length = generator.index(512);
            let extra = generator.bytes(extra_length);
            input.extend_from_slice(&extra);
        }
        for _ in 0..=generator.index(8) {
            if input.is_empty() {
                break;
            }
            let index = generator.index(input.len());
            input[index] = generator.next().to_le_bytes()[0];
        }
        input.truncate(MAX_JOURNAL_INPUT);
        fuzz_file.set_len(0).unwrap();
        fuzz_file.write_all(&input).unwrap();
        fuzz_file.sync_data().unwrap();
        let _ = JournalRecovery::read(&fuzz_path).and_then(|recovery| recovery.to_report());
    }
    assert!(started.elapsed() < MAX_CASE_RUNTIME);
}

struct TestDirectory(PathBuf);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-parser-fuzz-{}-{unique}-{}",
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
