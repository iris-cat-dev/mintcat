use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt as _;
use mintcat_runtime_artifacts::{ArtifactError, ArtifactGrant};
use mintcat_runtime_kernel::{KernelDatabase, KernelDatabaseError};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

mod uninstall;

const MIGRATION_OWNER: &str = "runtime.file-transactions";
const TARGET_TRANSACTION_DIR: &str = ".mintcat-transactions";
const COPY_BUFFER_BYTES: usize = 64 * 1024;

const MIGRATION: &str = r#"
CREATE TABLE file_transactions (
  transaction_id TEXT PRIMARY KEY,
  installation_id TEXT NOT NULL,
  owner_scope TEXT NOT NULL,
  root_identity TEXT NOT NULL,
  root_path TEXT NOT NULL,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  journal_json TEXT NOT NULL,
  receipt_json TEXT,
  sequence INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX file_transactions_active_root
  ON file_transactions(root_identity)
  WHERE state IN ('prepared', 'applying', 'recovery-required');
CREATE INDEX file_transactions_installation
  ON file_transactions(installation_id, created_at_ms DESC);

CREATE TABLE install_receipts (
  receipt_id TEXT PRIMARY KEY,
  installation_id TEXT NOT NULL,
  owner_scope TEXT NOT NULL,
  root_identity TEXT NOT NULL,
  state TEXT NOT NULL,
  receipt_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX install_receipts_installation
  ON install_receipts(installation_id, created_at_ms DESC);
"#;

#[derive(Debug, Error)]
pub enum FileTransactionError {
    #[error(transparent)]
    Kernel(#[from] KernelDatabaseError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("file transaction I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid relative deployment path: {0}")]
    InvalidPath(String),
    #[error("deployment target appears more than once: {0}")]
    DuplicateTarget(String),
    #[error("installation root is already owned by transaction {0}")]
    RootBusy(String),
    #[error("installation target is running: {0}")]
    TargetRunning(String),
    #[error("target {path} does not match the planned state: {reason}")]
    TargetMismatch { path: String, reason: String },
    #[error("staging is not on the installation filesystem")]
    CrossVolume,
    #[error("insufficient free space: need {required} bytes, have {available}")]
    InsufficientSpace { required: u64, available: u64 },
    #[error("file transaction {transaction_id} rolled back after commit failure: {reason}")]
    CommitRolledBack {
        transaction_id: String,
        reason: String,
    },
    #[error("file transaction {transaction_id} requires recovery: {reason}")]
    RecoveryRequired {
        transaction_id: String,
        reason: String,
    },
    #[error("file transaction was not found: {0}")]
    TransactionNotFound(String),
    #[error("install receipt was not found: {0}")]
    ReceiptNotFound(String),
    #[error("install receipt is not active: {0}")]
    ReceiptInactive(String),
    #[error("invalid file transaction request: {0}")]
    InvalidRequest(String),
    #[cfg(test)]
    #[error("simulated process termination")]
    SimulatedCrash,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExpectedTarget {
    Absent,
    Digest(String),
}

pub struct DeployFile<'a> {
    pub relative_path: String,
    pub artifact: &'a ArtifactGrant,
    pub expected_target: ExpectedTarget,
    pub owner: String,
    pub install_mode: Option<u32>,
}

pub struct DeployRequest<'a> {
    pub installation_id: String,
    pub owner_scope: String,
    pub root: PathBuf,
    pub game_fingerprint: String,
    pub profile_revision: u64,
    pub plan_hash: String,
    pub module_lock_digest: String,
    pub files: Vec<DeployFile<'a>>,
}

pub trait TargetProcessGuard: Send + Sync {
    fn ensure_stopped(&self, root: &Path) -> Result<(), String>;
}

impl<F> TargetProcessGuard for F
where
    F: Fn(&Path) -> Result<(), String> + Send + Sync,
{
    fn ensure_stopped(&self, root: &Path) -> Result<(), String> {
        self(root)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileCancellation {
    requested: Arc<AtomicBool>,
}

impl FileCancellation {
    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FileTransactionState {
    Prepared,
    Applying,
    Committed,
    RolledBack,
    RecoveryRequired,
}

impl FileTransactionState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Applying => "applying",
            Self::Committed => "committed",
            Self::RolledBack => "rolled-back",
            Self::RecoveryRequired => "recovery-required",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum FileTransactionKind {
    Install,
}

impl FileTransactionKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Install => "install",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum EntryState {
    Prepared,
    BackupSaved,
    Replaced,
    Verified,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JournalEntry {
    relative_path: String,
    owner: String,
    before_digest: Option<String>,
    after_digest: String,
    after_size: u64,
    staged_relative_path: String,
    backup_relative_path: Option<String>,
    original_mode: Option<u32>,
    install_mode: Option<u32>,
    created_parent_dirs: Vec<String>,
    state: EntryState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileTransactionJournal {
    schema_version: u32,
    transaction_id: String,
    receipt_id: String,
    installation_id: String,
    owner_scope: String,
    root_path: String,
    root_identity: String,
    game_fingerprint: String,
    profile_revision: u64,
    plan_hash: String,
    module_lock_digest: String,
    kind: FileTransactionKind,
    state: FileTransactionState,
    sequence: u64,
    entries: Vec<JournalEntry>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptFile {
    pub relative_path: String,
    pub owner: String,
    pub before_digest: Option<String>,
    pub installed_digest: String,
    pub original_mode: Option<u32>,
    pub installed_mode: Option<u32>,
    pub backup_relative_path: Option<String>,
    pub created_parent_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallReceipt {
    pub schema_version: u32,
    pub receipt_id: String,
    pub transaction_id: String,
    pub installation_id: String,
    pub owner_scope: String,
    pub root_path: String,
    pub root_identity: String,
    pub game_fingerprint: String,
    pub profile_revision: u64,
    pub plan_hash: String,
    pub module_lock_digest: String,
    pub files: Vec<ReceiptFile>,
    pub committed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitDisposition {
    Committed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub transaction_id: String,
    pub disposition: CommitDisposition,
    pub receipt: Option<InstallReceipt>,
    pub cancel_requested_during_commit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDisposition {
    RolledBack,
    RecoveredCommitted,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub transaction_id: String,
    pub disposition: RecoveryDisposition,
    pub detail: String,
    pub receipt: Option<InstallReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UninstallDisposition {
    Cancelled,
    Completed,
    CompletedWithPreservedFiles,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UninstallChainReport {
    pub processed_receipt_ids: Vec<String>,
    pub disposition: UninstallDisposition,
    pub restored: Vec<String>,
    pub removed: Vec<String>,
    pub preserved_modified: Vec<String>,
    pub missing: Vec<String>,
    pub foreign_files_untouched: bool,
    pub cancel_requested_during_commit: bool,
}

#[derive(Debug)]
pub struct PreparedTransaction {
    journal: FileTransactionJournal,
    _root_lock: File,
}

impl PreparedTransaction {
    pub fn transaction_id(&self) -> &str {
        &self.journal.transaction_id
    }

    pub fn root_identity(&self) -> &str {
        &self.journal.root_identity
    }

    pub fn state(&self) -> &FileTransactionState {
        &self.journal.state
    }
}

struct PendingArea {
    path: PathBuf,
    armed: bool,
}

impl PendingArea {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingArea {
    fn drop(&mut self) {
        if self.armed {
            cleanup_dir(&self.path);
        }
    }
}

#[derive(Clone)]
pub struct FileTransactionManager {
    database: KernelDatabase,
    locks: PathBuf,
}

impl FileTransactionManager {
    pub fn open(
        database: KernelDatabase,
        state_root: impl Into<PathBuf>,
    ) -> Result<Self, FileTransactionError> {
        database.apply_migration(MIGRATION_OWNER, 1, MIGRATION)?;
        let state_root = state_root.into();
        let locks = state_root.join("locks");
        create_dir_all(&locks)?;
        Ok(Self { database, locks })
    }

    pub fn prepare(
        &self,
        request: DeployRequest<'_>,
        process_guard: &dyn TargetProcessGuard,
    ) -> Result<PreparedTransaction, FileTransactionError> {
        validate_request(&request)?;
        let root = canonical_directory(&request.root)?;
        let root_identity = physical_root_identity(&root)?;
        let root_lock = self.acquire_root_lock(&root_identity)?;
        self.ensure_no_active_transaction(&root_identity)?;
        process_guard
            .ensure_stopped(&root)
            .map_err(FileTransactionError::TargetRunning)?;

        let transaction_id = Uuid::new_v4().to_string();
        let receipt_id = transaction_id.clone();
        let target_area = root.join(TARGET_TRANSACTION_DIR).join(&transaction_id);
        let mut pending_area = PendingArea::new(target_area.clone());
        let staged_root = target_area.join("staged");
        let backup_root = target_area.join("backups");
        create_dir_all(&staged_root)?;
        create_dir_all(&backup_root)?;
        ensure_same_filesystem(&root, &target_area)?;

        let mut seen = HashSet::with_capacity(request.files.len());
        let mut required_bytes = 0_u64;
        for file in &request.files {
            let relative = checked_relative_path(&file.relative_path)?;
            let key = path_identity_key(&relative);
            if !seen.insert(key) {
                return Err(FileTransactionError::DuplicateTarget(
                    file.relative_path.clone(),
                ));
            }
            let target = root.join(&relative);
            validate_existing_ancestors(&root, &relative)?;
            let old_size = target_metadata(&target)?.map_or(0, |metadata| metadata.len());
            required_bytes = required_bytes
                .checked_add(file.artifact.descriptor.size)
                .and_then(|value| value.checked_add(old_size))
                .ok_or_else(|| {
                    FileTransactionError::InvalidRequest("required byte count overflow".to_owned())
                })?;
        }
        let available = fs2::available_space(&root).map_err(|source| io_error(&root, source))?;
        if available < required_bytes {
            return Err(FileTransactionError::InsufficientSpace {
                required: required_bytes,
                available,
            });
        }

        let now = now_ms()?;
        let mut entries = Vec::with_capacity(request.files.len());
        for (index, file) in request.files.into_iter().enumerate() {
            let relative = checked_relative_path(&file.relative_path)?;
            let target = root.join(&relative);
            let (before_digest, original_mode) =
                validate_expected_target(&target, &relative, &file.expected_target)?;
            let staged_relative = format!("staged/{index:08}");
            let staged = target_area.join(&staged_relative);
            let mut input = file.artifact.open_read()?;
            let (digest, size) = copy_and_hash(&mut input, &staged)?;
            if digest != file.artifact.descriptor.digest || size != file.artifact.descriptor.size {
                return Err(FileTransactionError::TargetMismatch {
                    path: file.artifact.descriptor.path.clone(),
                    reason: "artifact grant bytes changed".to_owned(),
                });
            }
            let backup_relative = before_digest
                .as_ref()
                .map(|_| format!("backups/{index:08}"));
            entries.push(JournalEntry {
                relative_path: relative_path_string(&relative),
                owner: file.owner,
                before_digest,
                after_digest: digest,
                after_size: size,
                staged_relative_path: staged_relative,
                backup_relative_path: backup_relative,
                original_mode,
                install_mode: file.install_mode,
                created_parent_dirs: missing_parent_dirs(&root, &relative)?,
                state: EntryState::Prepared,
            });
        }
        sync_directory(&staged_root)?;
        sync_directory(&target_area)?;

        let journal = FileTransactionJournal {
            schema_version: 1,
            transaction_id,
            receipt_id,
            installation_id: request.installation_id,
            owner_scope: request.owner_scope,
            root_path: root.to_string_lossy().into_owned(),
            root_identity,
            game_fingerprint: request.game_fingerprint,
            profile_revision: request.profile_revision,
            plan_hash: request.plan_hash,
            module_lock_digest: request.module_lock_digest,
            kind: FileTransactionKind::Install,
            state: FileTransactionState::Prepared,
            sequence: 0,
            entries,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.insert_journal(&journal)?;
        pending_area.disarm();
        Ok(PreparedTransaction {
            journal,
            _root_lock: root_lock,
        })
    }

    pub fn commit(
        &self,
        prepared: PreparedTransaction,
        cancellation: &FileCancellation,
    ) -> Result<CommitOutcome, FileTransactionError> {
        self.commit_internal(prepared, cancellation, None, None)
    }

    pub fn recover_incomplete(
        &self,
        process_guard: &dyn TargetProcessGuard,
    ) -> Result<Vec<RecoveryReport>, FileTransactionError> {
        let rows = {
            let connection = self.database.connection()?;
            let mut statement = connection.prepare(
                "SELECT kind, journal_json FROM file_transactions
                 WHERE state IN ('prepared', 'applying', 'recovery-required')
                 ORDER BY created_at_ms, transaction_id",
            )?;
            let mapped = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            mapped.collect::<Result<Vec<_>, _>>()?
        };
        let mut reports = Vec::with_capacity(rows.len());
        for (kind, raw) in rows {
            if kind == "uninstall" {
                reports.push(self.recover_uninstall(&raw, process_guard)?);
                continue;
            }
            let mut journal: FileTransactionJournal = serde_json::from_str(&raw)?;
            let root = PathBuf::from(&journal.root_path);
            let identity = physical_root_identity(&root)?;
            if identity != journal.root_identity {
                reports.push(self.mark_recovery_required(
                    &mut journal,
                    "installation root identity changed".to_owned(),
                )?);
                continue;
            }
            let _lock = match self.acquire_root_lock(&identity) {
                Ok(lock) => lock,
                Err(FileTransactionError::RootBusy(owner)) => {
                    reports.push(RecoveryReport {
                        transaction_id: journal.transaction_id.clone(),
                        disposition: RecoveryDisposition::RecoveryRequired,
                        detail: format!("root remains busy: {owner}"),
                        receipt: None,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Err(reason) = process_guard.ensure_stopped(&root) {
                reports.push(RecoveryReport {
                    transaction_id: journal.transaction_id.clone(),
                    disposition: RecoveryDisposition::RecoveryRequired,
                    detail: format!("target is running: {reason}"),
                    receipt: None,
                });
                continue;
            }
            let marker = transaction_area(&journal).join("COMMITTED");
            if marker.is_file() {
                match self.recover_committed(&mut journal) {
                    Ok(receipt) => reports.push(RecoveryReport {
                        transaction_id: journal.transaction_id.clone(),
                        disposition: RecoveryDisposition::RecoveredCommitted,
                        detail: "committed marker and installed bytes verified".to_owned(),
                        receipt: Some(receipt),
                    }),
                    Err(error) => {
                        reports.push(self.mark_recovery_required(&mut journal, error.to_string())?)
                    }
                }
            } else {
                match self.rollback(&mut journal) {
                    Ok(()) => reports.push(RecoveryReport {
                        transaction_id: journal.transaction_id.clone(),
                        disposition: RecoveryDisposition::RolledBack,
                        detail: "restored the pre-transaction target state".to_owned(),
                        receipt: None,
                    }),
                    Err(error) => {
                        reports.push(self.mark_recovery_required(&mut journal, error.to_string())?)
                    }
                }
            }
        }
        Ok(reports)
    }

    pub fn receipt(&self, receipt_id: &str) -> Result<InstallReceipt, FileTransactionError> {
        let connection = self.database.connection()?;
        let (state, raw) = connection
            .query_row(
                "SELECT state, receipt_json FROM install_receipts WHERE receipt_id = ?1",
                [receipt_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| FileTransactionError::ReceiptNotFound(receipt_id.to_owned()))?;
        if state != "active" {
            return Err(FileTransactionError::ReceiptInactive(receipt_id.to_owned()));
        }
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn active_receipts_for_installation(
        &self,
        installation_id: &str,
    ) -> Result<Vec<InstallReceipt>, FileTransactionError> {
        if installation_id.trim().is_empty() {
            return Err(FileTransactionError::InvalidRequest(
                "installation id is required".to_owned(),
            ));
        }
        let connection = self.database.connection()?;
        let mut statement = connection.prepare(
            "SELECT receipt_json FROM install_receipts
             WHERE installation_id = ?1 AND state = 'active'
             ORDER BY created_at_ms DESC, rowid DESC",
        )?;
        let rows = statement.query_map([installation_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }


    fn commit_internal(
        &self,
        mut prepared: PreparedTransaction,
        cancellation: &FileCancellation,
        crash_after_mutation: Option<usize>,
        cancel_after_mutation: Option<usize>,
    ) -> Result<CommitOutcome, FileTransactionError> {
        let journal = &mut prepared.journal;
        let root = PathBuf::from(&journal.root_path);
        if physical_root_identity(&root)? != journal.root_identity {
            return Err(FileTransactionError::RecoveryRequired {
                transaction_id: journal.transaction_id.clone(),
                reason: "installation root identity changed after prepare".to_owned(),
            });
        }
        if cancellation.is_requested() {
            journal.state = FileTransactionState::RolledBack;
            self.persist_journal(journal)?;
            cleanup_dir(&transaction_area(journal));
            return Ok(CommitOutcome {
                transaction_id: journal.transaction_id.clone(),
                disposition: CommitDisposition::Cancelled,
                receipt: None,
                cancel_requested_during_commit: false,
            });
        }
        journal.state = FileTransactionState::Applying;
        self.persist_journal(journal)?;
        let apply_result = self.apply(
            journal,
            cancellation,
            crash_after_mutation,
            cancel_after_mutation,
        );
        if let Err(error) = apply_result {
            #[cfg(test)]
            if matches!(error, FileTransactionError::SimulatedCrash) {
                return Err(error);
            }
            let reason = error.to_string();
            return match self.rollback(journal) {
                Ok(()) => Err(FileTransactionError::CommitRolledBack {
                    transaction_id: journal.transaction_id.clone(),
                    reason,
                }),
                Err(rollback_error) => {
                    journal.state = FileTransactionState::RecoveryRequired;
                    let _ = self.persist_journal(journal);
                    Err(FileTransactionError::RecoveryRequired {
                        transaction_id: journal.transaction_id.clone(),
                        reason: format!("{reason}; rollback failed: {rollback_error}"),
                    })
                }
            };
        }

        let receipt = match build_receipt(journal) {
            Ok(receipt) => receipt,
            Err(error) => {
                return Err(self.rollback_commit_error(journal, error.to_string()));
            }
        };
        if let Err(error) = write_transaction_receipt(journal, &receipt) {
            return Err(self.rollback_commit_error(journal, error.to_string()));
        }
        let marker = transaction_area(journal).join("COMMITTED");
        if let Err(error) = write_create_new(&marker, b"mintcat-file-transaction-v1\n") {
            return Err(self.rollback_commit_error(journal, error.to_string()));
        }
        if let Err(error) = sync_directory(&transaction_area(journal)) {
            return Err(self.commit_decision_recovery_error(journal, error.to_string()));
        }
        #[cfg(test)]
        if crash_after_mutation == Some(journal.entries.len() * 2 + 1) {
            return Err(FileTransactionError::SimulatedCrash);
        }
        journal.state = FileTransactionState::Committed;
        journal.updated_at_ms = match now_ms() {
            Ok(now) => now,
            Err(error) => {
                return Err(self.commit_decision_recovery_error(journal, error.to_string()));
            }
        };
        if let Err(error) = self.persist_committed(journal, &receipt) {
            return Err(self.commit_decision_recovery_error(
                journal,
                format!("receipt index failed: {error}"),
            ));
        }
        Ok(CommitOutcome {
            transaction_id: journal.transaction_id.clone(),
            disposition: CommitDisposition::Committed,
            receipt: Some(receipt),
            cancel_requested_during_commit: cancellation.is_requested(),
        })
    }

    fn rollback_commit_error(
        &self,
        journal: &mut FileTransactionJournal,
        reason: String,
    ) -> FileTransactionError {
        match self.rollback(journal) {
            Ok(()) => FileTransactionError::CommitRolledBack {
                transaction_id: journal.transaction_id.clone(),
                reason,
            },
            Err(rollback_error) => {
                journal.state = FileTransactionState::RecoveryRequired;
                let persist_error = self.persist_journal(journal).err();
                let mut detail = format!("{reason}; rollback failed: {rollback_error}");
                if let Some(error) = persist_error {
                    detail.push_str(&format!("; recovery journal update failed: {error}"));
                }
                FileTransactionError::RecoveryRequired {
                    transaction_id: journal.transaction_id.clone(),
                    reason: detail,
                }
            }
        }
    }

    fn commit_decision_recovery_error(
        &self,
        journal: &mut FileTransactionJournal,
        reason: String,
    ) -> FileTransactionError {
        journal.state = FileTransactionState::RecoveryRequired;
        let persist_error = self.persist_journal(journal).err();
        let mut detail = format!("committed marker written but commit indexing failed: {reason}");
        if let Some(error) = persist_error {
            detail.push_str(&format!("; recovery journal update failed: {error}"));
        }
        FileTransactionError::RecoveryRequired {
            transaction_id: journal.transaction_id.clone(),
            reason: detail,
        }
    }

    fn apply(
        &self,
        journal: &mut FileTransactionJournal,
        cancellation: &FileCancellation,
        crash_after_mutation: Option<usize>,
        cancel_after_mutation: Option<usize>,
    ) -> Result<(), FileTransactionError> {
        let root = PathBuf::from(&journal.root_path);
        let area = transaction_area(journal);
        let mut mutation = 0usize;
        for index in 0..journal.entries.len() {
            let entry = journal.entries[index].clone();
            let relative = checked_relative_path(&entry.relative_path)?;
            validate_existing_ancestors(&root, &relative)?;
            for directory in &entry.created_parent_dirs {
                let relative_directory = checked_relative_path(directory)?;
                let path = root.join(relative_directory);
                if !path.exists() {
                    fs::create_dir(&path).map_err(|source| io_error(&path, source))?;
                    sync_parent(&path)?;
                } else if !path.is_dir() {
                    return Err(FileTransactionError::TargetMismatch {
                        path: directory.clone(),
                        reason: "planned parent is not a directory".to_owned(),
                    });
                }
            }
            let target = root.join(&relative);
            validate_before_digest(&target, &entry.before_digest, &entry.relative_path)?;
            if let Some(backup_relative) = &entry.backup_relative_path {
                let backup = area.join(backup_relative);
                if let Some(parent) = backup.parent() {
                    create_dir_all(parent)?;
                }
                fs::rename(&target, &backup).map_err(|source| io_error(&target, source))?;
                sync_parent(&target)?;
                mutation += 1;
                maybe_crash(crash_after_mutation, mutation)?;
                maybe_request_cancel(cancellation, cancel_after_mutation, mutation);
                journal.entries[index].state = EntryState::BackupSaved;
                self.persist_journal(journal)?;
            }
            let staged = area.join(&entry.staged_relative_path);
            fs::rename(&staged, &target).map_err(|source| io_error(&target, source))?;
            set_mode(&target, entry.install_mode)?;
            sync_file(&target)?;
            sync_parent(&target)?;
            mutation += 1;
            maybe_crash(crash_after_mutation, mutation)?;
            maybe_request_cancel(cancellation, cancel_after_mutation, mutation);
            journal.entries[index].state = EntryState::Replaced;
            self.persist_journal(journal)?;
            let (digest, size) = hash_file_with_size(&target)?;
            if digest != entry.after_digest || size != entry.after_size {
                return Err(FileTransactionError::TargetMismatch {
                    path: entry.relative_path.clone(),
                    reason: "installed bytes failed post-write verification".to_owned(),
                });
            }
            journal.entries[index].state = EntryState::Verified;
            self.persist_journal(journal)?;
        }
        Ok(())
    }

    fn rollback(&self, journal: &mut FileTransactionJournal) -> Result<(), FileTransactionError> {
        let root = PathBuf::from(&journal.root_path);
        let area = transaction_area(journal);
        for entry in journal.entries.iter().rev() {
            let relative = checked_relative_path(&entry.relative_path)?;
            let target = root.join(&relative);
            let target_digest = existing_file_digest(&target)?;
            match (&entry.before_digest, &entry.backup_relative_path) {
                (Some(before), Some(backup_relative)) => {
                    let backup = area.join(backup_relative);
                    if backup.is_file() {
                        if hash_file(&backup)? != *before {
                            return Err(recovery_mismatch(journal, entry, "backup digest changed"));
                        }
                        if let Some(digest) = target_digest.as_deref() {
                            if digest == entry.after_digest {
                                fs::remove_file(&target)
                                    .map_err(|source| io_error(&target, source))?;
                            } else if digest != before {
                                return Err(recovery_mismatch(
                                    journal,
                                    entry,
                                    "target was externally modified during rollback",
                                ));
                            } else {
                                fs::remove_file(&backup)
                                    .map_err(|source| io_error(&backup, source))?;
                                continue;
                            }
                        }
                        fs::rename(&backup, &target).map_err(|source| io_error(&target, source))?;
                        sync_parent(&target)?;
                    } else if target_digest.as_deref() != Some(before.as_str()) {
                        return Err(recovery_mismatch(
                            journal,
                            entry,
                            "original target and backup are unavailable",
                        ));
                    }
                }
                (None, None) => match target_digest.as_deref() {
                    Some(digest) if digest == entry.after_digest => {
                        fs::remove_file(&target).map_err(|source| io_error(&target, source))?;
                        sync_parent(&target)?;
                    }
                    Some(_) => {
                        return Err(recovery_mismatch(
                            journal,
                            entry,
                            "new target was externally modified during rollback",
                        ));
                    }
                    None => {}
                },
                _ => {
                    return Err(recovery_mismatch(
                        journal,
                        entry,
                        "journal before/backup contract is inconsistent",
                    ));
                }
            }
        }
        for entry in journal.entries.iter().rev() {
            remove_empty_dirs(&root, &entry.created_parent_dirs);
        }
        journal.state = FileTransactionState::RolledBack;
        self.persist_journal(journal)?;
        cleanup_dir(&area);
        Ok(())
    }

    fn recover_committed(
        &self,
        journal: &mut FileTransactionJournal,
    ) -> Result<InstallReceipt, FileTransactionError> {
        let receipt_path = transaction_area(journal).join("receipt.json");
        let receipt: InstallReceipt = serde_json::from_slice(
            &fs::read(&receipt_path).map_err(|source| io_error(&receipt_path, source))?,
        )?;
        let root = PathBuf::from(&journal.root_path);
        for entry in &receipt.files {
            let path = root.join(checked_relative_path(&entry.relative_path)?);
            if hash_file(&path)? != entry.installed_digest {
                return Err(FileTransactionError::RecoveryRequired {
                    transaction_id: journal.transaction_id.clone(),
                    reason: format!("committed target {} changed", entry.relative_path),
                });
            }
        }
        journal.state = FileTransactionState::Committed;
        journal.updated_at_ms = now_ms()?;
        self.persist_committed(journal, &receipt)?;
        Ok(receipt)
    }

    fn mark_recovery_required(
        &self,
        journal: &mut FileTransactionJournal,
        detail: String,
    ) -> Result<RecoveryReport, FileTransactionError> {
        journal.state = FileTransactionState::RecoveryRequired;
        self.persist_journal(journal)?;
        Ok(RecoveryReport {
            transaction_id: journal.transaction_id.clone(),
            disposition: RecoveryDisposition::RecoveryRequired,
            detail,
            receipt: None,
        })
    }

    fn acquire_root_lock(&self, root_identity: &str) -> Result<File, FileTransactionError> {
        let name = format!("{}.lock", sha256_bytes(root_identity.as_bytes()));
        let path = self.locks.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(file),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                Err(FileTransactionError::RootBusy(root_identity.to_owned()))
            }
            Err(source) => Err(io_error(&path, source)),
        }
    }

    fn ensure_no_active_transaction(
        &self,
        root_identity: &str,
    ) -> Result<(), FileTransactionError> {
        let connection = self.database.connection()?;
        let active = connection
            .query_row(
                "SELECT transaction_id FROM file_transactions
                 WHERE root_identity = ?1
                   AND state IN ('prepared', 'applying', 'recovery-required')",
                [root_identity],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(transaction_id) = active {
            return Err(FileTransactionError::RootBusy(transaction_id));
        }
        Ok(())
    }

    fn insert_journal(&self, journal: &FileTransactionJournal) -> Result<(), FileTransactionError> {
        let connection = self.database.connection()?;
        connection.execute(
            "INSERT INTO file_transactions(
               transaction_id, installation_id, owner_scope, root_identity, root_path,
               kind, state, journal_json, receipt_json, sequence, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?10)",
            params![
                journal.transaction_id,
                journal.installation_id,
                journal.owner_scope,
                journal.root_identity,
                journal.root_path,
                journal.kind.as_str(),
                journal.state.as_str(),
                serde_json::to_string(journal)?,
                journal.sequence,
                journal.created_at_ms,
            ],
        )?;
        Ok(())
    }

    fn persist_journal(
        &self,
        journal: &mut FileTransactionJournal,
    ) -> Result<(), FileTransactionError> {
        journal.sequence = journal.sequence.checked_add(1).ok_or_else(|| {
            FileTransactionError::InvalidRequest("journal sequence overflow".to_owned())
        })?;
        journal.updated_at_ms = now_ms()?;
        let connection = self.database.connection()?;
        let changed = connection.execute(
            "UPDATE file_transactions
             SET state = ?2, journal_json = ?3, sequence = ?4, updated_at_ms = ?5
             WHERE transaction_id = ?1",
            params![
                journal.transaction_id,
                journal.state.as_str(),
                serde_json::to_string(journal)?,
                journal.sequence,
                journal.updated_at_ms,
            ],
        )?;
        if changed != 1 {
            return Err(FileTransactionError::TransactionNotFound(
                journal.transaction_id.clone(),
            ));
        }
        Ok(())
    }

    fn persist_committed(
        &self,
        journal: &mut FileTransactionJournal,
        receipt: &InstallReceipt,
    ) -> Result<(), FileTransactionError> {
        journal.sequence = journal.sequence.checked_add(1).ok_or_else(|| {
            FileTransactionError::InvalidRequest("journal sequence overflow".to_owned())
        })?;
        let now = now_ms()?;
        journal.updated_at_ms = now;
        let journal_json = serde_json::to_string(journal)?;
        let receipt_json = serde_json::to_string(receipt)?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO install_receipts(
               receipt_id, installation_id, owner_scope, root_identity, state,
               receipt_json, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?6)
             ON CONFLICT(receipt_id) DO UPDATE SET
               state = 'active', receipt_json = excluded.receipt_json,
               updated_at_ms = excluded.updated_at_ms",
            params![
                receipt.receipt_id,
                receipt.installation_id,
                receipt.owner_scope,
                receipt.root_identity,
                receipt_json,
                receipt.committed_at_ms,
            ],
        )?;
        transaction.execute(
            "UPDATE file_transactions SET state = 'committed', journal_json = ?2,
               receipt_json = ?3, sequence = ?4, updated_at_ms = ?5
             WHERE transaction_id = ?1",
            params![
                journal.transaction_id,
                journal_json,
                serde_json::to_string(receipt)?,
                journal.sequence,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn commit_with_crash(
        &self,
        prepared: PreparedTransaction,
        crash_after_mutation: usize,
    ) -> Result<CommitOutcome, FileTransactionError> {
        self.commit_internal(
            prepared,
            &FileCancellation::default(),
            Some(crash_after_mutation),
            None,
        )
    }

    #[cfg(test)]
    fn commit_with_cancel_after_mutation(
        &self,
        prepared: PreparedTransaction,
        cancellation: &FileCancellation,
        cancel_after_mutation: usize,
    ) -> Result<CommitOutcome, FileTransactionError> {
        self.commit_internal(prepared, cancellation, None, Some(cancel_after_mutation))
    }
}

fn validate_request(request: &DeployRequest<'_>) -> Result<(), FileTransactionError> {
    if request.installation_id.trim().is_empty()
        || request.owner_scope.trim().is_empty()
        || request.game_fingerprint.trim().is_empty()
        || request.plan_hash.trim().is_empty()
        || request.module_lock_digest.trim().is_empty()
        || request.files.is_empty()
    {
        return Err(FileTransactionError::InvalidRequest(
            "identities, plan metadata, and at least one file are required".to_owned(),
        ));
    }
    if request
        .files
        .iter()
        .any(|file| file.owner.trim().is_empty())
    {
        return Err(FileTransactionError::InvalidRequest(
            "every file requires an owner".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, FileTransactionError> {
    let canonical = fs::canonicalize(path).map_err(|source| io_error(path, source))?;
    if !canonical.is_dir() {
        return Err(FileTransactionError::InvalidRequest(format!(
            "installation root is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

pub fn physical_root_identity(path: &Path) -> Result<String, FileTransactionError> {
    let canonical = canonical_directory(path)?;
    #[cfg(unix)]
    let metadata = fs::metadata(&canonical).map_err(|source| io_error(&canonical, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        Ok(format!(
            "windows:{}",
            canonical.to_string_lossy().to_lowercase()
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(format!("path:{}", canonical.display()))
    }
}

fn checked_relative_path(value: &str) -> Result<PathBuf, FileTransactionError> {
    if value.trim().is_empty()
        || value.contains('\0')
        || value.contains('\\')
        || value.contains(':')
    {
        return Err(FileTransactionError::InvalidPath(value.to_owned()));
    }
    let path = Path::new(value);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            _ => return Err(FileTransactionError::InvalidPath(value.to_owned())),
        }
    }
    if clean.as_os_str().is_empty()
        || clean
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == TARGET_TRANSACTION_DIR)
    {
        return Err(FileTransactionError::InvalidPath(value.to_owned()));
    }
    Ok(clean)
}

fn path_identity_key(path: &Path) -> String {
    relative_path_string(path).to_lowercase()
}

fn relative_path_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_existing_ancestors(root: &Path, relative: &Path) -> Result<(), FileTransactionError> {
    let mut current = root.to_path_buf();
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(FileTransactionError::TargetMismatch {
                    path: current.display().to_string(),
                    reason: "parent path is a symlink or junction".to_owned(),
                });
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(FileTransactionError::TargetMismatch {
                    path: current.display().to_string(),
                    reason: "parent path is not a directory".to_owned(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(source) => return Err(io_error(&current, source)),
        }
    }
    Ok(())
}

fn missing_parent_dirs(root: &Path, relative: &Path) -> Result<Vec<String>, FileTransactionError> {
    let mut current = root.to_path_buf();
    let mut relative_current = PathBuf::new();
    let mut missing = false;
    let mut result = Vec::new();
    for component in relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
    {
        current.push(component.as_os_str());
        relative_current.push(component.as_os_str());
        if missing || !current.exists() {
            missing = true;
            result.push(relative_path_string(&relative_current));
        }
    }
    Ok(result)
}

fn validate_expected_target(
    target: &Path,
    relative: &Path,
    expected: &ExpectedTarget,
) -> Result<(Option<String>, Option<u32>), FileTransactionError> {
    let metadata = target_metadata(target)?;
    match (expected, metadata) {
        (ExpectedTarget::Absent, None) => Ok((None, None)),
        (ExpectedTarget::Absent, Some(_)) => Err(FileTransactionError::TargetMismatch {
            path: relative.display().to_string(),
            reason: "target was expected to be absent".to_owned(),
        }),
        (ExpectedTarget::Digest(expected), Some(metadata)) if metadata.is_file() => {
            let actual = hash_file(target)?;
            if &actual != expected {
                return Err(FileTransactionError::TargetMismatch {
                    path: relative.display().to_string(),
                    reason: format!("expected digest {expected}, found {actual}"),
                });
            }
            Ok((Some(actual), mode(&metadata)))
        }
        (ExpectedTarget::Digest(_), Some(_)) => Err(FileTransactionError::TargetMismatch {
            path: relative.display().to_string(),
            reason: "target is not a regular file".to_owned(),
        }),
        (ExpectedTarget::Digest(expected), None) => Err(FileTransactionError::TargetMismatch {
            path: relative.display().to_string(),
            reason: format!("expected existing digest {expected}, found no file"),
        }),
    }
}

fn validate_before_digest(
    target: &Path,
    expected: &Option<String>,
    relative: &str,
) -> Result<(), FileTransactionError> {
    let actual = existing_file_digest(target)?;
    if &actual != expected {
        return Err(FileTransactionError::TargetMismatch {
            path: relative.to_owned(),
            reason: format!("prepare observed {expected:?}, commit observed {actual:?}"),
        });
    }
    Ok(())
}

fn target_metadata(path: &Path) -> Result<Option<fs::Metadata>, FileTransactionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(FileTransactionError::TargetMismatch {
                path: path.display().to_string(),
                reason: "target is a symlink or junction".to_owned(),
            })
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}

fn existing_file_digest(path: &Path) -> Result<Option<String>, FileTransactionError> {
    match target_metadata(path)? {
        Some(metadata) if metadata.is_file() => Ok(Some(hash_file(path)?)),
        Some(_) => Err(FileTransactionError::TargetMismatch {
            path: path.display().to_string(),
            reason: "target is not a regular file".to_owned(),
        }),
        None => Ok(None),
    }
}

fn copy_and_hash(
    input: &mut File,
    destination: &Path,
) -> Result<(String, u64), FileTransactionError> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| io_error(destination, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|source| io_error(destination, source))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|source| io_error(destination, source))?;
        hasher.update(&buffer[..read]);
        size = size.checked_add(read as u64).ok_or_else(|| {
            FileTransactionError::InvalidRequest("artifact size overflow".to_owned())
        })?;
    }
    output
        .sync_all()
        .map_err(|source| io_error(destination, source))?;
    Ok((format!("sha256:{:x}", hasher.finalize()), size))
}

fn hash_file(path: &Path) -> Result<String, FileTransactionError> {
    hash_file_with_size(path).map(|(digest, _)| digest)
}

fn hash_file_with_size(path: &Path) -> Result<(String, u64), FileTransactionError> {
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| FileTransactionError::InvalidRequest("file size overflow".to_owned()))?;
    }
    Ok((format!("sha256:{:x}", hasher.finalize()), size))
}

fn build_receipt(journal: &FileTransactionJournal) -> Result<InstallReceipt, FileTransactionError> {
    let root = PathBuf::from(&journal.root_path);
    let files = journal
        .entries
        .iter()
        .map(|entry| {
            let metadata = fs::metadata(root.join(&entry.relative_path))
                .map_err(|source| io_error(root.join(&entry.relative_path), source))?;
            Ok(ReceiptFile {
                relative_path: entry.relative_path.clone(),
                owner: entry.owner.clone(),
                before_digest: entry.before_digest.clone(),
                installed_digest: entry.after_digest.clone(),
                original_mode: entry.original_mode,
                installed_mode: mode(&metadata),
                backup_relative_path: entry.backup_relative_path.clone(),
                created_parent_dirs: entry.created_parent_dirs.clone(),
            })
        })
        .collect::<Result<Vec<_>, FileTransactionError>>()?;
    Ok(InstallReceipt {
        schema_version: 1,
        receipt_id: journal.receipt_id.clone(),
        transaction_id: journal.transaction_id.clone(),
        installation_id: journal.installation_id.clone(),
        owner_scope: journal.owner_scope.clone(),
        root_path: journal.root_path.clone(),
        root_identity: journal.root_identity.clone(),
        game_fingerprint: journal.game_fingerprint.clone(),
        profile_revision: journal.profile_revision,
        plan_hash: journal.plan_hash.clone(),
        module_lock_digest: journal.module_lock_digest.clone(),
        files,
        committed_at_ms: now_ms()?,
    })
}

fn write_transaction_receipt(
    journal: &FileTransactionJournal,
    receipt: &InstallReceipt,
) -> Result<(), FileTransactionError> {
    let path = transaction_area(journal).join("receipt.json");
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    write_create_new(&path, &bytes)
}

fn write_create_new(path: &Path, bytes: &[u8]) -> Result<(), FileTransactionError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error(path, source))?;
    file.sync_all().map_err(|source| io_error(path, source))?;
    Ok(())
}

fn transaction_area(journal: &FileTransactionJournal) -> PathBuf {
    PathBuf::from(&journal.root_path)
        .join(TARGET_TRANSACTION_DIR)
        .join(&journal.transaction_id)
}

fn recovery_mismatch(
    journal: &FileTransactionJournal,
    entry: &JournalEntry,
    reason: &str,
) -> FileTransactionError {
    FileTransactionError::RecoveryRequired {
        transaction_id: journal.transaction_id.clone(),
        reason: format!("{}: {reason}", entry.relative_path),
    }
}


fn remove_empty_dirs(root: &Path, directories: &[String]) {
    for directory in directories.iter().rev() {
        if let Ok(relative) = checked_relative_path(directory) {
            let _ = fs::remove_dir(root.join(relative));
        }
    }
}

fn cleanup_dir(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn ensure_same_filesystem(
    root: &Path,
    transaction_area: &Path,
) -> Result<(), FileTransactionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let root_device = fs::metadata(root)
            .map_err(|source| io_error(root, source))?
            .dev();
        let transaction_device = fs::metadata(transaction_area)
            .map_err(|source| io_error(transaction_area, source))?
            .dev();
        if root_device != transaction_device {
            return Err(FileTransactionError::CrossVolume);
        }
    }
    #[cfg(not(unix))]
    {
        let root = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
        let transaction_area = fs::canonicalize(transaction_area)
            .map_err(|source| io_error(transaction_area, source))?;
        if root.components().next() != transaction_area.components().next() {
            return Err(FileTransactionError::CrossVolume);
        }
    }
    Ok(())
}

fn create_dir_all(path: &Path) -> Result<(), FileTransactionError> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

fn sync_file(path: &Path) -> Result<(), FileTransactionError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(path, source))
}

fn sync_parent(path: &Path) -> Result<(), FileTransactionError> {
    path.parent().map_or(Ok(()), sync_directory)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), FileTransactionError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), FileTransactionError> {
    Ok(())
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn set_mode(path: &Path, requested: Option<u32>) -> Result<(), FileTransactionError> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(mode) = requested {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _requested: Option<u32>) -> Result<(), FileTransactionError> {
    Ok(())
}

fn maybe_crash(
    crash_after_mutation: Option<usize>,
    mutation: usize,
) -> Result<(), FileTransactionError> {
    #[cfg(test)]
    if crash_after_mutation == Some(mutation) {
        return Err(FileTransactionError::SimulatedCrash);
    }
    #[cfg(not(test))]
    let _ = (crash_after_mutation, mutation);
    Ok(())
}

fn maybe_request_cancel(
    cancellation: &FileCancellation,
    cancel_after_mutation: Option<usize>,
    mutation: usize,
) {
    #[cfg(test)]
    if cancel_after_mutation == Some(mutation) {
        cancellation.request();
    }
    #[cfg(not(test))]
    let _ = (cancellation, cancel_after_mutation, mutation);
}

fn now_ms() -> Result<i64, FileTransactionError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| FileTransactionError::InvalidRequest(error.to_string()))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| FileTransactionError::InvalidRequest("timestamp overflow".to_owned()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn io_error(path: impl Into<PathBuf>, source: io::Error) -> FileTransactionError {
    FileTransactionError::Io {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests;
