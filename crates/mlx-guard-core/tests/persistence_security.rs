#![cfg(unix)]
#![allow(unsafe_code)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mlx_guard_core::{JOURNAL_MAGIC, SecureJournal, StorageErrorKind};

struct TestDirectory(PathBuf);

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new(mode: u32) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mlx-guard-persistence-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path)
    }

    fn report(&self) -> PathBuf {
        self.0.join("report.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn secure_initialization_creates_and_syncs_an_exclusive_owner_only_journal() {
    // Catches delayed journal creation, permissive mode, or a target outside the report directory.
    let directory = TestDirectory::new(0o700);
    let report = directory.report();
    let journal = SecureJournal::initialize(&report).unwrap();

    assert_eq!(journal.report_path(), report);
    assert_eq!(journal.journal_path().parent(), report.parent());
    let metadata = fs::metadata(journal.journal_path()).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(fs::read(journal.journal_path()).unwrap(), JOURNAL_MAGIC);
}

#[test]
fn existing_journal_is_never_overwritten_or_truncated() {
    // Catches create-and-truncate behavior that destroys evidence from a prior interrupted run.
    let directory = TestDirectory::new(0o700);
    let journal_path = directory.0.join(".report.json.journal");
    fs::write(&journal_path, b"prior evidence").unwrap();
    fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600)).unwrap();

    let error = SecureJournal::initialize(&directory.report()).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::JournalExists);
    assert_eq!(fs::read(journal_path).unwrap(), b"prior evidence");
}

#[test]
fn symlinked_or_non_private_parent_directory_is_rejected() {
    // Catches path redirection and storage in a directory readable by other local users.
    let real = TestDirectory::new(0o700);
    let links = TestDirectory::new(0o700);
    let linked_parent = links.0.join("redirect");
    symlink(&real.0, &linked_parent).unwrap();
    let error = SecureJournal::initialize(&linked_parent.join("report.json")).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnsafeDirectory);

    let public = TestDirectory::new(0o755);
    let error = SecureJournal::initialize(&public.report()).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnsafeDirectory);
}

#[test]
fn invalid_report_target_is_rejected_without_creating_any_file() {
    // Catches accepting a directory itself or a path without a normal final component.
    let directory = TestDirectory::new(0o700);
    let error = SecureJournal::initialize(&directory.0).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::InvalidPath);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn unsafe_existing_report_is_rejected_before_journal_creation() {
    // Catches launching work when the final target is already exposed to other local users.
    let directory = TestDirectory::new(0o700);
    let report = directory.report();
    fs::write(&report, b"old report").unwrap();
    fs::set_permissions(&report, fs::Permissions::from_mode(0o644)).unwrap();

    let error = SecureJournal::initialize(&report).unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::UnsafeTarget);
    assert_eq!(fs::read(&report).unwrap(), b"old report");
    assert!(!directory.0.join(".report.json.journal").exists());
}

#[test]
fn existing_journal_symlink_is_never_followed_or_removed() {
    // Catches exclusive-create handling that follows or cleans up an attacker-controlled link.
    let directory = TestDirectory::new(0o700);
    let outside = directory.0.join("outside");
    fs::write(&outside, b"unchanged").unwrap();
    let journal_path = directory.0.join(".report.json.journal");
    symlink(&outside, &journal_path).unwrap();

    let error = SecureJournal::initialize(&directory.report()).unwrap_err();

    assert_eq!(error.kind(), StorageErrorKind::JournalExists);
    assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
    assert!(fs::symlink_metadata(journal_path).unwrap().is_symlink());
}
