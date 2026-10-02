use std::{
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_runtime_kernel::{KernelDatabase, KernelDatabaseError};
use mintcat_sdk::{
    ActionRequired, ActionState, AppError, JobEvent, JobEventPage, JobRef, JobSnapshot, JobState,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

const MIGRATION: &str = r#"
CREATE TABLE jobs (
  job_id TEXT PRIMARY KEY,
  command TEXT NOT NULL,
  owner_scope TEXT NOT NULL,
  state TEXT NOT NULL,
  generation_id TEXT NOT NULL,
  last_seq INTEGER NOT NULL,
  target_json TEXT NOT NULL,
  confirmation_policy TEXT NOT NULL,
  result_json TEXT,
  error_json TEXT,
  checkpoint_step TEXT,
  checkpoint_json TEXT,
  resume_policy TEXT,
  commit_state TEXT NOT NULL,
  cancel_requested INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX jobs_owner_updated ON jobs(owner_scope, updated_at_ms DESC);

CREATE TABLE job_events (
  job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  generation_id TEXT NOT NULL,
  phase TEXT NOT NULL,
  state TEXT NOT NULL,
  completed_units INTEGER,
  total_units INTEGER,
  message_key TEXT,
  details_json TEXT,
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (job_id, seq)
);

CREATE TABLE operations (
  owner_scope TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  command TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  job_id TEXT NOT NULL REFERENCES jobs(job_id),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (owner_scope, idempotency_key, command)
);

CREATE TABLE actions (
  action_id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES jobs(job_id),
  owner_scope TEXT NOT NULL,
  revision INTEGER NOT NULL,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  operation_id TEXT NOT NULL,
  target_json TEXT NOT NULL,
  plan_hash TEXT NOT NULL,
  module_lock_digest TEXT NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  response_schema_json TEXT NOT NULL,
  response_json TEXT,
  approved_at_ms INTEGER,
  consumed_at_ms INTEGER
);
CREATE INDEX actions_job_state ON actions(job_id, state);
"#;

const MIGRATION_2: &str =
    "ALTER TABLE actions ADD COLUMN target_revision INTEGER NOT NULL DEFAULT 0;";

#[derive(Debug, Error)]
pub enum JobError {
    #[error(transparent)]
    Kernel(#[from] KernelDatabaseError),
    #[error("job database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("job JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("job or action was not found: {0}")]
    NotFound(String),
    #[error("operation idempotency key was reused with different input")]
    IdempotencyConflict,
    #[error("revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("invalid job transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
    #[error("action is not available in state {0}")]
    ActionState(String),
    #[error("action expired")]
    ActionExpired,
    #[error("action response is invalid: {0}")]
    InvalidResponse(String),
    #[error("action binding no longer matches the target, plan, or module lock")]
    StalePlan,
    #[error("job belongs to another owner scope")]
    PermissionDenied,
    #[error("event retention must be at least one")]
    InvalidRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    None,
    Policy,
    Required,
}

impl ConfirmationPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Policy => "policy",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResumePolicy {
    ResumeFromCheckpoint,
    InspectBeforeResume,
}

impl ResumePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::ResumeFromCheckpoint => "resume-from-checkpoint",
            Self::InspectBeforeResume => "inspect-before-resume",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "resume-from-checkpoint" => Some(Self::ResumeFromCheckpoint),
            "inspect-before-resume" => Some(Self::InspectBeforeResume),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitState {
    NotStarted,
    InProgress,
    Committed,
    Unknown,
}

impl CommitState {
    fn parse(value: &str) -> Self {
        match value {
            "in-progress" => Self::InProgress,
            "committed" => Self::Committed,
            "unknown" => Self::Unknown,
            _ => Self::NotStarted,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegisterJob {
    pub command: String,
    pub owner_scope: String,
    pub idempotency_key: String,
    pub request_hash: String,
    pub target: Value,
    pub confirmation_policy: ConfirmationPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRegistration {
    pub job: JobRef,
    pub created: bool,
}

#[derive(Debug, Clone)]
pub struct EventUpdate {
    pub phase: String,
    pub state: JobState,
    pub completed_units: Option<u64>,
    pub total_units: Option<u64>,
    pub message_key: Option<String>,
    pub details: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<AppError>,
}

impl EventUpdate {
    pub fn state(phase: impl Into<String>, state: JobState) -> Self {
        Self {
            phase: phase.into(),
            state,
            completed_units: None,
            total_units: None,
            message_key: None,
            details: None,
            result: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerCheckpoint {
    pub step: String,
    pub payload: Value,
    pub resume_policy: ResumePolicy,
}

#[derive(Debug, Clone)]
pub struct ActionSpec {
    pub kind: String,
    pub operation_id: String,
    pub target: Value,
    pub target_revision: u64,
    pub plan_hash: String,
    pub module_lock_digest: String,
    pub expires_at_ms: i64,
    pub response_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ActionBinding {
    pub target: Value,
    pub target_revision: u64,
    pub plan_hash: String,
    pub module_lock_digest: String,
}

#[derive(Debug, Clone)]
pub struct ConsumedAction {
    pub job: JobRef,
    pub action: ActionRequired,
    pub response: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDisposition {
    ResumeCheckpoint,
    AwaitingAction,
    InspectCommit,
    NoSafeCheckpoint,
}

#[derive(Debug, Clone)]
pub struct RecoveryRecord {
    pub snapshot: JobSnapshot,
    pub checkpoint: Option<WorkerCheckpoint>,
    pub disposition: RecoveryDisposition,
}

#[derive(Clone)]
pub struct JobStore {
    database: KernelDatabase,
    generation_id: String,
    retention_limit: u64,
}

impl std::fmt::Debug for JobStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobStore")
            .field("database", &self.database)
            .field("generation_id", &self.generation_id)
            .field("retention_limit", &self.retention_limit)
            .finish()
    }
}

impl JobStore {
    pub fn open(
        database: KernelDatabase,
        generation_id: impl Into<String>,
        retention_limit: u64,
    ) -> Result<Self, JobError> {
        if retention_limit == 0 {
            return Err(JobError::InvalidRetention);
        }
        database.apply_migration("jobs", 1, MIGRATION)?;
        database.apply_migration("jobs", 2, MIGRATION_2)?;
        Ok(Self {
            database,
            generation_id: generation_id.into(),
            retention_limit,
        })
    }

    pub fn database(&self) -> &KernelDatabase {
        &self.database
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn register_job(&self, request: RegisterJob) -> Result<JobRegistration, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((request_hash, job_id)) = find_operation(
            &transaction,
            &request.owner_scope,
            &request.idempotency_key,
            &request.command,
        )? {
            if request_hash != request.request_hash {
                return Err(JobError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(JobRegistration {
                job: JobRef { job_id },
                created: false,
            });
        }

        let job_id = Uuid::new_v4().to_string();
        let now = now_ms();
        let target = serde_json::to_string(&request.target)?;
        transaction.execute(
            "INSERT INTO jobs(
               job_id, command, owner_scope, state, generation_id, last_seq, target_json,
               confirmation_policy, commit_state, cancel_requested, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 'queued', ?4, 0, ?5, ?6, 'not-started', 0, ?7, ?7)",
            params![
                job_id,
                request.command,
                request.owner_scope,
                self.generation_id,
                target,
                request.confirmation_policy.as_str(),
                now,
            ],
        )?;
        transaction.execute(
            "INSERT INTO job_events(
               job_id, seq, generation_id, phase, state, created_at_ms
             ) VALUES (?1, 0, ?2, 'queued', 'queued', ?3)",
            params![job_id, self.generation_id, now],
        )?;
        insert_operation(
            &transaction,
            &request.owner_scope,
            &request.idempotency_key,
            &request.command,
            &request.request_hash,
            &job_id,
            now,
        )?;
        transaction.commit()?;
        Ok(JobRegistration {
            job: JobRef { job_id },
            created: true,
        })
    }

    pub fn find_operation(
        &self,
        owner_scope: &str,
        command: &str,
        idempotency_key: &str,
    ) -> Result<Option<JobRef>, JobError> {
        let connection = self.database.connection()?;
        Ok(
            find_operation(&connection, owner_scope, idempotency_key, command)?
                .map(|(_, job_id)| JobRef { job_id }),
        )
    }

    pub fn get_job(&self, owner_scope: &str, job_id: &str) -> Result<JobSnapshot, JobError> {
        let connection = self.database.connection()?;
        let raw = load_job(&connection, job_id)?;
        ensure_owner(&raw, owner_scope)?;
        raw.into_snapshot()
    }

    pub fn commit_state(&self, owner_scope: &str, job_id: &str) -> Result<CommitState, JobError> {
        let connection = self.database.connection()?;
        let raw = load_job(&connection, job_id)?;
        ensure_owner(&raw, owner_scope)?;
        Ok(raw.commit_state)
    }

    pub fn get_job_target(&self, owner_scope: &str, job_id: &str) -> Result<Value, JobError> {
        let connection = self.database.connection()?;
        let (stored_owner, target_json) = connection
            .query_row(
                "SELECT owner_scope, target_json FROM jobs WHERE job_id = ?1",
                [job_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| JobError::NotFound(job_id.to_owned()))?;
        if stored_owner != owner_scope {
            return Err(JobError::PermissionDenied);
        }
        Ok(serde_json::from_str(&target_json)?)
    }

    pub fn events_after(
        &self,
        owner_scope: &str,
        job_id: &str,
        after_seq: u64,
        limit: u64,
    ) -> Result<JobEventPage, JobError> {
        let connection = self.database.connection()?;
        let raw = load_job(&connection, job_id)?;
        ensure_owner(&raw, owner_scope)?;
        let snapshot = raw.into_snapshot()?;
        let retention_start: u64 = connection.query_row(
            "SELECT COALESCE(MIN(seq), ?2) FROM job_events WHERE job_id = ?1",
            params![job_id, snapshot.last_seq],
            |row| row.get(0),
        )?;
        let reset_required = after_seq.saturating_add(1) < retention_start;
        let effective_after = if reset_required {
            retention_start.saturating_sub(1)
        } else {
            after_seq
        };
        let limit = limit.clamp(1, 1000);
        let mut statement = connection.prepare(
            "SELECT job_id, seq, generation_id, phase, state, completed_units, total_units,
                    message_key, details_json
             FROM job_events WHERE job_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = statement.query_map(params![job_id, effective_after, limit], raw_event)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?.into_event()?);
        }
        let next_after_seq = events
            .last()
            .map(|event| event.seq)
            .unwrap_or(if reset_required {
                snapshot.last_seq
            } else {
                after_seq
            });
        Ok(JobEventPage {
            snapshot,
            events,
            next_after_seq,
            retention_start_seq: retention_start,
            reset_required,
        })
    }

    pub fn append_event(&self, job_id: &str, update: EventUpdate) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let snapshot = self.transition(&transaction, job_id, update)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn save_checkpoint(
        &self,
        job_id: &str,
        expected_seq: u64,
        checkpoint: &WorkerCheckpoint,
    ) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_job(&transaction, job_id)?;
        if raw.last_seq != expected_seq {
            return Err(JobError::RevisionConflict {
                expected: expected_seq,
                actual: raw.last_seq,
            });
        }
        if is_terminal(&raw.state) {
            return Err(JobError::InvalidTransition {
                from: raw.state,
                to: "checkpoint".to_owned(),
            });
        }
        transaction.execute(
            "UPDATE jobs SET checkpoint_step = ?2, checkpoint_json = ?3, resume_policy = ?4
             WHERE job_id = ?1",
            params![
                job_id,
                checkpoint.step,
                serde_json::to_string(&checkpoint.payload)?,
                checkpoint.resume_policy.as_str(),
            ],
        )?;
        let snapshot = self.transition(
            &transaction,
            job_id,
            EventUpdate {
                phase: "checkpoint-saved".to_owned(),
                state: parse_job_state(&raw.state)?,
                completed_units: None,
                total_units: None,
                message_key: Some("job.checkpoint.saved".to_owned()),
                details: Some(json!({ "step": checkpoint.step })),
                result: None,
                error: None,
            },
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn request_cancel(
        &self,
        owner_scope: &str,
        job_id: &str,
        expected_revision: u64,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((existing_hash, existing_job_id)) =
            find_operation(&transaction, owner_scope, idempotency_key, "jobs.cancel")?
        {
            if existing_hash != request_hash || existing_job_id != job_id {
                return Err(JobError::IdempotencyConflict);
            }
            let snapshot = load_job(&transaction, job_id)?.into_snapshot()?;
            transaction.commit()?;
            return Ok(snapshot);
        }
        let raw = load_job(&transaction, job_id)?;
        ensure_owner(&raw, owner_scope)?;
        if raw.last_seq != expected_revision {
            return Err(JobError::RevisionConflict {
                expected: expected_revision,
                actual: raw.last_seq,
            });
        }
        transaction.execute(
            "UPDATE jobs SET cancel_requested = 1 WHERE job_id = ?1",
            [job_id],
        )?;
        let snapshot = if is_terminal(&raw.state) {
            raw.into_snapshot()?
        } else {
            let deferred = raw.state == "committing";
            self.transition(
                &transaction,
                job_id,
                EventUpdate {
                    phase: "cancel-requested".to_owned(),
                    state: if deferred {
                        JobState::Committing
                    } else {
                        JobState::Cancelling
                    },
                    completed_units: None,
                    total_units: None,
                    message_key: Some("job.cancel.requested".to_owned()),
                    details: Some(json!({ "deferredUntilSafePoint": deferred })),
                    result: None,
                    error: None,
                },
            )?
        };
        insert_operation(
            &transaction,
            owner_scope,
            idempotency_key,
            "jobs.cancel",
            request_hash,
            job_id,
            now_ms(),
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn cancel_at_safe_point(&self, job_id: &str) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_job(&transaction, job_id)?;
        if raw.cancel_requested == 0 || is_terminal(&raw.state) {
            let snapshot = raw.into_snapshot()?;
            transaction.commit()?;
            return Ok(snapshot);
        }
        if matches!(
            raw.commit_state,
            CommitState::InProgress | CommitState::Unknown
        ) {
            let snapshot = raw.into_snapshot()?;
            transaction.commit()?;
            return Ok(snapshot);
        }
        let snapshot = self.transition(
            &transaction,
            job_id,
            EventUpdate::state("cancelled-at-safe-point", JobState::Cancelled),
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn begin_commit(&self, job_id: &str) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE jobs SET commit_state = 'in-progress' WHERE job_id = ?1",
            [job_id],
        )?;
        let snapshot = self.transition(
            &transaction,
            job_id,
            EventUpdate::state("commit-started", JobState::Committing),
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn finish_commit(&self, job_id: &str, result: Value) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE jobs SET commit_state = 'committed' WHERE job_id = ?1",
            [job_id],
        )?;
        let mut update = EventUpdate::state("commit-finished", JobState::Succeeded);
        update.result = Some(result);
        let snapshot = self.transition(&transaction, job_id, update)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn finish_recovered_commit(
        &self,
        job_id: &str,
        result: Value,
    ) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_job(&transaction, job_id)?;
        if raw.state != "recovery-required" || raw.commit_state != CommitState::Unknown {
            return Err(JobError::InvalidTransition {
                from: raw.state,
                to: "recovered-commit-finished".to_owned(),
            });
        }
        transaction.execute(
            "UPDATE jobs SET commit_state = 'committed' WHERE job_id = ?1",
            [job_id],
        )?;
        let mut update = EventUpdate::state("recovered-commit-finished", JobState::Succeeded);
        update.result = Some(result);
        let snapshot = self.transition(&transaction, job_id, update)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    /// Only a journal owner may call this after proving that no commit decision
    /// survived (the transaction never started or its rollback completed).
    pub fn finish_uncommitted(
        &self,
        job_id: &str,
        failure: Option<AppError>,
    ) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_job(&transaction, job_id)?;
        if !matches!(raw.commit_state, CommitState::InProgress | CommitState::Unknown) {
            return Err(JobError::InvalidTransition {
                from: raw.state,
                to: "verified-uncommitted".to_owned(),
            });
        }
        transaction.execute(
            "UPDATE jobs SET commit_state = 'not-started' WHERE job_id = ?1",
            [job_id],
        )?;
        let mut update = EventUpdate::state(
            "verified-uncommitted",
            if failure.is_some() { JobState::Failed } else { JobState::Cancelled },
        );
        update.error = failure;
        let snapshot = self.transition(&transaction, job_id, update)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn mark_commit_unknown(&self, job_id: &str) -> Result<JobSnapshot, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE jobs SET commit_state = 'unknown' WHERE job_id = ?1",
            [job_id],
        )?;
        let snapshot = self.transition(
            &transaction,
            job_id,
            EventUpdate::state("commit-state-unknown", JobState::RecoveryRequired),
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn create_action(
        &self,
        job_id: &str,
        spec: ActionSpec,
    ) -> Result<ActionRequired, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_job(&transaction, job_id)?;
        if raw.confirmation_policy == "none" || is_terminal(&raw.state) {
            return Err(JobError::InvalidTransition {
                from: raw.state,
                to: "awaiting-approval".to_owned(),
            });
        }
        let action_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO actions(
               action_id, job_id, owner_scope, revision, kind, state, operation_id, target_json,
               target_revision, plan_hash, module_lock_digest, expires_at_ms, response_schema_json
             ) VALUES (?1, ?2, ?3, 0, ?4, 'pending', ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                action_id,
                job_id,
                raw.owner_scope,
                spec.kind,
                spec.operation_id,
                serde_json::to_string(&spec.target)?,
                spec.target_revision,
                spec.plan_hash,
                spec.module_lock_digest,
                spec.expires_at_ms,
                serde_json::to_string(&spec.response_schema)?,
            ],
        )?;
        self.transition(
            &transaction,
            job_id,
            EventUpdate {
                phase: "action-required".to_owned(),
                state: JobState::AwaitingApproval,
                completed_units: None,
                total_units: None,
                message_key: Some("job.action.required".to_owned()),
                details: Some(json!({ "actionId": action_id })),
                result: None,
                error: None,
            },
        )?;
        let action = load_action(&transaction, &action_id)?.into_action()?;
        transaction.commit()?;
        Ok(action)
    }

    pub fn get_action(
        &self,
        owner_scope: &str,
        action_id: &str,
    ) -> Result<ActionRequired, JobError> {
        let connection = self.database.connection()?;
        let raw = load_action(&connection, action_id)?;
        if raw.owner_scope != owner_scope {
            return Err(JobError::PermissionDenied);
        }
        raw.into_action()
    }

    /// The newest action belonging to this job, regardless of its approval state.
    pub fn get_job_action(
        &self,
        owner_scope: &str,
        job_id: &str,
    ) -> Result<Option<ActionRequired>, JobError> {
        let connection = self.database.connection()?;
        ensure_owner(&load_job(&connection, job_id)?, owner_scope)?;
        let action_id: Option<String> = connection.query_row(
            "SELECT action_id FROM actions WHERE job_id = ?1 ORDER BY rowid DESC LIMIT 1",
            [job_id],
            |row| row.get(0),
        ).optional()?;
        action_id.map(|action_id| {
            let action = load_action(&connection, &action_id)?;
            if action.owner_scope != owner_scope {
                return Err(JobError::PermissionDenied);
            }
            action.into_action()
        }).transpose()
    }

    pub fn action_job(&self, owner_scope: &str, action_id: &str) -> Result<JobRef, JobError> {
        let connection = self.database.connection()?;
        let action = load_action(&connection, action_id)?;
        if action.owner_scope != owner_scope { return Err(JobError::PermissionDenied); }
        ensure_owner(&load_job(&connection, &action.job_id)?, owner_scope)?;
        Ok(JobRef { job_id: action.job_id })
    }

    pub fn cancellation_requested(&self, owner_scope: &str, job_id: &str) -> Result<bool, JobError> {
        let connection = self.database.connection()?;
        let flags: Option<(bool, bool)> = connection.query_row(
            "SELECT owner_scope = ?2, cancel_requested <> 0 FROM jobs WHERE job_id = ?1",
            params![job_id, owner_scope], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        match flags {
            Some((true, cancelled)) => Ok(cancelled),
            Some((false, _)) => Err(JobError::PermissionDenied),
            None => Err(JobError::NotFound(job_id.to_owned())),
        }
    }

    pub fn approve_action(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        response: &Value,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<JobRef, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((existing_hash, job_id)) = find_operation(
            &transaction,
            owner_scope,
            idempotency_key,
            "actions.approve",
        )? {
            if existing_hash != request_hash {
                return Err(JobError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(JobRef { job_id });
        }
        let raw = load_action(&transaction, action_id)?;
        if raw.owner_scope != owner_scope {
            return Err(JobError::PermissionDenied);
        }
        if raw.revision != expected_revision {
            return Err(JobError::RevisionConflict {
                expected: expected_revision,
                actual: raw.revision,
            });
        }
        if raw.state != "pending" {
            return Err(JobError::ActionState(raw.state));
        }
        if raw.expires_at_ms <= now_ms() {
            transaction.execute(
                "UPDATE actions SET state = 'expired', revision = revision + 1 WHERE action_id = ?1",
                [action_id],
            )?;
            transaction.commit()?;
            return Err(JobError::ActionExpired);
        }
        let schema: Value = serde_json::from_str(&raw.response_schema_json)?;
        validate_response(&schema, response)?;
        transaction.execute(
            "UPDATE actions SET state = 'approved', revision = revision + 1,
               response_json = ?2, approved_at_ms = ?3 WHERE action_id = ?1",
            params![action_id, serde_json::to_string(response)?, now_ms()],
        )?;
        let current = load_job(&transaction, &raw.job_id)?;
        self.transition(
            &transaction,
            &raw.job_id,
            EventUpdate {
                phase: "action-approved".to_owned(),
                state: parse_job_state(&current.state)?,
                completed_units: None,
                total_units: None,
                message_key: Some("job.action.approved".to_owned()),
                details: Some(json!({ "actionId": action_id })),
                result: None,
                error: None,
            },
        )?;
        insert_operation(
            &transaction,
            owner_scope,
            idempotency_key,
            "actions.approve",
            request_hash,
            &raw.job_id,
            now_ms(),
        )?;
        transaction.commit()?;
        Ok(JobRef { job_id: raw.job_id })
    }
    pub fn reject_action(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        reason: &str,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<JobRef, JobError> {
        if reason.trim().is_empty() {
            return Err(JobError::InvalidResponse(
                "action rejection reason cannot be empty".to_owned(),
            ));
        }
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((existing_hash, job_id)) =
            find_operation(&transaction, owner_scope, idempotency_key, "actions.reject")?
        {
            if existing_hash != request_hash {
                return Err(JobError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(JobRef { job_id });
        }
        let raw = load_action(&transaction, action_id)?;
        if raw.owner_scope != owner_scope {
            return Err(JobError::PermissionDenied);
        }
        if raw.revision != expected_revision {
            return Err(JobError::RevisionConflict {
                expected: expected_revision,
                actual: raw.revision,
            });
        }
        if raw.state != "pending" {
            return Err(JobError::ActionState(raw.state));
        }
        transaction.execute(
            "UPDATE actions SET state = 'rejected', revision = revision + 1,
               response_json = ?2 WHERE action_id = ?1",
            params![
                action_id,
                serde_json::to_string(&json!({ "reason": reason }))?
            ],
        )?;
        self.transition(
            &transaction,
            &raw.job_id,
            EventUpdate {
                phase: "action-rejected".to_owned(),
                state: JobState::Failed,
                completed_units: None,
                total_units: None,
                message_key: Some("job.action.rejected".to_owned()),
                details: Some(json!({ "actionId": action_id, "reason": reason })),
                result: None,
                error: Some(AppError {
                    code: "INTERACTION_REJECTED".to_owned(),
                    message_key: "job.action.rejected".to_owned(),
                    details: json!({ "actionId": action_id, "reason": reason }),
                    request_id: raw.operation_id.clone(),
                    job_id: Some(raw.job_id.clone()),
                    retryable: false,
                    recovery_actions: Vec::new(),
                }),
            },
        )?;
        insert_operation(
            &transaction,
            owner_scope,
            idempotency_key,
            "actions.reject",
            request_hash,
            &raw.job_id,
            now_ms(),
        )?;
        transaction.commit()?;
        Ok(JobRef { job_id: raw.job_id })
    }

    pub fn consume_action(
        &self,
        action_id: &str,
        binding: &ActionBinding,
    ) -> Result<ConsumedAction, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = load_action(&transaction, action_id)?;
        let target: Value = serde_json::from_str(&raw.target_json)?;
        if target != binding.target
            || raw.target_revision != binding.target_revision
            || raw.plan_hash != binding.plan_hash
            || raw.module_lock_digest != binding.module_lock_digest
        {
            return Err(JobError::StalePlan);
        }
        if raw.expires_at_ms <= now_ms() && raw.state != "consumed" {
            return Err(JobError::ActionExpired);
        }
        if raw.state != "approved" && raw.state != "consumed" {
            return Err(JobError::ActionState(raw.state));
        }
        let response: Value = serde_json::from_str(
            raw.response_json
                .as_deref()
                .ok_or_else(|| JobError::ActionState(raw.state.clone()))?,
        )?;
        if raw.state == "approved" {
            transaction.execute(
                "UPDATE actions SET state = 'consumed', revision = revision + 1,
                   consumed_at_ms = ?2 WHERE action_id = ?1",
                params![action_id, now_ms()],
            )?;
            self.transition(
                &transaction,
                &raw.job_id,
                EventUpdate {
                    phase: "action-consumed".to_owned(),
                    state: JobState::Running,
                    completed_units: None,
                    total_units: None,
                    message_key: Some("job.action.consumed".to_owned()),
                    details: Some(json!({ "actionId": action_id })),
                    result: None,
                    error: None,
                },
            )?;
        }
        let action = load_action(&transaction, action_id)?.into_action()?;
        transaction.commit()?;
        Ok(ConsumedAction {
            job: JobRef { job_id: raw.job_id },
            action,
            response,
        })
    }

    pub fn recover_incomplete(&self) -> Result<Vec<RecoveryRecord>, JobError> {
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT job_id FROM jobs WHERE state NOT IN ('succeeded', 'cancelled', 'failed')
             ORDER BY created_at_ms, job_id",
        )?;
        let job_ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut records = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            let raw = load_job(&transaction, &job_id)?;
            let checkpoint = raw.checkpoint()?;
            let (state, phase, disposition) = if raw.state == "awaiting-approval" {
                (
                    JobState::AwaitingApproval,
                    "recovered-awaiting-action",
                    RecoveryDisposition::AwaitingAction,
                )
            } else if raw.state == "queued" {
                (
                    JobState::RecoveryRequired,
                    "recovered-queued",
                    RecoveryDisposition::ResumeCheckpoint,
                )
            } else if raw.state == "committing"
                || matches!(
                    raw.commit_state,
                    CommitState::InProgress | CommitState::Unknown
                )
            {
                transaction.execute(
                    "UPDATE jobs SET commit_state = 'unknown' WHERE job_id = ?1",
                    [&job_id],
                )?;
                (
                    JobState::RecoveryRequired,
                    "recovered-commit-unknown",
                    RecoveryDisposition::InspectCommit,
                )
            } else if checkpoint.as_ref().is_some_and(|checkpoint| {
                checkpoint.resume_policy == ResumePolicy::ResumeFromCheckpoint
            }) {
                (
                    JobState::RecoveryRequired,
                    "recovered-checkpoint-validated",
                    RecoveryDisposition::ResumeCheckpoint,
                )
            } else {
                (
                    JobState::RecoveryRequired,
                    "recovered-without-safe-checkpoint",
                    RecoveryDisposition::NoSafeCheckpoint,
                )
            };
            let mut snapshot = self.transition(
                &transaction,
                &job_id,
                EventUpdate {
                    phase: phase.to_owned(),
                    state,
                    completed_units: None,
                    total_units: None,
                    message_key: Some("job.recovered".to_owned()),
                    details: Some(json!({ "previousGenerationId": raw.generation_id })),
                    result: None,
                    error: None,
                },
            )?;
            if disposition == RecoveryDisposition::ResumeCheckpoint {
                snapshot = self.transition(
                    &transaction,
                    &job_id,
                    EventUpdate::state("resume-checkpoint-queued", JobState::Queued),
                )?;
            }
            records.push(RecoveryRecord {
                snapshot,
                checkpoint,
                disposition,
            });
        }
        transaction.commit()?;
        Ok(records)
    }

    fn transition(
        &self,
        transaction: &Transaction<'_>,
        job_id: &str,
        update: EventUpdate,
    ) -> Result<JobSnapshot, JobError> {
        let raw = load_job(transaction, job_id)?;
        let next_state = job_state_name(&update.state);
        if !transition_allowed(&raw.state, next_state) {
            return Err(JobError::InvalidTransition {
                from: raw.state,
                to: next_state.to_owned(),
            });
        }
        let seq = raw.last_seq + 1;
        let now = now_ms();
        let details_json = update
            .details
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let result_json = update
            .result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let error_json = update
            .error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        transaction.execute(
            "UPDATE jobs SET state = ?2, generation_id = ?3, last_seq = ?4, updated_at_ms = ?5,
               result_json = COALESCE(?6, result_json), error_json = COALESCE(?7, error_json)
             WHERE job_id = ?1",
            params![
                job_id,
                next_state,
                self.generation_id,
                seq,
                now,
                result_json,
                error_json,
            ],
        )?;
        transaction.execute(
            "INSERT INTO job_events(
               job_id, seq, generation_id, phase, state, completed_units, total_units,
               message_key, details_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                job_id,
                seq,
                self.generation_id,
                update.phase,
                next_state,
                update.completed_units,
                update.total_units,
                update.message_key,
                details_json,
                now,
            ],
        )?;
        let retention_start = seq.saturating_add(1).saturating_sub(self.retention_limit);
        transaction.execute(
            "DELETE FROM job_events WHERE job_id = ?1 AND seq < ?2",
            params![job_id, retention_start],
        )?;
        load_job(transaction, job_id)?.into_snapshot()
    }
}

pub fn request_hash(value: &Value) -> Result<String, JobError> {
    let bytes = serde_json::to_vec(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn insert_operation(
    transaction: &Transaction<'_>,
    owner_scope: &str,
    idempotency_key: &str,
    command: &str,
    request_hash: &str,
    job_id: &str,
    now: i64,
) -> Result<(), JobError> {
    transaction.execute(
        "INSERT INTO operations(owner_scope, idempotency_key, command, request_hash, job_id, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![owner_scope, idempotency_key, command, request_hash, job_id, now],
    )?;
    Ok(())
}

fn find_operation(
    connection: &rusqlite::Connection,
    owner_scope: &str,
    idempotency_key: &str,
    command: &str,
) -> Result<Option<(String, String)>, rusqlite::Error> {
    connection
        .prepare(
            "SELECT request_hash, job_id FROM operations
             WHERE owner_scope = ?1 AND idempotency_key = ?2 AND command = ?3",
        )?
        .query_row(params![owner_scope, idempotency_key, command], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
}

#[derive(Debug)]
struct RawJob {
    job_id: String,
    command: String,
    owner_scope: String,
    state: String,
    generation_id: String,
    last_seq: u64,
    confirmation_policy: String,
    result_json: Option<String>,
    error_json: Option<String>,
    checkpoint_step: Option<String>,
    checkpoint_json: Option<String>,
    resume_policy: Option<String>,
    commit_state: CommitState,
    cancel_requested: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl RawJob {
    fn into_snapshot(self) -> Result<JobSnapshot, JobError> {
        Ok(JobSnapshot {
            job_id: self.job_id,
            command: self.command,
            state: parse_job_state(&self.state)?,
            generation_id: self.generation_id,
            last_seq: self.last_seq,
            created_at: timestamp(self.created_at_ms),
            updated_at: timestamp(self.updated_at_ms),
            result: self
                .result_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            error: self
                .error_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        })
    }

    fn checkpoint(&self) -> Result<Option<WorkerCheckpoint>, JobError> {
        match (
            &self.checkpoint_step,
            &self.checkpoint_json,
            &self.resume_policy,
        ) {
            (Some(step), Some(payload), Some(policy)) => Ok(Some(WorkerCheckpoint {
                step: step.clone(),
                payload: serde_json::from_str(payload)?,
                resume_policy: ResumePolicy::parse(policy).ok_or_else(|| {
                    JobError::InvalidResponse(format!("unknown resume policy {policy}"))
                })?,
            })),
            _ => Ok(None),
        }
    }
}

fn load_job(connection: &rusqlite::Connection, job_id: &str) -> Result<RawJob, JobError> {
    connection
        .prepare(
            "SELECT job_id, command, owner_scope, state, generation_id, last_seq,
                    confirmation_policy, result_json, error_json, checkpoint_step, checkpoint_json,
                    resume_policy, commit_state, cancel_requested, created_at_ms, updated_at_ms
             FROM jobs WHERE job_id = ?1",
        )?
        .query_row([job_id], |row| {
            Ok(RawJob {
                job_id: row.get(0)?,
                command: row.get(1)?,
                owner_scope: row.get(2)?,
                state: row.get(3)?,
                generation_id: row.get(4)?,
                last_seq: row.get(5)?,
                confirmation_policy: row.get(6)?,
                result_json: row.get(7)?,
                error_json: row.get(8)?,
                checkpoint_step: row.get(9)?,
                checkpoint_json: row.get(10)?,
                resume_policy: row.get(11)?,
                commit_state: CommitState::parse(&row.get::<_, String>(12)?),
                cancel_requested: row.get(13)?,
                created_at_ms: row.get(14)?,
                updated_at_ms: row.get(15)?,
            })
        })
        .optional()?
        .ok_or_else(|| JobError::NotFound(job_id.to_owned()))
}

fn ensure_owner(raw: &RawJob, owner_scope: &str) -> Result<(), JobError> {
    if raw.owner_scope == owner_scope {
        Ok(())
    } else {
        Err(JobError::PermissionDenied)
    }
}

struct RawEvent {
    job_id: String,
    seq: u64,
    generation_id: String,
    phase: String,
    state: String,
    completed_units: Option<u64>,
    total_units: Option<u64>,
    message_key: Option<String>,
    details_json: Option<String>,
}

impl RawEvent {
    fn into_event(self) -> Result<JobEvent, JobError> {
        Ok(JobEvent {
            job_id: self.job_id,
            seq: self.seq,
            generation_id: self.generation_id,
            phase: self.phase,
            state: parse_job_state(&self.state)?,
            completed_units: self.completed_units,
            total_units: self.total_units,
            message_key: self.message_key,
            details: self
                .details_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        })
    }
}

fn raw_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok(RawEvent {
        job_id: row.get(0)?,
        seq: row.get(1)?,
        generation_id: row.get(2)?,
        phase: row.get(3)?,
        state: row.get(4)?,
        completed_units: row.get(5)?,
        total_units: row.get(6)?,
        message_key: row.get(7)?,
        details_json: row.get(8)?,
    })
}

struct RawAction {
    action_id: String,
    job_id: String,
    owner_scope: String,
    revision: u64,
    kind: String,
    state: String,
    operation_id: String,
    target_json: String,
    target_revision: u64,
    plan_hash: String,
    module_lock_digest: String,
    expires_at_ms: i64,
    response_schema_json: String,
    response_json: Option<String>,
}

impl RawAction {
    fn into_action(self) -> Result<ActionRequired, JobError> {
        Ok(ActionRequired {
            action_id: self.action_id,
            revision: self.revision,
            kind: self.kind,
            state: parse_action_state(&self.state)?,
            operation_id: self.operation_id,
            target: serde_json::from_str(&self.target_json)?,
            target_revision: self.target_revision,
            plan_hash: self.plan_hash,
            module_lock_digest: self.module_lock_digest,
            expires_at: timestamp(self.expires_at_ms),
            response_schema: serde_json::from_str(&self.response_schema_json)?,
        })
    }
}

fn load_action(connection: &rusqlite::Connection, action_id: &str) -> Result<RawAction, JobError> {
    connection
        .prepare(
            "SELECT action_id, job_id, owner_scope, revision, kind, state, operation_id,
                    target_json, target_revision, plan_hash, module_lock_digest, expires_at_ms,
                    response_schema_json, response_json
             FROM actions WHERE action_id = ?1",
        )?
        .query_row([action_id], |row| {
            Ok(RawAction {
                action_id: row.get(0)?,
                job_id: row.get(1)?,
                owner_scope: row.get(2)?,
                revision: row.get(3)?,
                kind: row.get(4)?,
                state: row.get(5)?,
                operation_id: row.get(6)?,
                target_json: row.get(7)?,
                target_revision: row.get(8)?,
                plan_hash: row.get(9)?,
                module_lock_digest: row.get(10)?,
                expires_at_ms: row.get(11)?,
                response_schema_json: row.get(12)?,
                response_json: row.get(13)?,
            })
        })
        .optional()?
        .ok_or_else(|| JobError::NotFound(action_id.to_owned()))
}

fn parse_job_state(value: &str) -> Result<JobState, JobError> {
    match value {
        "queued" => Ok(JobState::Queued),
        "preparing" => Ok(JobState::Preparing),
        "awaiting-approval" => Ok(JobState::AwaitingApproval),
        "running" => Ok(JobState::Running),
        "committing" => Ok(JobState::Committing),
        "succeeded" => Ok(JobState::Succeeded),
        "cancelling" => Ok(JobState::Cancelling),
        "cancelled" => Ok(JobState::Cancelled),
        "failed" => Ok(JobState::Failed),
        "recovery-required" => Ok(JobState::RecoveryRequired),
        other => Err(JobError::InvalidResponse(format!(
            "unknown persisted job state {other}"
        ))),
    }
}

fn job_state_name(state: &JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Preparing => "preparing",
        JobState::AwaitingApproval => "awaiting-approval",
        JobState::Running => "running",
        JobState::Committing => "committing",
        JobState::Succeeded => "succeeded",
        JobState::Cancelling => "cancelling",
        JobState::Cancelled => "cancelled",
        JobState::Failed => "failed",
        JobState::RecoveryRequired => "recovery-required",
    }
}

fn parse_action_state(value: &str) -> Result<ActionState, JobError> {
    match value {
        "pending" => Ok(ActionState::Pending),
        "approved" => Ok(ActionState::Approved),
        "rejected" => Ok(ActionState::Rejected),
        "expired" => Ok(ActionState::Expired),
        "consumed" => Ok(ActionState::Consumed),
        other => Err(JobError::InvalidResponse(format!(
            "unknown persisted action state {other}"
        ))),
    }
}

fn transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return !is_terminal(from);
    }
    match from {
        "queued" => matches!(
            to,
            "preparing" | "cancelling" | "failed" | "recovery-required"
        ),
        "preparing" => matches!(
            to,
            "awaiting-approval" | "running" | "cancelling" | "failed" | "recovery-required"
        ),
        "awaiting-approval" => matches!(
            to,
            "running" | "cancelling" | "failed" | "recovery-required"
        ),
        "running" => matches!(
            to,
            "awaiting-approval"
                | "committing"
                | "succeeded"
                | "cancelling"
                | "failed"
                | "recovery-required"
        ),
        "committing" => matches!(to, "succeeded" | "cancelled" | "failed" | "recovery-required"),
        "cancelling" => matches!(
            to,
            "cancelled" | "committing" | "succeeded" | "failed" | "recovery-required"
        ),
        "recovery-required" => matches!(to, "queued" | "succeeded" | "failed" | "cancelled"),
        _ => false,
    }
}

fn is_terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "cancelled" | "failed")
}

fn validate_response(schema: &Value, response: &Value) -> Result<(), JobError> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(JobError::InvalidResponse("response schema must declare an object".to_owned()));
    }
    mintcat_sdk::validate_schema(schema, response, "$.response")
        .map_err(|error| JobError::InvalidResponse(error.to_string()))
}

fn now_ms() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn timestamp(milliseconds: i64) -> String {
    format!("unix-ms:{milliseconds}")
}

#[cfg(test)]
mod tests {
    use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
    use mintcat_sdk::{ActionState, JobState};
    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        request_hash, ActionBinding, ActionSpec, ConfirmationPolicy, EventUpdate, JobError,
        JobStore, RecoveryDisposition, RegisterJob, ResumePolicy, WorkerCheckpoint,
    };

    fn store(retention: u64) -> (tempfile::TempDir, JobStore) {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("data"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let database = KernelDatabase::open(&layout).unwrap();
        let store = JobStore::open(database, "generation-1", retention).unwrap();
        (sandbox, store)
    }

    fn register(store: &JobStore, key: &str, policy: ConfirmationPolicy) -> String {
        store
            .register_job(RegisterJob {
                command: "fixture.run".to_owned(),
                owner_scope: "owner".to_owned(),
                idempotency_key: key.to_owned(),
                request_hash: request_hash(&json!({ "input": key })).unwrap(),
                target: json!({ "profileId": "profile-1", "revision": 7 }),
                confirmation_policy: policy,
            })
            .unwrap()
            .job
            .job_id
    }

    #[test]
    fn response_loss_resolves_the_same_persisted_job() {
        let (_sandbox, store) = store(16);
        let request = RegisterJob {
            command: "fixture.run".to_owned(),
            owner_scope: "owner".to_owned(),
            idempotency_key: "operation-1".to_owned(),
            request_hash: request_hash(&json!({ "input": 1 })).unwrap(),
            target: json!({ "profileId": "profile-1" }),
            confirmation_policy: ConfirmationPolicy::None,
        };
        let first = store.register_job(request.clone()).unwrap();
        let replay = store.register_job(request).unwrap();
        assert!(first.created);
        assert!(!replay.created);
        assert_eq!(first.job, replay.job);
        assert_eq!(
            store
                .find_operation("owner", "fixture.run", "operation-1")
                .unwrap(),
            Some(first.job)
        );
    }

    #[test]
    fn event_cursors_reset_after_retention_and_replay_without_duplicates() {
        let (_sandbox, store) = store(3);
        let job_id = register(&store, "events", ConfirmationPolicy::None);
        let mut snapshot = store.get_job("owner", &job_id).unwrap();
        for index in 0..6 {
            snapshot = store
                .append_event(
                    &job_id,
                    EventUpdate {
                        phase: format!("step-{index}"),
                        state: if index == 0 {
                            JobState::Preparing
                        } else {
                            JobState::Running
                        },
                        completed_units: Some(index),
                        total_units: Some(6),
                        message_key: None,
                        details: Some(json!({ "index": index })),
                        result: None,
                        error: None,
                    },
                )
                .unwrap();
        }
        let page = store.events_after("owner", &job_id, 0, 100).unwrap();
        assert!(page.reset_required);
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.snapshot.last_seq, snapshot.last_seq);
        let after = page.events[0].seq;
        let first = store.events_after("owner", &job_id, after, 100).unwrap();
        let replay = store.events_after("owner", &job_id, after, 100).unwrap();
        assert_eq!(first.events, replay.events);
        assert!(first
            .events
            .windows(2)
            .all(|pair| pair[0].seq < pair[1].seq));
    }

    #[test]
    fn cancellation_waits_for_a_safe_point_and_commit_is_authoritative() {
        let (_sandbox, store) = store(16);
        let job_id = register(&store, "cancel-safe", ConfirmationPolicy::None);
        let running = store
            .append_event(&job_id, EventUpdate::state("prepare", JobState::Preparing))
            .and_then(|_| store.append_event(&job_id, EventUpdate::state("run", JobState::Running)))
            .unwrap();
        let cancelled = store
            .request_cancel(
                "owner",
                &job_id,
                running.last_seq,
                "cancel-1",
                &request_hash(&json!({ "jobId": job_id })).unwrap(),
            )
            .unwrap();
        assert_eq!(cancelled.state, JobState::Cancelling);
        assert_eq!(
            store.cancel_at_safe_point(&job_id).unwrap().state,
            JobState::Cancelled
        );

        let committing_id = register(&store, "cancel-commit", ConfirmationPolicy::None);
        store
            .append_event(
                &committing_id,
                EventUpdate::state("prepare", JobState::Preparing),
            )
            .and_then(|_| {
                store.append_event(&committing_id, EventUpdate::state("run", JobState::Running))
            })
            .unwrap();
        let committing = store.begin_commit(&committing_id).unwrap();
        let deferred = store
            .request_cancel(
                "owner",
                &committing_id,
                committing.last_seq,
                "cancel-2",
                &request_hash(&json!({ "jobId": committing_id })).unwrap(),
            )
            .unwrap();
        assert_eq!(deferred.state, JobState::Committing);
        assert_eq!(
            store
                .finish_commit(&committing_id, json!({ "committed": true }))
                .unwrap()
                .state,
            JobState::Succeeded
        );
    }

    #[test]
    fn approval_consumption_checks_target_plan_and_module_lock() {
        let (_sandbox, store) = store(32);
        let job_id = register(&store, "action", ConfirmationPolicy::Required);
        store
            .append_event(&job_id, EventUpdate::state("prepare", JobState::Preparing))
            .unwrap();
        let target = json!({ "profileId": "profile-1", "revision": 7 });
        let action = store
            .create_action(
                &job_id,
                ActionSpec {
                    kind: "confirm-plan".to_owned(),
                    operation_id: "operation-action".to_owned(),
                    target: target.clone(),
                    target_revision: 17,
                    plan_hash: format!("sha256:{}", "1".repeat(64)),
                    module_lock_digest: format!("sha256:{}", "2".repeat(64)),
                    expires_at_ms: super::now_ms() + 60_000,
                    response_schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["approved"],
                        "properties": { "approved": { "type": "boolean", "const": true } }
                    }),
                },
            )
            .unwrap();
        let refused = json!({ "approved": false });
        assert!(matches!(
            store.approve_action("owner", &action.action_id, action.revision,
                &refused, "false-approval", &request_hash(&refused).unwrap()),
            Err(JobError::InvalidResponse(_))
        ));
        assert_eq!(store.get_action("owner", &action.action_id).unwrap().state, ActionState::Pending);
        let approval_hash =
            request_hash(&json!({ "actionId": action.action_id, "approved": true })).unwrap();
        let approved = store
            .approve_action(
                "owner",
                &action.action_id,
                action.revision,
                &json!({ "approved": true }),
                "approval-1",
                &approval_hash,
            )
            .unwrap();
        let replay = store
            .approve_action(
                "owner",
                &action.action_id,
                action.revision,
                &json!({ "approved": true }),
                "approval-1",
                &approval_hash,
            )
            .unwrap();
        assert_eq!(approved, replay);
        let stale_revision = store.consume_action(
            &action.action_id,
            &ActionBinding {
                target: target.clone(),
                target_revision: 16,
                plan_hash: format!("sha256:{}", "1".repeat(64)),
                module_lock_digest: format!("sha256:{}", "2".repeat(64)),
            },
        );
        assert!(matches!(stale_revision, Err(JobError::StalePlan)));
        let stale = store.consume_action(
            &action.action_id,
            &ActionBinding {
                target: target.clone(),
                target_revision: 17,
                plan_hash: format!("sha256:{}", "9".repeat(64)),
                module_lock_digest: format!("sha256:{}", "2".repeat(64)),
            },
        );
        assert!(matches!(stale, Err(JobError::StalePlan)));
        let consumed = store
            .consume_action(
                &action.action_id,
                &ActionBinding {
                    target,
                    target_revision: 17,
                    plan_hash: format!("sha256:{}", "1".repeat(64)),
                    module_lock_digest: format!("sha256:{}", "2".repeat(64)),
                },
            )
            .unwrap();
        assert_eq!(consumed.response, json!({ "approved": true }));
        assert_eq!(consumed.action.state, ActionState::Consumed);
        assert_eq!(
            store.get_action("owner", &action.action_id).unwrap().state,
            ActionState::Consumed
        );
    }

    #[test]
    fn restart_resumes_only_checkpoints_and_quarantines_unknown_commits() {
        let (sandbox, store) = store(64);
        let resumable = register(&store, "resume", ConfirmationPolicy::None);
        let running = store
            .append_event(
                &resumable,
                EventUpdate::state("prepare", JobState::Preparing),
            )
            .and_then(|_| {
                store.append_event(&resumable, EventUpdate::state("run", JobState::Running))
            })
            .unwrap();
        store
            .save_checkpoint(
                &resumable,
                running.last_seq,
                &WorkerCheckpoint {
                    step: "downloaded".to_owned(),
                    payload: json!({ "artifact": "sha256:checkpoint" }),
                    resume_policy: ResumePolicy::ResumeFromCheckpoint,
                },
            )
            .unwrap();

        let unknown = register(&store, "unknown", ConfirmationPolicy::None);
        store
            .append_event(&unknown, EventUpdate::state("prepare", JobState::Preparing))
            .and_then(|_| {
                store.append_event(&unknown, EventUpdate::state("run", JobState::Running))
            })
            .unwrap();
        store.begin_commit(&unknown).unwrap();
        drop(store);

        let layout = RuntimePaths::new(sandbox.path().join("data"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let reopened =
            JobStore::open(KernelDatabase::open(&layout).unwrap(), "generation-2", 64).unwrap();
        let recovered = reopened.recover_incomplete().unwrap();
        assert!(recovered.iter().any(|record| {
            record.snapshot.job_id == resumable
                && record.snapshot.state == JobState::Queued
                && record.disposition == RecoveryDisposition::ResumeCheckpoint
                && record.checkpoint.is_some()
        }));
        assert!(recovered.iter().any(|record| {
            record.snapshot.job_id == unknown
                && record.snapshot.state == JobState::RecoveryRequired
                && record.disposition == RecoveryDisposition::InspectCommit
        }));
        assert_eq!(
            reopened.get_job("owner", &unknown).unwrap().state,
            JobState::RecoveryRequired
        );
    }
}
