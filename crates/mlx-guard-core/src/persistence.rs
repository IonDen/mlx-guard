#![allow(unsafe_code)]

use std::error::Error;
use std::ffi::{CString, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    ArtifactErrorRecord, CalibrationArtifact, Capabilities, CheckpointRecord, EscapeEvidence,
    PrivacyDefaults, ReportConfiguration, ReportV1, RunIdentity, SampleWindow, SignalRecord,
    TerminalKind, TerminalOutcome, TransitionRecord,
};

/// Header for the version-1 binary journal format.
pub const JOURNAL_MAGIC: &[u8] = b"MLXGJNL\x01";
pub const JOURNAL_RECORD_VERSION: u16 = 1;
pub const MAX_JOURNAL_RECORD_BYTES: usize = 64 * 1024;
const MAX_RECOVERED_RECORDS: usize = 65_536;

/// Stable class of a persistence failure without a sensitive path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageErrorKind {
    InvalidPath,
    UnsafeDirectory,
    JournalExists,
    CreateFailed,
    WriteFailed,
    SyncFailed,
    ReadFailed,
    InvalidRecord,
    UnsafeTarget,
    FinalizeFailed,
}

/// Redacted journal or report persistence failure.
#[derive(Debug)]
pub struct StorageError {
    kind: StorageErrorKind,
    source: Option<io::Error>,
}

impl StorageError {
    fn new(kind: StorageErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn from_io(kind: StorageErrorKind, source: io::Error) -> Self {
        Self {
            kind,
            source: Some(source),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> StorageErrorKind {
        self.kind
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            StorageErrorKind::InvalidPath => "artifact target path is invalid",
            StorageErrorKind::UnsafeDirectory => "artifact directory is not private and owned",
            StorageErrorKind::JournalExists => "journal target already exists",
            StorageErrorKind::CreateFailed => "journal target creation failed",
            StorageErrorKind::WriteFailed => "artifact write failed",
            StorageErrorKind::SyncFailed => "artifact synchronization failed",
            StorageErrorKind::ReadFailed => "artifact read failed",
            StorageErrorKind::InvalidRecord => "journal record is invalid",
            StorageErrorKind::UnsafeTarget => "artifact output target is unsafe",
            StorageErrorKind::FinalizeFailed => "final report creation failed",
        })
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Securely initialized same-directory journal and final-report target.
pub struct SecureJournal {
    directory: File,
    file: File,
    report_path: PathBuf,
    journal_path: PathBuf,
    report_name: OsString,
    journal_name: OsString,
    next_sequence: u64,
    failed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalDurability {
    Buffered,
    Sync,
}

/// Minimal append boundary used to isolate persistence failure from safety enforcement.
pub trait JournalAppender {
    /// Append one record, returning only the redacted stable failure class.
    ///
    /// # Errors
    ///
    /// Returns the stable failure class when the record cannot be written or synchronized.
    fn append_record(
        &mut self,
        record: &JournalRecord,
        durability: JournalDurability,
    ) -> Result<(), StorageErrorKind>;
}

/// Observable result of a best-effort journal append after secure pre-launch initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceAttempt {
    Persisted,
    Failed(StorageErrorKind),
    Disabled(StorageErrorKind),
}

/// Latches the first late storage failure so enforcement callers can continue without retrying it.
#[derive(Debug)]
pub struct ResilientJournal<J> {
    journal: Option<J>,
    failure: Option<StorageErrorKind>,
}

impl<J: JournalAppender> ResilientJournal<J> {
    #[must_use]
    pub const fn new(journal: J) -> Self {
        Self {
            journal: Some(journal),
            failure: None,
        }
    }

    /// Attempt one append, permanently disabling persistence after the first failure.
    pub fn record(
        &mut self,
        record: &JournalRecord,
        durability: JournalDurability,
    ) -> PersistenceAttempt {
        let Some(journal) = self.journal.as_mut() else {
            return PersistenceAttempt::Disabled(
                self.failure.unwrap_or(StorageErrorKind::WriteFailed),
            );
        };
        match journal.append_record(record, durability) {
            Ok(()) => PersistenceAttempt::Persisted,
            Err(kind) => {
                self.failure = Some(kind);
                self.journal = None;
                PersistenceAttempt::Failed(kind)
            }
        }
    }

    #[must_use]
    pub const fn failure_kind(&self) -> Option<StorageErrorKind> {
        self.failure
    }

    #[must_use]
    pub const fn supervisor_outcome(&self) -> Option<crate::SupervisorOutcome> {
        if self.failure.is_some() {
            Some(crate::SupervisorOutcome::PartialArtifactFailure)
        } else {
            None
        }
    }

    /// Return a redacted stderr diagnostic suitable for a late persistence failure.
    #[must_use]
    pub const fn stderr_notice(&self) -> Option<&'static str> {
        match self.failure {
            Some(StorageErrorKind::WriteFailed) => Some(
                "mlx-guard: artifact write failed; safety enforcement continues; complete report unavailable",
            ),
            Some(StorageErrorKind::SyncFailed) => Some(
                "mlx-guard: artifact synchronization failed; safety enforcement continues; complete report unavailable",
            ),
            Some(_) => Some(
                "mlx-guard: artifact persistence failed; safety enforcement continues; complete report unavailable",
            ),
            None => None,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> Option<J> {
        self.journal
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum JournalEntry {
    Header(Box<JournalHeader>),
    Sample(Box<SampleWindow>),
    SampleHistoryReset,
    Transition(TransitionRecord),
    Signal(SignalRecord),
    Checkpoint(CheckpointRecord),
    Escape(EscapeEvidence),
    ArtifactError(ArtifactErrorRecord),
    Calibration(Box<CalibrationArtifact>),
    Outcome(TerminalOutcome),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalHeader {
    pub schema_version: u32,
    pub package_version: String,
    pub run: RunIdentity,
    pub capabilities: Capabilities,
    pub configuration: ReportConfiguration,
    pub privacy: PrivacyDefaults,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalRecord {
    pub version: u16,
    pub sequence: u64,
    pub entry: JournalEntry,
}

impl JournalRecord {
    #[must_use]
    pub const fn new(sequence: u64, entry: JournalEntry) -> Self {
        Self {
            version: JOURNAL_RECORD_VERSION,
            sequence,
            entry,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalRecoveryStatus {
    Complete,
    Truncated,
    Corrupt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecovery {
    pub records: Vec<JournalRecord>,
    pub status: JournalRecoveryStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedArtifacts {
    pub report: ReportV1,
    pub summary: String,
}

impl fmt::Debug for SecureJournal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecureJournal { redacted_paths: true }")
    }
}

impl SecureJournal {
    /// Create and durably initialize an exclusive owner-only journal before worker launch.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for an invalid target, unsafe directory, existing journal, or failed
    /// create/write/sync operation.
    pub fn initialize(report_path: &Path) -> Result<Self, StorageError> {
        validate_report_target(report_path)?;
        let parent = report_path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = report_path
            .file_name()
            .ok_or_else(|| StorageError::new(StorageErrorKind::InvalidPath))?;
        let journal_name = artifact_name(file_name, b".", b".journal")?;
        let journal_path = parent.join(&journal_name);

        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|error| StorageError::from_io(StorageErrorKind::UnsafeDirectory, error))?;
        validate_directory(&directory)?;
        validate_existing_output_at(&directory, file_name)?;

        let name = CString::new(journal_name.as_bytes())
            .map_err(|_| StorageError::new(StorageErrorKind::InvalidPath))?;
        // SAFETY: `directory` is an open validated directory, `name` is a single NUL-free component,
        // and the returned descriptor is checked before being owned by `File`.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            let error = io::Error::last_os_error();
            let kind = if error.raw_os_error() == Some(libc::EEXIST) {
                StorageErrorKind::JournalExists
            } else {
                StorageErrorKind::CreateFailed
            };
            return Err(StorageError::from_io(kind, error));
        }
        // SAFETY: `openat` returned a new owned descriptor exactly once.
        let mut file = unsafe { File::from_raw_fd(descriptor) };
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| StorageError::from_io(StorageErrorKind::CreateFailed, error))?;
        file.write_all(JOURNAL_MAGIC)
            .map_err(|error| StorageError::from_io(StorageErrorKind::WriteFailed, error))?;
        file.sync_all()
            .map_err(|error| StorageError::from_io(StorageErrorKind::SyncFailed, error))?;
        directory
            .sync_all()
            .map_err(|error| StorageError::from_io(StorageErrorKind::SyncFailed, error))?;

        Ok(Self {
            directory,
            file,
            report_path: report_path.to_path_buf(),
            journal_path,
            report_name: file_name.to_os_string(),
            journal_name,
            next_sequence: 0,
            failed: false,
        })
    }

    /// Append one bounded, versioned, checksummed record.
    ///
    /// Headers, transitions, signals, checkpoint states, and outcomes require
    /// [`JournalDurability::Sync`]. Callers must finish the decisive transition append before
    /// executing its related signal.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid ordering, serialization, write, or synchronization failure.
    pub fn append(
        &mut self,
        record: &JournalRecord,
        durability: JournalDurability,
    ) -> Result<(), StorageError> {
        if self.failed
            || record.version != JOURNAL_RECORD_VERSION
            || record.sequence != self.next_sequence
            || (requires_sync(&record.entry) && durability != JournalDurability::Sync)
        {
            return Err(StorageError::new(StorageErrorKind::InvalidRecord));
        }
        let payload = serde_json::to_vec(record)
            .map_err(|_| StorageError::new(StorageErrorKind::InvalidRecord))?;
        let length = u32::try_from(payload.len())
            .ok()
            .filter(|length| {
                usize::try_from(*length)
                    .ok()
                    .is_some_and(|v| v <= MAX_JOURNAL_RECORD_BYTES)
            })
            .ok_or_else(|| StorageError::new(StorageErrorKind::InvalidRecord))?;
        let checksum = crc32(&payload);
        if let Err(error) = self
            .file
            .write_all(&length.to_be_bytes())
            .and_then(|()| self.file.write_all(&payload))
            .and_then(|()| self.file.write_all(&checksum.to_be_bytes()))
        {
            self.failed = true;
            return Err(StorageError::from_io(StorageErrorKind::WriteFailed, error));
        }
        if durability == JournalDurability::Sync
            && let Err(error) = self.file.sync_all()
        {
            self.failed = true;
            return Err(StorageError::from_io(StorageErrorKind::SyncFailed, error));
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(())
    }

    /// Synchronize buffered records.
    ///
    /// # Errors
    ///
    /// Returns a typed synchronization failure and poisons later writes.
    pub fn flush(&mut self) -> Result<(), StorageError> {
        if self.failed {
            return Err(StorageError::new(StorageErrorKind::SyncFailed));
        }
        if let Err(error) = self.file.sync_all() {
            self.failed = true;
            return Err(StorageError::from_io(StorageErrorKind::SyncFailed, error));
        }
        Ok(())
    }

    /// Append a validated report as bounded component records, syncing the terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, write, or synchronization failure.
    pub fn append_report(&mut self, report: &ReportV1) -> Result<(), StorageError> {
        report
            .validate()
            .map_err(|_| StorageError::new(StorageErrorKind::InvalidRecord))?;
        if self.next_sequence != 0 {
            return Err(StorageError::new(StorageErrorKind::InvalidRecord));
        }
        let header = JournalHeader {
            schema_version: report.schema_version,
            package_version: report.package_version.clone(),
            run: report.run.clone(),
            capabilities: report.capabilities.clone(),
            configuration: report.configuration.clone(),
            privacy: report.privacy.clone(),
        };
        self.append_next(
            JournalEntry::Header(Box::new(header)),
            JournalDurability::Sync,
        )?;
        for sample in &report.samples {
            self.append_next(
                JournalEntry::Sample(Box::new(sample.clone())),
                JournalDurability::Buffered,
            )?;
        }
        for transition in &report.transitions {
            self.append_next(
                JournalEntry::Transition(transition.clone()),
                JournalDurability::Sync,
            )?;
        }
        for signal in &report.signals {
            self.append_next(
                JournalEntry::Signal(signal.clone()),
                JournalDurability::Sync,
            )?;
        }
        self.append_next(
            JournalEntry::Checkpoint(report.checkpoint.clone()),
            JournalDurability::Sync,
        )?;
        self.append_next(
            JournalEntry::Escape(report.escape.clone()),
            JournalDurability::Buffered,
        )?;
        for error in &report.artifact_errors {
            self.append_next(
                JournalEntry::ArtifactError(error.clone()),
                JournalDurability::Buffered,
            )?;
        }
        if let Some(calibration) = &report.calibration {
            self.append_next(
                JournalEntry::Calibration(Box::new(calibration.clone())),
                JournalDurability::Buffered,
            )?;
        }
        self.append_next(
            JournalEntry::Outcome(report.outcome.clone()),
            JournalDurability::Sync,
        )
    }

    fn append_next(
        &mut self,
        entry: JournalEntry,
        durability: JournalDurability,
    ) -> Result<(), StorageError> {
        self.append(&JournalRecord::new(self.next_sequence, entry), durability)
    }

    /// Replay the journal and atomically replace the final schema-v1 JSON file.
    ///
    /// # Errors
    ///
    /// Returns a typed recovery, target-safety, write, sync, or rename failure.
    pub fn finalize(&mut self) -> Result<FinalizedArtifacts, StorageError> {
        self.flush()?;
        let report = read_journal_at(&self.directory, &self.journal_name)?.to_report()?;
        let encoded = report
            .to_json_pretty()
            .map_err(|_| StorageError::new(StorageErrorKind::InvalidRecord))?;
        validate_existing_output_at(&self.directory, &self.report_name)?;
        let temp_name = artifact_name(&self.report_name, b".", b".tmp")?;
        let mut temp = create_at(&self.directory, &temp_name)?;
        if let Err(error) = temp
            .set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| temp.write_all(encoded.as_bytes()))
            .and_then(|()| temp.sync_all())
        {
            drop(temp);
            unlink_at(&self.directory, &temp_name);
            return Err(StorageError::from_io(
                StorageErrorKind::FinalizeFailed,
                error,
            ));
        }
        drop(temp);
        if let Err(error) = rename_at(
            &self.directory,
            &temp_name,
            &self.directory,
            &self.report_name,
        ) {
            unlink_at(&self.directory, &temp_name);
            return Err(StorageError::from_io(
                StorageErrorKind::FinalizeFailed,
                error,
            ));
        }
        self.directory
            .sync_all()
            .map_err(|error| StorageError::from_io(StorageErrorKind::SyncFailed, error))?;
        Ok(FinalizedArtifacts {
            summary: report_summary(&report),
            report,
        })
    }

    #[must_use]
    pub fn report_path(&self) -> &Path {
        &self.report_path
    }

    #[must_use]
    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }
}

impl JournalAppender for SecureJournal {
    fn append_record(
        &mut self,
        record: &JournalRecord,
        durability: JournalDurability,
    ) -> Result<(), StorageErrorKind> {
        self.append(record, durability)
            .map_err(|error| error.kind())
    }
}

impl JournalRecovery {
    /// Recover the valid record prefix from an owner-only, no-follow journal file.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the file cannot be safely opened or its header is invalid.
    pub fn read(path: &Path) -> Result<Self, StorageError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|error| StorageError::from_io(StorageErrorKind::ReadFailed, error))?;
        Self::read_file(file)
    }

    fn read_file(mut file: File) -> Result<Self, StorageError> {
        let metadata = file
            .metadata()
            .map_err(|error| StorageError::from_io(StorageErrorKind::ReadFailed, error))?;
        // SAFETY: geteuid has no preconditions.
        let effective_uid = unsafe { libc::geteuid() };
        if !metadata.is_file()
            || metadata.uid() != effective_uid
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(StorageError::new(StorageErrorKind::InvalidRecord));
        }
        let mut magic = vec![0_u8; JOURNAL_MAGIC.len()];
        file.read_exact(&mut magic)
            .map_err(|error| StorageError::from_io(StorageErrorKind::ReadFailed, error))?;
        if magic != JOURNAL_MAGIC {
            return Err(StorageError::new(StorageErrorKind::InvalidRecord));
        }

        let mut records = Vec::new();
        loop {
            let mut length_bytes = [0_u8; 4];
            let first = file
                .read(&mut length_bytes)
                .map_err(|error| StorageError::from_io(StorageErrorKind::ReadFailed, error))?;
            if first == 0 {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Complete,
                });
            }
            if first < length_bytes.len() && !read_remaining(&mut file, &mut length_bytes[first..])?
            {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Truncated,
                });
            }
            let length = u32::from_be_bytes(length_bytes) as usize;
            if length > MAX_JOURNAL_RECORD_BYTES || records.len() == MAX_RECOVERED_RECORDS {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Corrupt,
                });
            }
            let mut payload = vec![0_u8; length];
            let mut checksum = [0_u8; 4];
            if !read_remaining(&mut file, &mut payload)?
                || !read_remaining(&mut file, &mut checksum)?
            {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Truncated,
                });
            }
            if crc32(&payload) != u32::from_be_bytes(checksum) {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Corrupt,
                });
            }
            let Ok(record) = serde_json::from_slice::<JournalRecord>(&payload) else {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Corrupt,
                });
            };
            if record.version != JOURNAL_RECORD_VERSION || record.sequence != records.len() as u64 {
                return Ok(Self {
                    records,
                    status: JournalRecoveryStatus::Corrupt,
                });
            }
            records.push(record);
        }
    }

    /// Replay a complete valid component sequence into schema-v1.
    ///
    /// # Errors
    ///
    /// Returns a typed error when required components are missing, duplicated, or invalid.
    pub fn to_report(&self) -> Result<ReportV1, StorageError> {
        if self.status != JournalRecoveryStatus::Complete {
            return Err(StorageError::new(StorageErrorKind::InvalidRecord));
        }
        let mut header = None;
        let mut samples = Vec::new();
        let mut sample_history_reset = false;
        let mut transitions = Vec::new();
        let mut signals = Vec::new();
        let mut checkpoint = None;
        let mut escape = None;
        let mut artifact_errors = Vec::new();
        let mut outcome = None;
        let mut calibration = None;
        for (index, record) in self.records.iter().enumerate() {
            match &record.entry {
                JournalEntry::Header(value) if header.is_none() && index == 0 => {
                    header = Some((**value).clone());
                }
                JournalEntry::Sample(value) => samples.push((**value).clone()),
                JournalEntry::SampleHistoryReset if !sample_history_reset => {
                    samples.clear();
                    sample_history_reset = true;
                }
                JournalEntry::Transition(value) => transitions.push(value.clone()),
                JournalEntry::Signal(value) => signals.push(value.clone()),
                JournalEntry::Checkpoint(value) if checkpoint.is_none() => {
                    checkpoint = Some(value.clone());
                }
                JournalEntry::Escape(value) if escape.is_none() => escape = Some(value.clone()),
                JournalEntry::ArtifactError(value) => artifact_errors.push(value.clone()),
                JournalEntry::Calibration(value) if calibration.is_none() => {
                    calibration = Some((**value).clone());
                }
                JournalEntry::Outcome(value)
                    if outcome.is_none() && index + 1 == self.records.len() =>
                {
                    outcome = Some(value.clone());
                }
                JournalEntry::Header(_)
                | JournalEntry::SampleHistoryReset
                | JournalEntry::Calibration(_)
                | JournalEntry::Checkpoint(_)
                | JournalEntry::Escape(_)
                | JournalEntry::Outcome(_) => {
                    return Err(StorageError::new(StorageErrorKind::InvalidRecord));
                }
            }
        }
        let header = header.ok_or_else(|| StorageError::new(StorageErrorKind::InvalidRecord))?;
        let report = ReportV1 {
            schema_version: header.schema_version,
            package_version: header.package_version,
            run: header.run,
            capabilities: header.capabilities,
            configuration: header.configuration,
            samples,
            transitions,
            signals,
            checkpoint: checkpoint
                .ok_or_else(|| StorageError::new(StorageErrorKind::InvalidRecord))?,
            escape: escape.ok_or_else(|| StorageError::new(StorageErrorKind::InvalidRecord))?,
            artifact_errors,
            outcome: outcome.ok_or_else(|| StorageError::new(StorageErrorKind::InvalidRecord))?,
            calibration,
            privacy: header.privacy,
        };
        report
            .validate()
            .map_err(|_| StorageError::new(StorageErrorKind::InvalidRecord))?;
        Ok(report)
    }
}

const fn requires_sync(entry: &JournalEntry) -> bool {
    matches!(
        entry,
        JournalEntry::Header(_)
            | JournalEntry::SampleHistoryReset
            | JournalEntry::Transition(_)
            | JournalEntry::Signal(_)
            | JournalEntry::Checkpoint(_)
            | JournalEntry::Outcome(_)
    )
}

fn validate_existing_output_at(
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<(), StorageError> {
    let name = c_name(name)?;
    // SAFETY: the directory descriptor and component name are valid; the output is initialized.
    let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: pointers refer to live values and AT_SYMLINK_NOFOLLOW prevents target traversal.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &raw mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(StorageError::from_io(StorageErrorKind::UnsafeTarget, error));
    }
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
        || metadata.st_uid != uid
        || metadata.st_mode & 0o777 != 0o600
    {
        return Err(StorageError::new(StorageErrorKind::UnsafeTarget));
    }
    Ok(())
}

fn read_journal_at(
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<JournalRecovery, StorageError> {
    let name = c_name(name)?;
    // SAFETY: the descriptor is a validated directory and name is one NUL-free component.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(StorageError::from_io(
            StorageErrorKind::ReadFailed,
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: openat returned a new owned descriptor exactly once.
    JournalRecovery::read_file(unsafe { File::from_raw_fd(descriptor) })
}

fn create_at(directory: &File, name: &std::ffi::OsStr) -> Result<File, StorageError> {
    let name = c_name(name)?;
    // SAFETY: the descriptor is a validated directory and name is one NUL-free component.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        let kind = if error.raw_os_error() == Some(libc::EEXIST) {
            StorageErrorKind::UnsafeTarget
        } else {
            StorageErrorKind::FinalizeFailed
        };
        return Err(StorageError::from_io(kind, error));
    }
    // SAFETY: openat returned a new owned descriptor exactly once.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn rename_at(
    source_directory: &File,
    source: &std::ffi::OsStr,
    target_directory: &File,
    target: &std::ffi::OsStr,
) -> io::Result<()> {
    let source = CString::new(source.as_bytes())?;
    let target = CString::new(target.as_bytes())?;
    // SAFETY: both descriptors and component-name pointers remain valid for the call.
    let result = unsafe {
        libc::renameat(
            source_directory.as_raw_fd(),
            source.as_ptr(),
            target_directory.as_raw_fd(),
            target.as_ptr(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn unlink_at(directory: &File, name: &std::ffi::OsStr) {
    let Ok(name) = CString::new(name.as_bytes()) else {
        return;
    };
    // SAFETY: the descriptor and component-name pointer remain valid for the call.
    unsafe {
        libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
    }
}

fn c_name(name: &std::ffi::OsStr) -> Result<CString, StorageError> {
    CString::new(name.as_bytes()).map_err(|_| StorageError::new(StorageErrorKind::InvalidPath))
}

fn report_summary(report: &ReportV1) -> String {
    let outcome = match report.outcome.kind {
        TerminalKind::ChildExited { .. } => "child_exited",
        TerminalKind::ChildSignaled { .. } => "child_signaled",
        TerminalKind::LaunchNotFound => "launch_not_found",
        TerminalKind::LaunchNotExecutable => "launch_not_executable",
        TerminalKind::InvalidConfiguration => "invalid_configuration",
        TerminalKind::PolicyIntervention => "policy_intervention",
        TerminalKind::SupervisorFailure => "supervisor_failure",
        TerminalKind::PartialArtifactFailure => "partial_artifact_failure",
    };
    format!(
        "mlx-guard: {outcome} at {}ms; {} sample{}, {} signal{}\n",
        report.outcome.at_ms,
        report.samples.len(),
        if report.samples.len() == 1 { "" } else { "s" },
        report.signals.len(),
        if report.signals.len() == 1 { "" } else { "s" }
    )
}

fn read_remaining(file: &mut File, buffer: &mut [u8]) -> Result<bool, StorageError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => return Ok(false),
            Ok(count) => filled += count,
            Err(error) => return Err(StorageError::from_io(StorageErrorKind::ReadFailed, error)),
        }
    }
    Ok(true)
}

fn crc32(bytes: &[u8]) -> u32 {
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

fn validate_report_target(report_path: &Path) -> Result<(), StorageError> {
    let Some(file_name) = report_path.file_name() else {
        return Err(StorageError::new(StorageErrorKind::InvalidPath));
    };
    if file_name.as_bytes().is_empty() || matches!(file_name.as_bytes(), b"." | b"..") {
        return Err(StorageError::new(StorageErrorKind::InvalidPath));
    }
    if let Ok(metadata) = fs::symlink_metadata(report_path)
        && !metadata.file_type().is_file()
    {
        return Err(StorageError::new(StorageErrorKind::InvalidPath));
    }
    Ok(())
}

fn validate_directory(directory: &File) -> Result<(), StorageError> {
    let metadata = directory
        .metadata()
        .map_err(|error| StorageError::from_io(StorageErrorKind::UnsafeDirectory, error))?;
    // SAFETY: geteuid has no preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    let mode = metadata.mode() & 0o777;
    if !metadata.is_dir() || metadata.uid() != effective_uid || mode != 0o700 {
        return Err(StorageError::new(StorageErrorKind::UnsafeDirectory));
    }
    Ok(())
}

fn artifact_name(
    file_name: &std::ffi::OsStr,
    prefix: &[u8],
    suffix: &[u8],
) -> Result<OsString, StorageError> {
    let bytes = file_name.as_bytes();
    let capacity = prefix
        .len()
        .checked_add(bytes.len())
        .and_then(|value| value.checked_add(suffix.len()))
        .ok_or_else(|| StorageError::new(StorageErrorKind::InvalidPath))?;
    let mut name = Vec::with_capacity(capacity);
    name.extend_from_slice(prefix);
    name.extend_from_slice(bytes);
    name.extend_from_slice(suffix);
    Ok(OsString::from_vec(name))
}
