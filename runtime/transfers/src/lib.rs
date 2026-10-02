use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use md5::{Digest as _, Md5};
use mintcat_runtime_artifacts::{ArtifactError, ArtifactGrant, ArtifactStore};
use mintcat_runtime_broker::{
    BrokerError, CancellationToken, NetworkBroker, NetworkRequest, NetworkStreamResponse,
};
use mintcat_runtime_kernel::{KernelDatabase, KernelDatabaseError};
use mintcat_sdk::ArtifactRef;
use mintcat_source_api::DownloadDescriptor;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use uuid::Uuid;

const MIGRATION: &str = r#"
CREATE TABLE transfers (
  transfer_id TEXT PRIMARY KEY,
  identity_digest TEXT NOT NULL UNIQUE,
  identity_json TEXT NOT NULL,
  state TEXT NOT NULL,
  active_attempt_id TEXT,
  artifact_json TEXT,
  failure_code TEXT,
  last_seq INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE TABLE transfer_attempts (
  attempt_id TEXT PRIMARY KEY,
  transfer_id TEXT NOT NULL REFERENCES transfers(transfer_id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  state TEXT NOT NULL,
  part_path TEXT NOT NULL UNIQUE,
  downloaded_bytes INTEGER NOT NULL,
  validator_kind TEXT,
  validator_value TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(transfer_id, ordinal)
);
CREATE INDEX transfer_attempts_transfer ON transfer_attempts(transfer_id, ordinal DESC);

CREATE TABLE transfer_consumers (
  transfer_id TEXT NOT NULL REFERENCES transfers(transfer_id) ON DELETE CASCADE,
  consumer_id TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY(transfer_id, consumer_id)
);

CREATE TABLE transfer_artifact_references (
  reference_id TEXT PRIMARY KEY,
  transfer_id TEXT NOT NULL REFERENCES transfers(transfer_id) ON DELETE CASCADE,
  digest TEXT NOT NULL,
  kind TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX transfer_artifact_refs_digest ON transfer_artifact_references(digest);

CREATE TABLE transfer_events (
  transfer_id TEXT NOT NULL REFERENCES transfers(transfer_id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  attempt_id TEXT,
  phase TEXT NOT NULL,
  state TEXT NOT NULL,
  downloaded_bytes INTEGER NOT NULL,
  total_bytes INTEGER,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(transfer_id, seq)
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferFileKey {
    pub source_id: String,
    pub remote_game_key: String,
    pub remote_mod_id: String,
    pub resolved_version_id: String,
    pub remote_file_id: String,
    pub byte_variant: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TransferSharing {
    StableFileIdentity,
    AfterDigestOnly,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferValidation {
    pub expected_sha256: Option<String>,
    pub required_prefix: Vec<u8>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub file_key: TransferFileKey,
    pub sharing: TransferSharing,
    pub descriptor: DownloadDescriptor,
    pub validation: TransferValidation,
}

#[derive(Debug, Clone, Copy)]
pub struct TransferConfig {
    pub max_bytes: u64,
    pub progress_step_bytes: u64,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024 * 1024,
            progress_step_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TransferState {
    Queued,
    Downloading,
    Verifying,
    Publishing,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferSnapshot {
    pub transfer_id: String,
    pub file_key: TransferFileKey,
    pub state: TransferState,
    pub active_attempt_id: Option<String>,
    pub artifact: Option<ArtifactRef>,
    pub failure_code: Option<String>,
    pub last_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferEvent {
    pub transfer_id: String,
    pub seq: u64,
    pub attempt_id: Option<String>,
    pub phase: String,
    pub state: TransferState,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub created_at_ms: i64,
}

#[derive(Debug, Error)]
pub enum TransferError {
    #[error(transparent)]
    Kernel(#[from] KernelDatabaseError),
    #[error("transfer database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("transfer JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Broker(#[from] BrokerError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("transfer I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("transfer was not found: {0}")]
    NotFound(String),
    #[error("transfer already has an active attempt")]
    AlreadyRunning,
    #[error("transfer has no active consumers")]
    NoConsumers,
    #[error("transfer request does not match its fixed file identity: {0}")]
    IdentityMismatch(String),
    #[error("download authorization expired")]
    AuthorizationExpired,
    #[error("download server returned unexpected HTTP status {0}")]
    HttpStatus(u16),
    #[error("download resume response did not match its validator")]
    InvalidResume,
    #[error("download size mismatch: expected {expected}, actual {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("download checksum mismatch: {algorithm}")]
    ChecksumMismatch { algorithm: &'static str },
    #[error("download content does not match its required format prefix")]
    FormatMismatch,
    #[error("transfer was cancelled after its last consumer detached")]
    Cancelled,
    #[error("transfer runtime state lock is poisoned")]
    Poisoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransferIdentity {
    file_key: TransferFileKey,
    expected_size: u64,
    expected_md5: Option<String>,
    validation: TransferValidation,
}

#[derive(Debug)]
struct ClaimedAttempt {
    attempt_id: String,
    part_path: PathBuf,
    checkpoint: Option<ResumeCheckpoint>,
}

#[derive(Debug)]
struct ResumeCheckpoint {
    part_path: PathBuf,
    downloaded_bytes: u64,
    validator_kind: String,
    validator_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseValidator {
    kind: String,
    value: String,
}

#[derive(Debug)]
struct RawTransfer {
    transfer_id: String,
    identity_json: String,
    state: String,
    active_attempt_id: Option<String>,
    artifact_json: Option<String>,
    failure_code: Option<String>,
    last_seq: u64,
}

#[derive(Clone)]
pub struct TransferManager {
    database: KernelDatabase,
    artifacts: ArtifactStore,
    broker: NetworkBroker,
    staging: PathBuf,
    config: TransferConfig,
    active_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl TransferManager {
    pub fn open(
        database: KernelDatabase,
        artifacts: ArtifactStore,
        broker: NetworkBroker,
        staging: impl AsRef<Path>,
        config: TransferConfig,
    ) -> Result<Self, TransferError> {
        if config.max_bytes == 0 || config.progress_step_bytes == 0 {
            return Err(TransferError::IdentityMismatch(
                "transfer limits must be positive".to_owned(),
            ));
        }
        database.apply_migration("runtime.transfers", 1, MIGRATION)?;
        std::fs::create_dir_all(staging.as_ref())?;
        let manager = Self {
            database,
            artifacts,
            broker,
            staging: staging.as_ref().canonicalize()?,
            config,
            active_cancellations: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.recover_incomplete()?;
        Ok(manager)
    }

    pub fn attach(
        &self,
        consumer_id: &str,
        request: &TransferRequest,
    ) -> Result<TransferSnapshot, TransferError> {
        validate_request(request, self.config)?;
        if consumer_id.trim().is_empty() {
            return Err(TransferError::IdentityMismatch(
                "consumer id cannot be empty".to_owned(),
            ));
        }
        let identity = TransferIdentity {
            file_key: request.file_key.clone(),
            expected_size: request.descriptor.expected_size,
            expected_md5: normalize_md5(request.descriptor.expected_md5.as_deref())?,
            validation: request.validation.clone(),
        };
        let identity_json = serde_json::to_string(&identity)?;
        let sharing_salt = match request.sharing {
            TransferSharing::StableFileIdentity => None,
            TransferSharing::AfterDigestOnly => Some(Uuid::new_v4().to_string()),
        };
        let identity_digest = identity_digest(&identity, sharing_salt.as_deref())?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT transfer_id, identity_json, state, active_attempt_id, artifact_json,
                        failure_code, last_seq
                 FROM transfers WHERE identity_digest = ?1",
                [&identity_digest],
                raw_transfer,
            )
            .optional()?;
        let transfer_id = if let Some(existing) = existing {
            if existing.identity_json != identity_json {
                return Err(TransferError::IdentityMismatch(
                    "transfer identity digest collision".to_owned(),
                ));
            }
            if matches!(existing.state.as_str(), "failed" | "cancelled") {
                transaction.execute(
                    "UPDATE transfers SET state = 'queued', failure_code = NULL,
                       active_attempt_id = NULL, updated_at_ms = ?2 WHERE transfer_id = ?1",
                    params![existing.transfer_id, now_ms()?],
                )?;
            }
            existing.transfer_id
        } else {
            let transfer_id = format!("transfer:{}", Uuid::new_v4());
            let now = now_ms()?;
            transaction.execute(
                "INSERT INTO transfers(
                   transfer_id, identity_digest, identity_json, state, active_attempt_id,
                   artifact_json, failure_code, last_seq, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, 'queued', NULL, NULL, NULL, 0, ?4, ?4)",
                params![transfer_id, identity_digest, identity_json, now],
            )?;
            transfer_id
        };
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO transfer_consumers(transfer_id, consumer_id, state, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'active', ?3, ?3)
             ON CONFLICT(transfer_id, consumer_id) DO UPDATE SET state = 'active', updated_at_ms = excluded.updated_at_ms",
            params![transfer_id, consumer_id, now],
        )?;
        let raw = load_transfer(&transaction, &transfer_id)?;
        if let Some(artifact_json) = &raw.artifact_json {
            let artifact: ArtifactRef = serde_json::from_str(artifact_json)?;
            insert_artifact_reference(
                &transaction,
                &consumer_reference_id(&transfer_id, consumer_id),
                &transfer_id,
                &artifact.digest,
                "transfer-consumer",
            )?;
        }
        append_event(
            &transaction,
            &transfer_id,
            None,
            "consumer-attached",
            parse_state(&raw.state)?,
            0,
            Some(identity.expected_size),
        )?;
        transaction.commit()?;
        drop(connection);
        self.get(&transfer_id)
    }

    pub fn detach_consumer(
        &self,
        transfer_id: &str,
        consumer_id: &str,
    ) -> Result<(), TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE transfer_consumers SET state = 'released', updated_at_ms = ?3
             WHERE transfer_id = ?1 AND consumer_id = ?2 AND state = 'active'",
            params![transfer_id, consumer_id, now_ms()?],
        )?;
        if updated == 0 {
            load_transfer(&transaction, transfer_id)?;
        }
        transaction.execute(
            "DELETE FROM transfer_artifact_references WHERE reference_id = ?1",
            [consumer_reference_id(transfer_id, consumer_id)],
        )?;
        let active = active_consumer_count(&transaction, transfer_id)?;
        let raw = load_transfer(&transaction, transfer_id)?;
        if active == 0 && raw.state == "queued" {
            transaction.execute(
                "UPDATE transfers SET state = 'cancelled', failure_code = 'NO_CONSUMERS',
                   updated_at_ms = ?2 WHERE transfer_id = ?1",
                params![transfer_id, now_ms()?],
            )?;
            append_event(
                &transaction,
                transfer_id,
                None,
                "cancelled",
                TransferState::Cancelled,
                0,
                None,
            )?;
        } else {
            append_event(
                &transaction,
                transfer_id,
                raw.active_attempt_id.as_deref(),
                "consumer-detached",
                parse_state(&raw.state)?,
                0,
                None,
            )?;
        }
        transaction.commit()?;
        if active == 0 {
            if let Some(token) = self
                .active_cancellations
                .lock()
                .map_err(|_| TransferError::Poisoned)?
                .get(transfer_id)
                .cloned()
            {
                token.cancel();
            }
        }
        Ok(())
    }

    pub async fn run(
        &self,
        transfer_id: &str,
        request: &TransferRequest,
    ) -> Result<ArtifactRef, TransferError> {
        validate_request(request, self.config)?;
        self.validate_attached_identity(transfer_id, request)?;
        if let Some(artifact) = self.get(transfer_id)?.artifact {
            return Ok(artifact);
        }
        let claimed = self.claim_attempt(transfer_id)?;
        let cancellation = CancellationToken::default();
        {
            let mut active = self
                .active_cancellations
                .lock()
                .map_err(|_| TransferError::Poisoned)?;
            active.insert(transfer_id.to_owned(), cancellation.clone());
        }
        let outcome = self
            .execute_attempt(transfer_id, &claimed, request, &cancellation)
            .await;
        self.active_cancellations
            .lock()
            .map_err(|_| TransferError::Poisoned)?
            .remove(transfer_id);
        match outcome {
            Ok(artifact) => Ok(artifact),
            Err(error) => {
                self.fail_attempt(transfer_id, &claimed.attempt_id, &error)?;
                Err(error)
            }
        }
    }

    pub fn get(&self, transfer_id: &str) -> Result<TransferSnapshot, TransferError> {
        let connection = self.database.connection()?;
        load_transfer(&connection, transfer_id)?.into_snapshot()
    }

    pub fn events(&self, transfer_id: &str) -> Result<Vec<TransferEvent>, TransferError> {
        let connection = self.database.connection()?;
        load_transfer(&connection, transfer_id)?;
        let mut statement = connection.prepare(
            "SELECT seq, attempt_id, phase, state, downloaded_bytes, total_bytes, created_at_ms
             FROM transfer_events WHERE transfer_id = ?1 ORDER BY seq",
        )?;
        let rows = statement
            .query_map([transfer_id], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u64>(4)?,
                    row.get::<_, Option<u64>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .map(|row| {
                let (seq, attempt_id, phase, state, downloaded_bytes, total_bytes, created_at_ms) =
                    row?;
                Ok(TransferEvent {
                    transfer_id: transfer_id.to_owned(),
                    seq,
                    attempt_id,
                    phase,
                    state: parse_state(&state)?,
                    downloaded_bytes,
                    total_bytes,
                    created_at_ms,
                })
            })
            .collect();
        rows
    }

    pub fn latest_event(&self, transfer_id: &str) -> Result<Option<TransferEvent>, TransferError> {
        let connection = self.database.connection()?;
        load_transfer(&connection, transfer_id)?;
        let event = connection
            .query_row(
                "SELECT seq, attempt_id, phase, state, downloaded_bytes, total_bytes, created_at_ms
                 FROM transfer_events WHERE transfer_id = ?1 ORDER BY seq DESC LIMIT 1",
                [transfer_id],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u64>(4)?,
                        row.get::<_, Option<u64>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        event
            .map(
                |(seq, attempt_id, phase, state, downloaded_bytes, total_bytes, created_at_ms)| {
                    Ok(TransferEvent {
                        transfer_id: transfer_id.to_owned(),
                        seq,
                        attempt_id,
                        phase,
                        state: parse_state(&state)?,
                        downloaded_bytes,
                        total_bytes,
                        created_at_ms,
                    })
                },
            )
            .transpose()
    }

    pub fn artifact_for_consumer(
        &self,
        transfer_id: &str,
        consumer_id: &str,
    ) -> Result<ArtifactGrant, TransferError> {
        let connection = self.database.connection()?;
        let raw = load_transfer(&connection, transfer_id)?;
        let artifact_json = raw
            .artifact_json
            .ok_or_else(|| TransferError::IdentityMismatch("artifact is not ready".to_owned()))?;
        let artifact: ArtifactRef = serde_json::from_str(&artifact_json)?;
        let reference = connection
            .query_row(
                "SELECT 1 FROM transfer_artifact_references WHERE reference_id = ?1 AND digest = ?2",
                params![consumer_reference_id(transfer_id, consumer_id), artifact.digest],
                |_| Ok(()),
            )
            .optional()?;
        if reference.is_none() {
            return Err(TransferError::NoConsumers);
        }
        Ok(self.artifacts.grant_blob(&artifact)?)
    }

    pub fn add_artifact_reference(
        &self,
        transfer_id: &str,
        reference_id: &str,
    ) -> Result<ArtifactRef, TransferError> {
        if reference_id.trim().is_empty() {
            return Err(TransferError::IdentityMismatch(
                "artifact reference id cannot be empty".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_transfer(&transaction, transfer_id)?;
        let artifact: ArtifactRef =
            serde_json::from_str(raw.artifact_json.as_deref().ok_or_else(|| {
                TransferError::IdentityMismatch("artifact is not ready".to_owned())
            })?)?;
        insert_artifact_reference(
            &transaction,
            reference_id,
            transfer_id,
            &artifact.digest,
            "durable",
        )?;
        transaction.commit()?;
        Ok(artifact)
    }

    pub fn release_artifact_reference(&self, reference_id: &str) -> Result<(), TransferError> {
        let connection = self.database.connection()?;
        connection.execute(
            "DELETE FROM transfer_artifact_references WHERE reference_id = ?1",
            [reference_id],
        )?;
        Ok(())
    }

    pub fn retained_artifact_digests(&self) -> Result<HashSet<String>, TransferError> {
        let connection = self.database.connection()?;
        let mut statement =
            connection.prepare("SELECT DISTINCT digest FROM transfer_artifact_references")?;
        let digests = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        Ok(digests)
    }

    fn validate_attached_identity(
        &self,
        transfer_id: &str,
        request: &TransferRequest,
    ) -> Result<(), TransferError> {
        let snapshot = self.get(transfer_id)?;
        if snapshot.file_key != request.file_key {
            return Err(TransferError::IdentityMismatch(
                "source file key changed".to_owned(),
            ));
        }
        let connection = self.database.connection()?;
        let stored: String = connection.query_row(
            "SELECT identity_json FROM transfers WHERE transfer_id = ?1",
            [transfer_id],
            |row| row.get(0),
        )?;
        let identity: TransferIdentity = serde_json::from_str(&stored)?;
        let expected = TransferIdentity {
            file_key: request.file_key.clone(),
            expected_size: request.descriptor.expected_size,
            expected_md5: normalize_md5(request.descriptor.expected_md5.as_deref())?,
            validation: request.validation.clone(),
        };
        if identity != expected {
            return Err(TransferError::IdentityMismatch(
                "validation contract changed".to_owned(),
            ));
        }
        Ok(())
    }

    fn claim_attempt(&self, transfer_id: &str) -> Result<ClaimedAttempt, TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_transfer(&transaction, transfer_id)?;
        if raw.state == "succeeded" {
            return Err(TransferError::IdentityMismatch(
                "artifact is already available".to_owned(),
            ));
        }
        if matches!(
            raw.state.as_str(),
            "downloading" | "verifying" | "publishing"
        ) {
            return Err(TransferError::AlreadyRunning);
        }
        if active_consumer_count(&transaction, transfer_id)? == 0 {
            return Err(TransferError::NoConsumers);
        }
        let ordinal: u64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM transfer_attempts WHERE transfer_id = ?1",
            [transfer_id],
            |row| row.get(0),
        )?;
        let checkpoint = transaction
            .query_row(
                "SELECT part_path, downloaded_bytes, validator_kind, validator_value
                 FROM transfer_attempts
                 WHERE transfer_id = ?1 AND state = 'interrupted' AND downloaded_bytes > 0
                   AND validator_kind IS NOT NULL AND validator_value IS NOT NULL
                 ORDER BY ordinal DESC LIMIT 1",
                [transfer_id],
                |row| {
                    Ok(ResumeCheckpoint {
                        part_path: PathBuf::from(row.get::<_, String>(0)?),
                        downloaded_bytes: row.get(1)?,
                        validator_kind: row.get(2)?,
                        validator_value: row.get(3)?,
                    })
                },
            )
            .optional()?;
        let attempt_id = format!("attempt:{}", Uuid::new_v4());
        let part_path = self.staging.join(format!("{}.part", Uuid::new_v4()));
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO transfer_attempts(
               attempt_id, transfer_id, ordinal, state, part_path, downloaded_bytes,
               validator_kind, validator_value, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 'running', ?4, 0, NULL, NULL, ?5, ?5)",
            params![
                attempt_id,
                transfer_id,
                ordinal,
                part_path.to_string_lossy(),
                now
            ],
        )?;
        transaction.execute(
            "UPDATE transfers SET state = 'downloading', active_attempt_id = ?2,
               failure_code = NULL, updated_at_ms = ?3 WHERE transfer_id = ?1",
            params![transfer_id, attempt_id, now],
        )?;
        append_event(
            &transaction,
            transfer_id,
            Some(&attempt_id),
            "downloading",
            TransferState::Downloading,
            0,
            None,
        )?;
        transaction.commit()?;
        Ok(ClaimedAttempt {
            attempt_id,
            part_path,
            checkpoint,
        })
    }

    async fn execute_attempt(
        &self,
        transfer_id: &str,
        attempt: &ClaimedAttempt,
        request: &TransferRequest,
        cancellation: &CancellationToken,
    ) -> Result<ArtifactRef, TransferError> {
        if request
            .descriptor
            .expires_at
            .is_some_and(|expires| expires <= now_seconds().unwrap_or(i64::MAX))
        {
            return Err(TransferError::AuthorizationExpired);
        }
        let mut resume_offset = 0_u64;
        let mut resume_validator = None;
        if let Some(checkpoint) = &attempt.checkpoint {
            if checkpoint.downloaded_bytes <= self.config.max_bytes
                && checkpoint.part_path.is_file()
                && checkpoint.part_path.metadata()?.len() >= checkpoint.downloaded_bytes
            {
                copy_prefix(
                    &checkpoint.part_path,
                    &attempt.part_path,
                    checkpoint.downloaded_bytes,
                )
                .await?;
                resume_offset = checkpoint.downloaded_bytes;
                resume_validator = Some(ResponseValidator {
                    kind: checkpoint.validator_kind.clone(),
                    value: checkpoint.validator_value.clone(),
                });
            }
        }
        let mut stream = self
            .open_download_stream(
                request,
                resume_offset,
                resume_validator.as_ref(),
                cancellation,
            )
            .await?;
        let resume_accepted = resume_offset > 0
            && response_accepts_resume(&stream, resume_offset, resume_validator.as_ref());
        if resume_offset > 0 && !resume_accepted {
            resume_offset = 0;
            tokio::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&attempt.part_path)
                .await?;
            if stream.status != 200 {
                drop(stream);
                stream = self
                    .open_download_stream(request, 0, None, cancellation)
                    .await?;
            }
        }
        if (resume_offset == 0 && stream.status != 200)
            || (resume_offset > 0 && stream.status != 206)
        {
            return Err(TransferError::HttpStatus(stream.status));
        }
        let validator = response_validator(&stream.headers);
        self.update_attempt_checkpoint(
            transfer_id,
            &attempt.attempt_id,
            resume_offset,
            validator.as_ref(),
        )?;
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .append(resume_offset > 0)
            .truncate(resume_offset == 0)
            .create(resume_offset == 0)
            .open(&attempt.part_path)
            .await?;
        let mut downloaded = resume_offset;
        let mut next_progress = downloaded.saturating_add(self.config.progress_step_bytes);
        while let Some(chunk) = stream.next_chunk(cancellation).await? {
            if cancellation.is_cancelled() {
                return Err(TransferError::Cancelled);
            }
            downloaded = downloaded.checked_add(chunk.len() as u64).ok_or_else(|| {
                TransferError::SizeMismatch {
                    expected: self.config.max_bytes,
                    actual: u64::MAX,
                }
            })?;
            let byte_limit = if request.descriptor.expected_size > 0 {
                request.descriptor.expected_size
            } else {
                self.config.max_bytes
            };
            if downloaded > byte_limit {
                return Err(TransferError::SizeMismatch {
                    expected: byte_limit,
                    actual: downloaded,
                });
            }
            output.write_all(&chunk).await?;
            if downloaded >= next_progress {
                output.sync_data().await?;
                self.update_attempt_checkpoint(
                    transfer_id,
                    &attempt.attempt_id,
                    downloaded,
                    validator.as_ref(),
                )?;
                next_progress = downloaded.saturating_add(self.config.progress_step_bytes);
            }
        }
        output.sync_all().await?;
        if cancellation.is_cancelled() {
            return Err(TransferError::Cancelled);
        }
        self.update_attempt_checkpoint(
            transfer_id,
            &attempt.attempt_id,
            downloaded,
            validator.as_ref(),
        )?;
        if request.descriptor.expected_size > 0 && downloaded != request.descriptor.expected_size {
            return Err(TransferError::SizeMismatch {
                expected: request.descriptor.expected_size,
                actual: downloaded,
            });
        }
        self.mark_phase(
            transfer_id,
            &attempt.attempt_id,
            TransferState::Verifying,
            "verifying",
            downloaded,
            request.descriptor.expected_size,
        )?;
        let path = attempt.part_path.clone();
        let expected_md5 = normalize_md5(request.descriptor.expected_md5.as_deref())?;
        let prefix = request.validation.required_prefix.clone();
        tokio::task::spawn_blocking(move || verify_file(&path, expected_md5.as_deref(), &prefix))
            .await
            .map_err(|_| TransferError::Poisoned)??;
        if cancellation.is_cancelled() {
            return Err(TransferError::Cancelled);
        }
        self.mark_phase(
            transfer_id,
            &attempt.attempt_id,
            TransferState::Publishing,
            "publishing",
            downloaded,
            request.descriptor.expected_size,
        )?;
        let path = attempt.part_path.clone();
        let media_type = request.validation.media_type.clone();
        let artifacts = self.artifacts.clone();
        let artifact =
            tokio::task::spawn_blocking(move || artifacts.publish_blob_file(&path, media_type))
                .await
                .map_err(|_| TransferError::Poisoned)??;
        if request
            .validation
            .expected_sha256
            .as_deref()
            .is_some_and(|expected| expected != artifact.digest)
        {
            return Err(TransferError::ChecksumMismatch {
                algorithm: "sha256",
            });
        }
        self.mark_success(transfer_id, &attempt.attempt_id, &artifact, downloaded)?;
        self.remove_attempt_parts(transfer_id);
        Ok(artifact)
    }

    async fn open_download_stream(
        &self,
        request: &TransferRequest,
        resume_offset: u64,
        validator: Option<&ResponseValidator>,
        cancellation: &CancellationToken,
    ) -> Result<NetworkStreamResponse, TransferError> {
        let mut network = NetworkRequest::get(
            "runtime.transfer",
            request.descriptor.url.clone(),
            request.descriptor.allowed_origins.clone(),
        );
        network.headers = request.descriptor.headers.clone();
        if network
            .headers
            .keys()
            .any(|name| matches!(name.to_ascii_lowercase().as_str(), "range" | "if-range"))
        {
            return Err(TransferError::IdentityMismatch(
                "download descriptor cannot override resume headers".to_owned(),
            ));
        }
        network.credential = request.descriptor.credential.clone();
        network.max_response_bytes = usize::try_from(self.config.max_bytes).map_err(|_| {
            TransferError::IdentityMismatch("transfer size exceeds this platform".to_owned())
        })?;
        if resume_offset > 0 {
            let validator = validator.ok_or(TransferError::InvalidResume)?;
            network
                .headers
                .insert("range".to_owned(), format!("bytes={resume_offset}-"));
            network
                .headers
                .insert("if-range".to_owned(), validator.value.clone());
        }
        Ok(self.broker.open_stream(&network, cancellation).await?)
    }

    fn update_attempt_checkpoint(
        &self,
        transfer_id: &str,
        attempt_id: &str,
        downloaded_bytes: u64,
        validator: Option<&ResponseValidator>,
    ) -> Result<(), TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (kind, value) = validator
            .map(|validator| {
                (
                    Some(validator.kind.as_str()),
                    Some(validator.value.as_str()),
                )
            })
            .unwrap_or((None, None));
        let updated = transaction.execute(
            "UPDATE transfer_attempts SET downloaded_bytes = ?3, validator_kind = ?4,
               validator_value = ?5, updated_at_ms = ?6
             WHERE attempt_id = ?1 AND transfer_id = ?2 AND state = 'running'",
            params![
                attempt_id,
                transfer_id,
                downloaded_bytes,
                kind,
                value,
                now_ms()?
            ],
        )?;
        if updated != 1 {
            return Err(TransferError::AlreadyRunning);
        }
        let identity = load_identity(&transaction, transfer_id)?;
        append_event(
            &transaction,
            transfer_id,
            Some(attempt_id),
            "downloading",
            TransferState::Downloading,
            downloaded_bytes,
            Some(identity.expected_size),
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn mark_phase(
        &self,
        transfer_id: &str,
        attempt_id: &str,
        state: TransferState,
        phase: &str,
        downloaded: u64,
        total: u64,
    ) -> Result<(), TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_transfer(&transaction, transfer_id)?;
        if raw.active_attempt_id.as_deref() != Some(attempt_id) {
            return Err(TransferError::AlreadyRunning);
        }
        transaction.execute(
            "UPDATE transfers SET state = ?2, updated_at_ms = ?3 WHERE transfer_id = ?1",
            params![transfer_id, state_name(state), now_ms()?],
        )?;
        append_event(
            &transaction,
            transfer_id,
            Some(attempt_id),
            phase,
            state,
            downloaded,
            (total > 0).then_some(total),
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn mark_success(
        &self,
        transfer_id: &str,
        attempt_id: &str,
        artifact: &ArtifactRef,
        downloaded: u64,
    ) -> Result<(), TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_transfer(&transaction, transfer_id)?;
        if raw.active_attempt_id.as_deref() != Some(attempt_id) || raw.state != "publishing" {
            return Err(TransferError::AlreadyRunning);
        }
        let artifact_json = serde_json::to_string(artifact)?;
        let now = now_ms()?;
        transaction.execute(
            "UPDATE transfer_attempts SET state = 'succeeded', downloaded_bytes = ?3,
               updated_at_ms = ?4 WHERE attempt_id = ?1 AND transfer_id = ?2",
            params![attempt_id, transfer_id, downloaded, now],
        )?;
        transaction.execute(
            "UPDATE transfers SET state = 'succeeded', active_attempt_id = NULL,
               artifact_json = ?2, failure_code = NULL, updated_at_ms = ?3
             WHERE transfer_id = ?1",
            params![transfer_id, artifact_json, now],
        )?;
        let mut statement = transaction.prepare(
            "SELECT consumer_id FROM transfer_consumers WHERE transfer_id = ?1 AND state = 'active'",
        )?;
        let consumers = statement
            .query_map([transfer_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for consumer_id in consumers {
            insert_artifact_reference(
                &transaction,
                &consumer_reference_id(transfer_id, &consumer_id),
                transfer_id,
                &artifact.digest,
                "transfer-consumer",
            )?;
        }
        append_event(
            &transaction,
            transfer_id,
            Some(attempt_id),
            "succeeded",
            TransferState::Succeeded,
            downloaded,
            Some(downloaded),
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn fail_attempt(
        &self,
        transfer_id: &str,
        attempt_id: &str,
        error: &TransferError,
    ) -> Result<(), TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_transfer(&transaction, transfer_id)?;
        if raw.active_attempt_id.as_deref() != Some(attempt_id) {
            transaction.commit()?;
            return Ok(());
        }
        let cancelled = matches!(error, TransferError::Cancelled)
            || active_consumer_count(&transaction, transfer_id)? == 0;
        let attempt_state = if is_resumable_error(error) {
            "interrupted"
        } else {
            "failed"
        };
        let transfer_state = if cancelled {
            TransferState::Cancelled
        } else {
            TransferState::Failed
        };
        let failure_code = error_code(error);
        let downloaded: u64 = transaction.query_row(
            "SELECT downloaded_bytes FROM transfer_attempts WHERE attempt_id = ?1",
            [attempt_id],
            |row| row.get(0),
        )?;
        let now = now_ms()?;
        transaction.execute(
            "UPDATE transfer_attempts SET state = ?2, updated_at_ms = ?3 WHERE attempt_id = ?1",
            params![attempt_id, attempt_state, now],
        )?;
        transaction.execute(
            "UPDATE transfers SET state = ?2, active_attempt_id = NULL,
               failure_code = ?3, updated_at_ms = ?4 WHERE transfer_id = ?1",
            params![transfer_id, state_name(transfer_state), failure_code, now],
        )?;
        append_event(
            &transaction,
            transfer_id,
            Some(attempt_id),
            if cancelled { "cancelled" } else { "failed" },
            transfer_state,
            downloaded,
            None,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn remove_attempt_parts(&self, transfer_id: &str) {
        let Ok(connection) = self.database.connection() else {
            return;
        };
        let Ok(mut statement) =
            connection.prepare("SELECT part_path FROM transfer_attempts WHERE transfer_id = ?1")
        else {
            return;
        };
        let Ok(paths) = statement.query_map([transfer_id], |row| row.get::<_, String>(0)) else {
            return;
        };
        for path in paths.flatten() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn recover_incomplete(&self) -> Result<(), TransferError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT transfer_id, active_attempt_id FROM transfers
             WHERE state IN ('downloading', 'verifying', 'publishing')",
        )?;
        let incomplete = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (transfer_id, attempt_id) in incomplete {
            if let Some(attempt_id) = &attempt_id {
                transaction.execute(
                    "UPDATE transfer_attempts SET state = 'interrupted', updated_at_ms = ?2
                     WHERE attempt_id = ?1 AND state = 'running'",
                    params![attempt_id, now_ms()?],
                )?;
            }
            transaction.execute(
                "UPDATE transfers SET state = 'queued', active_attempt_id = NULL,
                   failure_code = 'RECOVERED_INCOMPLETE', updated_at_ms = ?2
                 WHERE transfer_id = ?1",
                params![transfer_id, now_ms()?],
            )?;
            append_event(
                &transaction,
                &transfer_id,
                attempt_id.as_deref(),
                "recovered",
                TransferState::Queued,
                0,
                None,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

impl RawTransfer {
    fn into_snapshot(self) -> Result<TransferSnapshot, TransferError> {
        let identity: TransferIdentity = serde_json::from_str(&self.identity_json)?;
        Ok(TransferSnapshot {
            transfer_id: self.transfer_id,
            file_key: identity.file_key,
            state: parse_state(&self.state)?,
            active_attempt_id: self.active_attempt_id,
            artifact: self
                .artifact_json
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
            failure_code: self.failure_code,
            last_seq: self.last_seq,
        })
    }
}

fn validate_request(
    request: &TransferRequest,
    config: TransferConfig,
) -> Result<(), TransferError> {
    let key = &request.file_key;
    if [
        key.source_id.as_str(),
        key.remote_game_key.as_str(),
        key.remote_mod_id.as_str(),
        key.resolved_version_id.as_str(),
        key.remote_file_id.as_str(),
        key.byte_variant.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(TransferError::IdentityMismatch(
            "stable file key fields cannot be empty".to_owned(),
        ));
    }
    let descriptor = &request.descriptor;
    if descriptor.mod_ref.source_id != key.source_id
        || descriptor.mod_ref.remote_game_key != key.remote_game_key
        || descriptor.mod_ref.remote_mod_id != key.remote_mod_id
        || descriptor.mod_ref.version_id.as_deref() != Some(key.resolved_version_id.as_str())
        || descriptor.mod_ref.remote_file_id.as_deref() != Some(key.remote_file_id.as_str())
    {
        return Err(TransferError::IdentityMismatch(
            "download descriptor identity differs from the transfer key".to_owned(),
        ));
    }
    if descriptor.expected_size > config.max_bytes {
        return Err(TransferError::SizeMismatch {
            expected: config.max_bytes,
            actual: descriptor.expected_size,
        });
    }
    normalize_md5(descriptor.expected_md5.as_deref())?;
    if let Some(expected) = &request.validation.expected_sha256 {
        validate_sha256(expected)?;
    }
    if request.validation.required_prefix.len() > 64 * 1024 {
        return Err(TransferError::IdentityMismatch(
            "required content prefix is too large".to_owned(),
        ));
    }
    Ok(())
}

fn identity_digest(
    identity: &TransferIdentity,
    sharing_salt: Option<&str>,
) -> Result<String, TransferError> {
    let bytes = serde_json::to_vec(&(identity, sharing_salt))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn normalize_md5(value: Option<&str>) -> Result<Option<String>, TransferError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized.len() != 32 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(TransferError::IdentityMismatch(
                    "expected MD5 must be 32 hexadecimal characters".to_owned(),
                ));
            }
            Ok(normalized)
        })
        .transpose()
}

fn validate_sha256(value: &str) -> Result<(), TransferError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(TransferError::IdentityMismatch(
            "expected SHA-256 must use the sha256: prefix".to_owned(),
        ));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TransferError::IdentityMismatch(
            "expected SHA-256 must contain 64 hexadecimal characters".to_owned(),
        ));
    }
    Ok(())
}

async fn copy_prefix(source: &Path, destination: &Path, bytes: u64) -> Result<(), TransferError> {
    let input = tokio::fs::File::open(source).await?;
    let mut output = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await?;
    let copied = tokio::io::copy(&mut input.take(bytes), &mut output).await?;
    if copied != bytes {
        return Err(TransferError::InvalidResume);
    }
    output.sync_all().await?;
    Ok(())
}

fn response_validator(headers: &BTreeMap<String, String>) -> Option<ResponseValidator> {
    headers
        .get("etag")
        .filter(|value| !value.trim().is_empty() && !value.trim_start().starts_with("W/"))
        .map(|value| ResponseValidator {
            kind: "etag".to_owned(),
            value: value.clone(),
        })
        .or_else(|| {
            headers
                .get("last-modified")
                .filter(|value| !value.trim().is_empty())
                .map(|value| ResponseValidator {
                    kind: "last-modified".to_owned(),
                    value: value.clone(),
                })
        })
}

fn response_accepts_resume(
    response: &NetworkStreamResponse,
    offset: u64,
    expected: Option<&ResponseValidator>,
) -> bool {
    if response.status != 206 || response_validator(&response.headers).as_ref() != expected {
        return false;
    }
    response
        .headers
        .get("content-range")
        .and_then(|value| parse_content_range_start(value))
        == Some(offset)
}

fn parse_content_range_start(value: &str) -> Option<u64> {
    value
        .strip_prefix("bytes ")?
        .split_once('-')?
        .0
        .parse()
        .ok()
}

fn verify_file(
    path: &Path,
    expected_md5: Option<&str>,
    required_prefix: &[u8],
) -> Result<(), TransferError> {
    let mut input = File::open(path)?;
    let mut prefix = vec![0_u8; required_prefix.len()];
    if !prefix.is_empty() {
        input.read_exact(&mut prefix)?;
        if prefix != required_prefix {
            return Err(TransferError::FormatMismatch);
        }
    }
    if let Some(expected_md5) = expected_md5 {
        let mut hasher = Md5::new();
        hasher.update(&prefix);
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected_md5 {
            return Err(TransferError::ChecksumMismatch { algorithm: "md5" });
        }
    }
    Ok(())
}

fn raw_transfer(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawTransfer> {
    Ok(RawTransfer {
        transfer_id: row.get(0)?,
        identity_json: row.get(1)?,
        state: row.get(2)?,
        active_attempt_id: row.get(3)?,
        artifact_json: row.get(4)?,
        failure_code: row.get(5)?,
        last_seq: row.get(6)?,
    })
}

fn load_transfer(
    connection: &rusqlite::Connection,
    transfer_id: &str,
) -> Result<RawTransfer, TransferError> {
    connection
        .query_row(
            "SELECT transfer_id, identity_json, state, active_attempt_id, artifact_json,
                    failure_code, last_seq FROM transfers WHERE transfer_id = ?1",
            [transfer_id],
            raw_transfer,
        )
        .optional()?
        .ok_or_else(|| TransferError::NotFound(transfer_id.to_owned()))
}

fn load_identity(
    connection: &rusqlite::Connection,
    transfer_id: &str,
) -> Result<TransferIdentity, TransferError> {
    let value: String = connection.query_row(
        "SELECT identity_json FROM transfers WHERE transfer_id = ?1",
        [transfer_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&value)?)
}

fn active_consumer_count(
    connection: &rusqlite::Connection,
    transfer_id: &str,
) -> Result<u64, TransferError> {
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM transfer_consumers WHERE transfer_id = ?1 AND state = 'active'",
        [transfer_id],
        |row| row.get(0),
    )?)
}

fn append_event(
    transaction: &Transaction<'_>,
    transfer_id: &str,
    attempt_id: Option<&str>,
    phase: &str,
    state: TransferState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
) -> Result<(), TransferError> {
    let last_seq: u64 = transaction.query_row(
        "SELECT last_seq FROM transfers WHERE transfer_id = ?1",
        [transfer_id],
        |row| row.get(0),
    )?;
    let seq = last_seq
        .checked_add(1)
        .ok_or_else(|| TransferError::IdentityMismatch("event sequence overflow".to_owned()))?;
    let now = now_ms()?;
    transaction.execute(
        "UPDATE transfers SET last_seq = ?2, updated_at_ms = ?3 WHERE transfer_id = ?1",
        params![transfer_id, seq, now],
    )?;
    transaction.execute(
        "INSERT INTO transfer_events(
           transfer_id, seq, attempt_id, phase, state, downloaded_bytes, total_bytes, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            transfer_id,
            seq,
            attempt_id,
            phase,
            state_name(state),
            downloaded_bytes,
            total_bytes,
            now
        ],
    )?;
    Ok(())
}

fn insert_artifact_reference(
    transaction: &Transaction<'_>,
    reference_id: &str,
    transfer_id: &str,
    digest: &str,
    kind: &str,
) -> Result<(), TransferError> {
    transaction.execute(
        "INSERT INTO transfer_artifact_references(reference_id, transfer_id, digest, kind, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(reference_id) DO UPDATE SET
           transfer_id = excluded.transfer_id, digest = excluded.digest, kind = excluded.kind",
        params![reference_id, transfer_id, digest, kind, now_ms()?],
    )?;
    Ok(())
}

fn consumer_reference_id(transfer_id: &str, consumer_id: &str) -> String {
    format!("transfer-consumer:{transfer_id}:{consumer_id}")
}

fn parse_state(value: &str) -> Result<TransferState, TransferError> {
    match value {
        "queued" => Ok(TransferState::Queued),
        "downloading" => Ok(TransferState::Downloading),
        "verifying" => Ok(TransferState::Verifying),
        "publishing" => Ok(TransferState::Publishing),
        "succeeded" => Ok(TransferState::Succeeded),
        "failed" => Ok(TransferState::Failed),
        "cancelled" => Ok(TransferState::Cancelled),
        _ => Err(TransferError::IdentityMismatch(format!(
            "unknown transfer state {value}"
        ))),
    }
}

fn state_name(state: TransferState) -> &'static str {
    match state {
        TransferState::Queued => "queued",
        TransferState::Downloading => "downloading",
        TransferState::Verifying => "verifying",
        TransferState::Publishing => "publishing",
        TransferState::Succeeded => "succeeded",
        TransferState::Failed => "failed",
        TransferState::Cancelled => "cancelled",
    }
}

fn is_resumable_error(error: &TransferError) -> bool {
    matches!(
        error,
        TransferError::Broker(BrokerError::Cancelled { .. })
            | TransferError::Broker(BrokerError::Timeout { .. })
            | TransferError::Broker(BrokerError::Transport { .. })
            | TransferError::Cancelled
            | TransferError::Io(_)
    )
}

fn error_code(error: &TransferError) -> &'static str {
    match error {
        TransferError::AuthorizationExpired => "AUTHORIZATION_EXPIRED",
        TransferError::HttpStatus(_) => "HTTP_STATUS",
        TransferError::InvalidResume => "INVALID_RESUME",
        TransferError::SizeMismatch { .. } => "SIZE_MISMATCH",
        TransferError::ChecksumMismatch { .. } => "CHECKSUM_MISMATCH",
        TransferError::FormatMismatch => "FORMAT_MISMATCH",
        TransferError::Cancelled | TransferError::Broker(BrokerError::Cancelled { .. }) => {
            "CANCELLED"
        }
        TransferError::Broker(_) => "NETWORK_FAILED",
        TransferError::Io(_) => "IO_FAILED",
        _ => "TRANSFER_FAILED",
    }
}

fn now_ms() -> Result<i64, TransferError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| TransferError::IdentityMismatch(error.to_string()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| TransferError::IdentityMismatch("timestamp overflow".to_owned()))
}

fn now_seconds() -> Result<i64, TransferError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| TransferError::IdentityMismatch(error.to_string()))?;
    i64::try_from(elapsed.as_secs())
        .map_err(|_| TransferError::IdentityMismatch("timestamp overflow".to_owned()))
}

#[cfg(test)]
mod tests;
