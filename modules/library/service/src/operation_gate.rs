use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_sdk::{
    ArtifactGrantDescriptor, ArtifactRef, DownloadMode, ModsSetVersionParams, OperationFence,
    VersionSelection, VersionSelectionKind,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{LibraryError, OwnerStore};

pub const READ_PERMISSION: &str = "library.read";
pub const PROFILE_WRITE_PERMISSION: &str = "library.profile.write";
pub const CONTENT_WRITE_PERMISSION: &str = "library.content.write";
pub const METADATA_WRITE_PERMISSION: &str = "library.metadata.write";
const CONTENT_RESOURCE_PREFIX: &str = "profile-content:";
const DOWNLOAD_EVENT: &str = "download.requested";
const KERNEL_JOB_DESTINATION: &str = "kernel-job";
const KERNEL_JOB_CONTROL_DESTINATION: &str = "kernel-job-control";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryAuthorization {
    owner_scope: String,
    permissions: HashSet<String>,
}

impl LibraryAuthorization {
    pub fn new(
        owner_scope: impl Into<String>,
        permissions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            owner_scope: owner_scope.into(),
            permissions: permissions.into_iter().map(Into::into).collect(),
        }
    }

    pub fn owner_scope(&self) -> &str {
        &self.owner_scope
    }

    pub(crate) fn require(&self, permission: &str) -> Result<(), LibraryError> {
        self.permissions
            .contains(permission)
            .then_some(())
            .ok_or_else(|| LibraryError::PermissionDenied(permission.to_owned()))
    }

    fn require_content_write(&self) -> Result<(), LibraryError> {
        self.require(CONTENT_WRITE_PERMISSION)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartOperationItem {
    pub item_id: String,
    pub profile_mod_id: String,
    pub profile_mod_incarnation_id: String,
    pub mod_id: String,
    pub source_id: String,
    pub source_mod_id: String,
    pub source_file_id: Option<String>,
    pub remote_game_key: String,
    pub resolved_version: String,
    pub expected_digest: Option<String>,
    pub validator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<VersionSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartDownloadOperation {
    pub idempotency_scope: String,
    pub idempotency_key: String,
    pub profile_id: String,
    pub expected_content_revision: u64,
    pub target_incarnation_id: String,
    pub owner_generation: String,
    pub module_lock_digest: String,
    pub mode: DownloadMode,
    pub items: Vec<StartOperationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadOperationState {
    Queued,
    Running,
    RecoveryRequired,
    Succeeded,
    Failed,
    Cancelled,
}

impl DownloadOperationState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RecoveryRequired => "recovery-required",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, LibraryError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "recovery-required" => Ok(Self::RecoveryRequired),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(LibraryError::OperationState(other.to_owned())),
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadOperationSnapshot {
    pub operation_id: String,
    pub owner_scope: String,
    pub profile_id: String,
    pub mode: DownloadMode,
    pub inputs_hash: String,
    pub state: DownloadOperationState,
    pub fence: OperationFence,
    pub result: Option<Value>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartedDownloadOperation {
    pub operation: DownloadOperationSnapshot,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadExecutionItem {
    pub item_id: String,
    pub profile_mod_id: String,
    pub profile_mod_incarnation_id: String,
    pub mod_id: String,
    pub source_id: String,
    pub remote_game_key: String,
    pub source_mod_id: String,
    pub source_file_id: Option<String>,
    pub resolved_version: String,
    pub expected_digest: Option<String>,
    pub validator: Option<String>,
    pub selection: Option<VersionSelection>,
    pub artifact: Option<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadExecutionPlan {
    pub operation: DownloadOperationSnapshot,
    pub items: Vec<DownloadExecutionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedArtifact {
    pub operation_id: String,
    pub item_id: String,
    pub digest: String,
    pub newly_recorded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailedDownloadOperation {
    pub operation_id: String,
    pub state: DownloadOperationState,
    pub error_code: String,
    pub details: Value,
}
#[derive(Debug)]
pub struct AppliedArtifact<'a> {
    pub item_id: &'a str,
    pub grant: &'a ArtifactGrant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyOperationResult {
    pub operation_id: String,
    pub profile_id: String,
    pub content_revision: u64,
    pub artifact_digests: Vec<String>,
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetProfileModEnabled {
    pub idempotency_key: String,
    pub profile_id: String,
    pub profile_mod_id: String,
    pub expected_content_revision: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteProfileMod {
    pub idempotency_key: String,
    pub profile_id: String,
    pub profile_mod_id: String,
    pub expected_content_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutationResult {
    pub operation_id: String,
    pub profile_id: String,
    pub content_revision: u64,
    pub changed: bool,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxEvent {
    pub event_seq: u64,
    pub event_id: String,
    pub operation_id: Option<String>,
    pub event_type: String,
    pub destination: String,
    pub payload: Value,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileContentCapability {
    pub content_revision: u64,
    pub writable: bool,
    pub busy_operation_id: Option<String>,
    pub recovery_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileContentGuard {
    pub guard_id: String,
    pub profile_id: String,
    pub owner_scope: String,
    pub owner_generation: String,
    pub fence_epoch: u64,
    pub expected_content_revision: u64,
    pub target_incarnation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileDownloadState {
    pub capability: ProfileContentCapability,
    pub plans: Vec<DownloadExecutionPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum CancelOperationResult {
    Cancelled,
    AlreadyCancelled,
    AlreadyCompleted(Value),
}

#[derive(Debug)]
struct ProfileGateState {
    owner_scope: String,
    content_revision: u64,
    lease_epoch: u64,
    incarnation_id: String,
}

#[derive(Debug)]
struct StoredOperation {
    operation_id: String,
    owner_scope: String,
    inputs_hash: String,
    profile_id: String,
    mode: DownloadMode,
    expected_content_revision: u64,
    target_incarnation_id: String,
    owner_generation: String,
    fence_epoch: u64,
    state: DownloadOperationState,
    result: Option<Value>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

#[derive(Debug)]
struct StoredItem {
    item_id: String,
    profile_mod_id: String,
    profile_mod_incarnation_id: String,
    mod_id: String,
    source_id: String,
    source_mod_id: String,
    remote_game_key: String,
    source_file_id: Option<String>,
    resolved_version: String,
    expected_digest: Option<String>,
    validator: Option<String>,
    apply_selection_kind: Option<String>,
    apply_selected_version_id: Option<String>,
    artifact: Option<ArtifactRef>,
}

impl OwnerStore { pub fn acquire_profile_content_guard(
    &self,
    authorization: &LibraryAuthorization,
    guard_id: &str,
    profile_id: &str,
    expected_content_revision: u64,
    target_incarnation_id: &str,
    owner_generation: &str,
) -> Result<ProfileContentGuard, LibraryError> {
    authorization.require_content_write()?;
    if guard_id.trim().is_empty()
        || profile_id.trim().is_empty()
        || target_incarnation_id.trim().is_empty()
        || owner_generation.trim().is_empty()
    {
        return Err(LibraryError::InvalidRecord(
            "Profile content guard identifiers must be non-empty".to_owned(),
        ));
    }
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let profile = load_profile_gate(&transaction, profile_id)?;
    authorize_owner(authorization, &profile.owner_scope)?;
    ensure_revision(expected_content_revision, profile.content_revision)?;
    if profile.incarnation_id != target_incarnation_id {
        return Err(LibraryError::StaleTarget(format!(
            "profile {profile_id} incarnation changed"
        )));
    }
    let resource_key = content_resource_key(profile_id);
    ensure_resource_available(&transaction, &resource_key)?;
    let fence_epoch = profile
        .lease_epoch
        .checked_add(1)
        .ok_or_else(|| LibraryError::InvalidRecord("lease epoch overflow".to_owned()))?;
    transaction.execute(
        "UPDATE library_profiles SET lease_epoch = ?2, updated_at_ms = ?3
         WHERE profile_id = ?1",
        params![profile_id, fence_epoch, now],
    )?;
    transaction.execute(
        "INSERT INTO library_profile_content_guards(
           resource_key, guard_id, profile_id, owner_scope, owner_generation,
           fence_epoch, expected_content_revision, target_incarnation_id,
           state, acquired_at_ms, heartbeat_at_ms, released_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?9, NULL)
         ON CONFLICT(resource_key) DO UPDATE SET
           guard_id = excluded.guard_id,
           profile_id = excluded.profile_id,
           owner_scope = excluded.owner_scope,
           owner_generation = excluded.owner_generation,
           fence_epoch = excluded.fence_epoch,
           expected_content_revision = excluded.expected_content_revision,
           target_incarnation_id = excluded.target_incarnation_id,
           state = 'active',
           acquired_at_ms = excluded.acquired_at_ms,
           heartbeat_at_ms = excluded.heartbeat_at_ms,
           released_at_ms = NULL",
        params![
            resource_key,
            guard_id,
            profile_id,
            authorization.owner_scope(),
            owner_generation,
            fence_epoch,
            expected_content_revision,
            target_incarnation_id,
            now,
        ],
    )?;
    transaction.commit()?;
    Ok(ProfileContentGuard {
        guard_id: guard_id.to_owned(),
        profile_id: profile_id.to_owned(),
        owner_scope: authorization.owner_scope().to_owned(),
        owner_generation: owner_generation.to_owned(),
        fence_epoch,
        expected_content_revision,
        target_incarnation_id: target_incarnation_id.to_owned(),
    })
}

pub fn validate_profile_content_guard(
    &self,
    guard: &ProfileContentGuard,
) -> Result<(), LibraryError> {
    let connection = self.connection()?;
    let valid = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM library_profile_content_guards
           WHERE guard_id = ?1 AND profile_id = ?2 AND owner_scope = ?3
             AND owner_generation = ?4 AND fence_epoch = ?5
             AND expected_content_revision = ?6 AND target_incarnation_id = ?7
             AND state = 'active'
         )",
        params![
            guard.guard_id,
            guard.profile_id,
            guard.owner_scope,
            guard.owner_generation,
            guard.fence_epoch,
            guard.expected_content_revision,
            guard.target_incarnation_id,
        ],
        |row| row.get::<_, bool>(0),
    )?;
    if !valid {
        return Err(LibraryError::StaleOperation);
    }
    Ok(())
}

pub fn release_profile_content_guard(
    &self,
    guard: &ProfileContentGuard,
) -> Result<(), LibraryError> {
    let now = now_ms()?;
    let connection = self.connection()?;
    let changed = connection.execute(
        "UPDATE library_profile_content_guards
         SET state = 'released', heartbeat_at_ms = ?8, released_at_ms = ?8
         WHERE guard_id = ?1 AND profile_id = ?2 AND owner_scope = ?3
           AND owner_generation = ?4 AND fence_epoch = ?5
           AND expected_content_revision = ?6 AND target_incarnation_id = ?7
           AND state = 'active'",
        params![
            guard.guard_id,
            guard.profile_id,
            guard.owner_scope,
            guard.owner_generation,
            guard.fence_epoch,
            guard.expected_content_revision,
            guard.target_incarnation_id,
            now,
        ],
    )?;
    if changed == 0 {
        let released = connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM library_profile_content_guards
               WHERE guard_id = ?1 AND state = 'released'
             )",
            params![guard.guard_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !released {
            return Err(LibraryError::StaleOperation);
        }
    }
    Ok(())
}

pub fn start_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    request: &StartDownloadOperation,
) -> Result<StartedDownloadOperation, LibraryError> {
    authorization.require_content_write()?;
    validate_start_request(request)?;
    let inputs_hash = hash_json(&json!({
        "profileId": request.profile_id,
        "expectedContentRevision": request.expected_content_revision,
        "targetIncarnationId": request.target_incarnation_id,
        "mode": request.mode,
        "items": request.items,
    }))?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing_id = transaction
        .query_row(
            "SELECT operation_id FROM library_download_operations
             WHERE owner_scope = ?1 AND idempotency_scope = ?2 AND idempotency_key = ?3",
            params![
                authorization.owner_scope(),
                request.idempotency_scope,
                request.idempotency_key
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(operation_id) = existing_id {
        let operation = load_operation(&transaction, &operation_id)?;
        if operation.inputs_hash != inputs_hash {
            return Err(LibraryError::IdempotencyConflict);
        }
        transaction.commit()?;
        return Ok(StartedDownloadOperation {
            operation: operation.into_snapshot(),
            created: false,
        });
    }

    let profile = load_profile_gate(&transaction, &request.profile_id)?;
    authorize_owner(authorization, &profile.owner_scope)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    if profile.incarnation_id != request.target_incarnation_id {
        return Err(LibraryError::StaleTarget(format!(
            "profile {} incarnation changed",
            request.profile_id
        )));
    }
    let operation_id = Uuid::new_v4().to_string();
    let (resource_key, fence_epoch) = match request.mode {
        DownloadMode::PrepareAndApply => {
            let resource_key = content_resource_key(&request.profile_id);
            ensure_resource_available(&transaction, &resource_key)?;
            let fence_epoch = profile.lease_epoch.checked_add(1).ok_or_else(|| {
                LibraryError::InvalidRecord("lease epoch overflow".to_owned())
            })?;
            transaction.execute(
                "UPDATE library_profiles SET lease_epoch = ?2, updated_at_ms = ?3
                 WHERE profile_id = ?1",
                params![request.profile_id, fence_epoch, now],
            )?;
            (resource_key, fence_epoch)
        }
        DownloadMode::CacheOnly => (cache_resource_key(&operation_id), 1),
    };
    for item in &request.items {
        validate_start_item(&transaction, &request.profile_id, item)?;
    }
    transaction.execute(
        "INSERT INTO library_download_operations(
           operation_id, owner_scope, idempotency_scope, idempotency_key, inputs_hash,
           profile_id, mode, expected_content_revision, target_incarnation_id,
           owner_generation, fence_epoch, state, result_json, error_code,
           created_at_ms, updated_at_ms, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'queued', NULL, NULL, ?12, ?12, NULL)",
        params![
            operation_id,
            authorization.owner_scope(),
            request.idempotency_scope,
            request.idempotency_key,
            inputs_hash,
            request.profile_id,
            download_mode_name(&request.mode),
            request.expected_content_revision,
            request.target_incarnation_id,
            request.owner_generation,
            fence_epoch,
            now
        ],
    )?;
    transaction.execute(
        "INSERT INTO library_operation_leases(
           resource_key, operation_id, fence_epoch, owner_generation, state,
           acquired_at_ms, heartbeat_at_ms, released_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, NULL)
         ON CONFLICT(resource_key) DO UPDATE SET
           operation_id = excluded.operation_id,
           fence_epoch = excluded.fence_epoch,
           owner_generation = excluded.owner_generation,
           state = excluded.state,
           acquired_at_ms = excluded.acquired_at_ms,
           heartbeat_at_ms = excluded.heartbeat_at_ms,
           released_at_ms = NULL",
        params![
            resource_key,
            operation_id,
            fence_epoch,
            request.owner_generation,
            now
        ],
    )?;
    for item in &request.items {
        let cached_artifact = item
            .expected_digest
            .as_deref()
            .map(|digest| {
                transaction
                    .query_row(
                        "SELECT digest FROM library_content_refs
                         WHERE digest = ?1 AND verified = 1",
                        [digest],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
            })
            .transpose()?
            .flatten();
        let item_state = if cached_artifact.is_some() {
            "verified"
        } else {
            "pending"
        };
        transaction.execute(
            "INSERT INTO library_download_operation_items(
               operation_id, item_id, profile_mod_id, profile_mod_incarnation_id,
               mod_id, source_id, source_mod_id, remote_game_key, source_file_id,
               resolved_version, expected_digest, validator, artifact_digest,
               apply_selection_kind, apply_selected_version_id, state
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
               ?15, ?13, ?14, ?16
             )",
            params![
                operation_id,
                item.item_id,
                item.profile_mod_id,
                item.profile_mod_incarnation_id,
                item.mod_id,
                item.source_id,
                item.source_mod_id,
                item.remote_game_key,
                item.source_file_id,
                item.resolved_version,
                item.expected_digest,
                item.validator,
                item.selection.as_ref().map(selection_kind_name),
                item.selection
                    .as_ref()
                    .and_then(|selection| selection.version_id.as_deref()),
                cached_artifact,
                item_state
            ],
        )?;
    }
    insert_outbox(
        &transaction,
        Some(&operation_id),
        DOWNLOAD_EVENT,
        KERNEL_JOB_DESTINATION,
        &json!({
            "operationId": operation_id,
            "ownerScope": authorization.owner_scope(),
            "inputsHash": inputs_hash,
            "moduleLockDigest": request.module_lock_digest,
            "profileId": request.profile_id,
            "mode": request.mode,
            "fenceEpoch": fence_epoch,
            "ownerGeneration": request.owner_generation,
        }),
        now,
    )?;
    let operation = load_operation(&transaction, &operation_id)?;
    transaction.commit()?;
    Ok(StartedDownloadOperation {
        operation: operation.into_snapshot(),
        created: true,
    })
}

pub fn download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<DownloadOperationSnapshot, LibraryError> {
    let connection = self.connection()?;
    let operation = load_operation_connection(&connection, operation_id)?;
    authorize_owner(authorization, &operation.owner_scope)?;
    Ok(operation.into_snapshot())
}

pub fn start_version_download(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    owner_generation: &str,
    module_lock_digest: &str,
    request: &ModsSetVersionParams,
) -> Result<StartedDownloadOperation, LibraryError> {
    authorization.require_content_write()?;
    validate_version_selection(&request.selection)?;
    if idempotency_key.trim().is_empty() || owner_generation.trim().is_empty() {
        return Err(LibraryError::InvalidRecord(
            "version download identifiers cannot be empty".to_owned(),
        ));
    }
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing_id = transaction
        .query_row(
            "SELECT operation_id FROM library_download_operations
             WHERE owner_scope = ?1 AND idempotency_scope = 'mods.set-version'
               AND idempotency_key = ?2",
            params![authorization.owner_scope(), idempotency_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(operation_id) = existing_id {
        let operation = load_operation(&transaction, &operation_id)?;
        let items = load_operation_items(&transaction, &operation_id)?;
        let stored_selection = items.first().map(stored_selection).transpose()?.flatten();
        let same_request = items.first().is_some_and(|item| {
            items.len() == 1
                && operation.profile_id == request.profile_id
                && operation.mode == request.mode
                && operation.expected_content_revision == request.expected_content_revision
                && item.profile_mod_id == request.profile_mod_id
                && item.profile_mod_incarnation_id == request.incarnation_id
                && stored_selection.as_ref() == Some(&request.selection)
        });
        if !same_request {
            return Err(LibraryError::IdempotencyConflict);
        }
        transaction.commit()?;
        return Ok(StartedDownloadOperation {
            operation: operation.into_snapshot(),
            created: false,
        });
    }
    let profile = load_profile_gate(&transaction, &request.profile_id)?;
    authorize_owner(authorization, &profile.owner_scope)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    let membership = transaction
        .query_row(
            "SELECT pm.incarnation_id, pm.mod_id, m.source_id, m.remote_game_key,
                    m.remote_mod_id
             FROM library_profile_mods pm
             JOIN library_mods m ON m.mod_id = pm.mod_id
             WHERE pm.profile_mod_id = ?1 AND pm.profile_id = ?2",
            params![request.profile_mod_id, request.profile_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(request.profile_mod_id.clone()))?;
    if membership.0 != request.incarnation_id {
        return Err(LibraryError::StaleTarget(request.profile_mod_id.clone()));
    }
    let resolved_version = match &request.selection.kind {
        VersionSelectionKind::Pinned => {
            request.selection.version_id.clone().ok_or_else(|| {
                LibraryError::InvalidRecord(
                    "pinned version selection requires versionId".to_owned(),
                )
            })?
        }
        VersionSelectionKind::Auto => {
            let mut statement = transaction.prepare(
                "SELECT version_id FROM library_mod_versions
                 WHERE mod_id = ?1 AND current = 1
                 ORDER BY updated_at_ms DESC, version_id LIMIT 2",
            )?;
            let versions = statement
                .query_map([&membership.1], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if versions.len() != 1 {
                return Err(LibraryError::InvalidRecord(format!(
                    "automatic version selection for {} expected one current version",
                    membership.1
                )));
            }
            versions[0].clone()
        }
    };
    let version = transaction
        .query_row(
            "SELECT remote_file_id, artifact_digest FROM library_mod_versions
             WHERE mod_id = ?1 AND version_id = ?2",
            params![membership.1, resolved_version],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            LibraryError::NotFound(format!("{}@{resolved_version}", membership.1))
        })?;
    let target_incarnation_id = profile.incarnation_id;
    transaction.commit()?;
    drop(connection);

    self.start_download_operation(
        authorization,
        &StartDownloadOperation {
            idempotency_scope: "mods.set-version".to_owned(),
            idempotency_key: idempotency_key.to_owned(),
            profile_id: request.profile_id.clone(),
            expected_content_revision: request.expected_content_revision,
            target_incarnation_id,
            owner_generation: owner_generation.to_owned(),
            module_lock_digest: module_lock_digest.to_owned(),
            mode: request.mode.clone(),
            items: vec![StartOperationItem {
                item_id: "primary".to_owned(),
                profile_mod_id: request.profile_mod_id.clone(),
                profile_mod_incarnation_id: membership.0,
                mod_id: membership.1,
                source_id: membership.2,
                source_mod_id: membership.4,
                remote_game_key: membership.3,
                source_file_id: version.0,
                resolved_version,
                expected_digest: version.1,
                validator: None,
                selection: Some(request.selection.clone()),
            }],
        },
    )
}

pub fn download_execution_plan(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<DownloadExecutionPlan, LibraryError> {
    authorization.require_content_write()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction()?;
    let plan = load_execution_plan(&transaction, operation_id)?;
    authorize_owner(authorization, &plan.operation.owner_scope)?;
    transaction.commit()?;
    Ok(plan)
}

pub fn profile_download_state(
    &self,
    authorization: &LibraryAuthorization,
    profile_id: &str,
) -> Result<ProfileDownloadState, LibraryError> {
    authorization.require(READ_PERMISSION)?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction()?;
    let (owner_scope, content_revision) = transaction
        .query_row(
            "SELECT owner_scope, content_revision FROM library_profiles
             WHERE profile_id = ?1 AND deleted_at_ms IS NULL",
            [profile_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(profile_id.to_owned()))?;
    authorize_owner(authorization, &owner_scope)?;
    let lease = transaction
        .query_row(
            "SELECT owner_id, state FROM (
               SELECT operation_id AS owner_id, state
               FROM library_operation_leases
               WHERE resource_key = ?1 AND state IN ('active', 'suspect')
               UNION ALL
               SELECT guard_id AS owner_id, state
               FROM library_profile_content_guards
               WHERE resource_key = ?1 AND state = 'active'
             ) LIMIT 1",
            params![content_resource_key(profile_id)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let capability = match lease {
        Some((operation_id, state)) => ProfileContentCapability {
            content_revision,
            writable: false,
            busy_operation_id: Some(operation_id),
            recovery_required: state == "suspect",
        },
        None => ProfileContentCapability {
            content_revision,
            writable: true,
            busy_operation_id: None,
            recovery_required: false,
        },
    };
    let operation_ids = {
        let mut statement = transaction.prepare(
            "SELECT operation_id FROM library_download_operations
             WHERE profile_id = ?1
               AND state IN ('queued', 'running', 'recovery-required')
             ORDER BY created_at_ms, operation_id",
        )?;
        let rows = statement.query_map([profile_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let plans = operation_ids
        .iter()
        .map(|operation_id| load_execution_plan(&transaction, operation_id))
        .collect::<Result<Vec<_>, _>>()?;
    transaction.commit()?;
    Ok(ProfileDownloadState { capability, plans })
}
pub fn set_profile_mod_enabled(
    &self,
    authorization: &LibraryAuthorization,
    request: &SetProfileModEnabled,
) -> Result<MutationResult, LibraryError> {
    authorization.require_content_write()?;
    if request.idempotency_key.is_empty() {
        return Err(LibraryError::InvalidRecord(
            "idempotency key must not be empty".to_owned(),
        ));
    }
    let inputs_hash = hash_json(&json!({
        "profileId": request.profile_id,
        "profileModId": request.profile_mod_id,
        "expectedContentRevision": request.expected_content_revision,
        "enabled": request.enabled,
    }))?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = transaction
        .query_row(
            "SELECT inputs_hash, result_json FROM library_mutation_results
             WHERE owner_scope = ?1 AND idempotency_scope = 'profile-mod.set-enabled'
               AND idempotency_key = ?2",
            params![authorization.owner_scope(), request.idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((stored_hash, result_json)) = existing {
        if stored_hash != inputs_hash {
            return Err(LibraryError::IdempotencyConflict);
        }
        let mut result: MutationResult = serde_json::from_str(&result_json)?;
        result.replayed = true;
        transaction.commit()?;
        return Ok(result);
    }

    let profile = load_profile_gate(&transaction, &request.profile_id)?;
    authorize_owner(authorization, &profile.owner_scope)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_resource_available(&transaction, &content_resource_key(&request.profile_id))?;
    let current_enabled = transaction
        .query_row(
            "SELECT enabled FROM library_profile_mods
             WHERE profile_mod_id = ?1 AND profile_id = ?2",
            params![request.profile_mod_id, request.profile_id],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(request.profile_mod_id.clone()))?;
    let changed = current_enabled != request.enabled;
    let content_revision = if changed {
        transaction.execute(
            "UPDATE library_profile_mods SET enabled = ?2, updated_at_ms = ?3
             WHERE profile_mod_id = ?1",
            params![request.profile_mod_id, request.enabled, now],
        )?;
        let next = profile.content_revision.checked_add(1).ok_or_else(|| {
            LibraryError::InvalidRecord("content revision overflow".to_owned())
        })?;
        transaction.execute(
            "UPDATE library_profiles SET content_revision = ?2, updated_at_ms = ?3
             WHERE profile_id = ?1",
            params![request.profile_id, next, now],
        )?;
        next
    } else {
        profile.content_revision
    };
    let operation_id = Uuid::new_v4().to_string();
    let result = MutationResult {
        operation_id: operation_id.clone(),
        profile_id: request.profile_id.clone(),
        content_revision,
        changed,
        replayed: false,
    };
    let result_json = serde_json::to_string(&result)?;
    transaction.execute(
        "INSERT INTO library_mutation_results(
           owner_scope, idempotency_scope, idempotency_key, inputs_hash,
           result_json, operation_id, created_at_ms
         ) VALUES (?1, 'profile-mod.set-enabled', ?2, ?3, ?4, ?5, ?6)",
        params![
            authorization.owner_scope(),
            request.idempotency_key,
            inputs_hash,
            result_json,
            operation_id,
            now
        ],
    )?;
    insert_outbox(
        &transaction,
        None,
        "profile.content.changed",
        "application-events",
        &json!({
            "operationId": operation_id,
            "profileId": request.profile_id,
            "profileModId": request.profile_mod_id,
            "contentRevision": content_revision,
            "changed": changed,
        }),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn delete_profile_mod(
    &self,
    authorization: &LibraryAuthorization,
    request: &DeleteProfileMod,
) -> Result<MutationResult, LibraryError> {
    authorization.require_content_write()?;
    if request.idempotency_key.is_empty() {
        return Err(LibraryError::InvalidRecord(
            "idempotency key must not be empty".to_owned(),
        ));
    }
    let inputs_hash = hash_json(&json!({
        "profileId": request.profile_id,
        "profileModId": request.profile_mod_id,
        "expectedContentRevision": request.expected_content_revision,
    }))?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = transaction
        .query_row(
            "SELECT inputs_hash, result_json FROM library_mutation_results
             WHERE owner_scope = ?1 AND idempotency_scope = 'profile-mod.delete'
               AND idempotency_key = ?2",
            params![authorization.owner_scope(), request.idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((stored_hash, result_json)) = existing {
        if stored_hash != inputs_hash {
            return Err(LibraryError::IdempotencyConflict);
        }
        let mut result: MutationResult = serde_json::from_str(&result_json)?;
        result.replayed = true;
        transaction.commit()?;
        return Ok(result);
    }

    let profile = load_profile_gate(&transaction, &request.profile_id)?;
    authorize_owner(authorization, &profile.owner_scope)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_resource_available(&transaction, &content_resource_key(&request.profile_id))?;
    let deleted = transaction.execute(
        "DELETE FROM library_profile_mods
         WHERE profile_mod_id = ?1 AND profile_id = ?2",
        params![request.profile_mod_id, request.profile_id],
    )?;
    if deleted == 0 {
        return Err(LibraryError::NotFound(request.profile_mod_id.clone()));
    }
    let content_revision = profile
        .content_revision
        .checked_add(1)
        .ok_or_else(|| LibraryError::InvalidRecord("content revision overflow".to_owned()))?;
    transaction.execute(
        "UPDATE library_profiles SET content_revision = ?2, updated_at_ms = ?3
         WHERE profile_id = ?1",
        params![request.profile_id, content_revision, now],
    )?;
    let operation_id = Uuid::new_v4().to_string();
    let result = MutationResult {
        operation_id: operation_id.clone(),
        profile_id: request.profile_id.clone(),
        content_revision,
        changed: true,
        replayed: false,
    };
    transaction.execute(
        "INSERT INTO library_mutation_results(
           owner_scope, idempotency_scope, idempotency_key, inputs_hash,
           result_json, operation_id, created_at_ms
         ) VALUES (?1, 'profile-mod.delete', ?2, ?3, ?4, ?5, ?6)",
        params![
            authorization.owner_scope(),
            request.idempotency_key,
            inputs_hash,
            serde_json::to_string(&result)?,
            operation_id,
            now
        ],
    )?;
    insert_outbox(
        &transaction,
        None,
        "profile.content.changed",
        "application-events",
        &json!({
            "operationId": operation_id,
            "profileId": request.profile_id,
            "profileModId": request.profile_mod_id,
            "contentRevision": content_revision,
            "deleted": true,
        }),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn mark_download_running(
    &self,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_active_lease(&transaction, operation_id, fence_epoch, owner_generation)?;
    let changed = transaction.execute(
        "UPDATE library_download_operations SET state = 'running', updated_at_ms = ?2
         WHERE operation_id = ?1 AND state IN ('queued', 'running')",
        params![operation_id, now],
    )?;
    if changed == 0 {
        let state = load_operation(&transaction, operation_id)?.state;
        return Err(LibraryError::OperationState(state.as_str().to_owned()));
    }
    transaction.execute(
        "UPDATE library_operation_leases SET heartbeat_at_ms = ?2
         WHERE operation_id = ?1 AND state = 'active'",
        params![operation_id, now],
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn heartbeat_download_operation(
    &self,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
    let now = now_ms()?;
    let connection = self.connection()?;
    let changed = connection.execute(
        "UPDATE library_operation_leases SET heartbeat_at_ms = ?4
         WHERE operation_id = ?1 AND fence_epoch = ?2 AND owner_generation = ?3
           AND state = 'active'",
        params![operation_id, fence_epoch, owner_generation, now],
    )?;
    if changed == 0 {
        return Err(LibraryError::StaleOperation);
    }
    Ok(())
}

pub fn record_download_artifact(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    item_id: &str,
    grant: &ArtifactGrant,
) -> Result<RecordedArtifact, LibraryError> {
    authorization.require_content_write()?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let operation = load_operation(&transaction, operation_id)?;
    authorize_owner(authorization, &operation.owner_scope)?;
    if operation.state.is_terminal() {
        return Err(LibraryError::OperationState(
            operation.state.as_str().to_owned(),
        ));
    }
    validate_active_lease(&transaction, operation_id, fence_epoch, owner_generation)?;
    let items = load_operation_items(&transaction, operation_id)?;
    let item = items
        .iter()
        .find(|item| item.item_id == item_id)
        .ok_or_else(|| LibraryError::NotFound(item_id.to_owned()))?;
    if operation.mode == DownloadMode::PrepareAndApply {
        let profile = load_profile_gate(&transaction, &operation.profile_id)?;
        ensure_revision(
            operation.expected_content_revision,
            profile.content_revision,
        )?;
        if profile.incarnation_id != operation.target_incarnation_id {
            return Err(LibraryError::StaleTarget(operation.profile_id.clone()));
        }
        validate_pinned_item(&transaction, &operation.profile_id, item)?;
    }
    let descriptor = &grant.descriptor;
    if item
        .expected_digest
        .as_ref()
        .is_some_and(|digest| digest != &descriptor.digest)
    {
        return Err(LibraryError::ArtifactNotVerified(format!(
            "{} digest mismatch",
            item.item_id
        )));
    }
    if let Some(existing) = &item.artifact {
        if existing.digest != descriptor.digest || existing.size != descriptor.size {
            return Err(LibraryError::ArtifactNotVerified(format!(
                "{} recorded artifact changed",
                item.item_id
            )));
        }
        transaction.commit()?;
        return Ok(RecordedArtifact {
            operation_id: operation_id.to_owned(),
            item_id: item_id.to_owned(),
            digest: descriptor.digest.clone(),
            newly_recorded: false,
        });
    }
    persist_verified_artifact(&transaction, item, descriptor, &descriptor.grant_id, now)?;
    transaction.execute(
        "UPDATE library_download_operation_items
         SET artifact_digest = ?3, state = 'verified'
         WHERE operation_id = ?1 AND item_id = ?2",
        params![operation_id, item_id, descriptor.digest],
    )?;
    transaction.commit()?;
    Ok(RecordedArtifact {
        operation_id: operation_id.to_owned(),
        item_id: item_id.to_owned(),
        digest: descriptor.digest.clone(),
        newly_recorded: true,
    })
}

pub fn apply_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    artifacts: &[AppliedArtifact<'_>],
) -> Result<ApplyOperationResult, LibraryError> {
    authorization.require_content_write()?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let operation = load_operation(&transaction, operation_id)?;
    authorize_owner(authorization, &operation.owner_scope)?;
    if operation.state == DownloadOperationState::Succeeded {
        let result = operation
            .result
            .ok_or_else(|| LibraryError::OperationState("missing result".to_owned()))?;
        return Ok(serde_json::from_value(result)?);
    }
    if operation.state.is_terminal() {
        return Err(LibraryError::OperationState(
            operation.state.as_str().to_owned(),
        ));
    }
    validate_active_lease(&transaction, operation_id, fence_epoch, owner_generation)?;
    if operation.fence_epoch != fence_epoch {
        return Err(LibraryError::StaleFence {
            expected: operation.fence_epoch,
            actual: fence_epoch,
        });
    }
    if operation.owner_generation != owner_generation {
        return Err(LibraryError::StaleOperation);
    }
    let profile = if operation.mode == DownloadMode::PrepareAndApply {
        let profile = load_profile_gate(&transaction, &operation.profile_id)?;
        ensure_revision(
            operation.expected_content_revision,
            profile.content_revision,
        )?;
        if profile.incarnation_id != operation.target_incarnation_id {
            return Err(LibraryError::StaleTarget(format!(
                "profile {} incarnation changed",
                operation.profile_id
            )));
        }
        Some(profile)
    } else {
        None
    };
    let items = load_operation_items(&transaction, operation_id)?;
    let mut grants = HashMap::with_capacity(artifacts.len());
    for artifact in artifacts {
        if grants.insert(artifact.item_id, artifact.grant).is_some() {
            return Err(LibraryError::InvalidRecord(format!(
                "duplicate artifact item {}",
                artifact.item_id
            )));
        }
    }
    if grants.len() != items.len() {
        return Err(LibraryError::ArtifactNotVerified(format!(
            "operation {operation_id} expected {} artifacts, received {}",
            items.len(),
            grants.len()
        )));
    }

    let mut artifact_digests = Vec::with_capacity(items.len());
    for item in &items {
        if operation.mode == DownloadMode::PrepareAndApply {
            validate_pinned_item(&transaction, &operation.profile_id, item)?;
        }
        let grant = grants
            .remove(item.item_id.as_str())
            .ok_or_else(|| LibraryError::ArtifactNotVerified(item.item_id.clone()))?;
        let descriptor = &grant.descriptor;
        if item.artifact.as_ref().is_some_and(|artifact| {
            artifact.digest != descriptor.digest || artifact.size != descriptor.size
        }) {
            return Err(LibraryError::ArtifactNotVerified(format!(
                "{} grant differs from its recorded artifact",
                item.item_id
            )));
        }
        persist_verified_artifact(&transaction, item, descriptor, &descriptor.grant_id, now)?;
        if operation.mode == DownloadMode::PrepareAndApply {
            transaction.execute(
                "INSERT INTO library_profile_content_bindings(
                   profile_mod_id, version_id, artifact_digest, operation_id, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(profile_mod_id) DO UPDATE SET
                   version_id = excluded.version_id,
                   artifact_digest = excluded.artifact_digest,
                   operation_id = excluded.operation_id,
                   updated_at_ms = excluded.updated_at_ms",
                params![
                    item.profile_mod_id,
                    item.resolved_version,
                    descriptor.digest,
                    operation_id,
                    now
                ],
            )?;
            if let Some(selection) = stored_selection(item)? {
                transaction.execute(
                    "UPDATE library_profile_mods
                     SET used_version = ?2, version_selection_kind = ?3,
                         selected_version_id = ?4, updated_at_ms = ?5
                     WHERE profile_mod_id = ?1",
                    params![
                        item.profile_mod_id,
                        item.resolved_version,
                        selection_kind_name(&selection),
                        selection.version_id,
                        now
                    ],
                )?;
            } else {
                transaction.execute(
                    "UPDATE library_profile_mods SET used_version = ?2, updated_at_ms = ?3
                     WHERE profile_mod_id = ?1",
                    params![item.profile_mod_id, item.resolved_version, now],
                )?;
            }
        }
        transaction.execute(
            "UPDATE library_download_operation_items
             SET artifact_digest = ?3, state = 'verified'
             WHERE operation_id = ?1 AND item_id = ?2",
            params![operation_id, item.item_id, descriptor.digest],
        )?;
        artifact_digests.push(descriptor.digest.clone());
    }

    let content_revision = if let Some(profile) = profile {
        let next = profile.content_revision.checked_add(1).ok_or_else(|| {
            LibraryError::InvalidRecord("content revision overflow".to_owned())
        })?;
        transaction.execute(
            "UPDATE library_profiles SET content_revision = ?2, updated_at_ms = ?3
             WHERE profile_id = ?1",
            params![operation.profile_id, next, now],
        )?;
        next
    } else {
        operation.expected_content_revision
    };
    let result = ApplyOperationResult {
        operation_id: operation_id.to_owned(),
        profile_id: operation.profile_id.clone(),
        content_revision,
        artifact_digests,
        applied: operation.mode == DownloadMode::PrepareAndApply,
    };
    transaction.execute(
        "UPDATE library_download_operations
         SET state = 'succeeded', result_json = ?2, error_code = NULL,
             updated_at_ms = ?3, completed_at_ms = ?3
         WHERE operation_id = ?1",
        params![operation_id, serde_json::to_string(&result)?, now],
    )?;
    transaction.execute(
        "UPDATE library_operation_leases
         SET state = 'released', heartbeat_at_ms = ?2, released_at_ms = ?2
         WHERE operation_id = ?1 AND fence_epoch = ?3 AND owner_generation = ?4",
        params![operation_id, now, fence_epoch, owner_generation],
    )?;
    insert_outbox(
        &transaction,
        Some(operation_id),
        if result.applied {
            "profile.content.changed"
        } else {
            "artifact.available"
        },
        "application-events",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn fail_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
    error_code: &str,
    details: Value,
) -> Result<FailedDownloadOperation, LibraryError> {
    authorization.require_content_write()?;
    if error_code.trim().is_empty() || error_code.len() > 100 {
        return Err(LibraryError::InvalidRecord(
            "download failure code is invalid".to_owned(),
        ));
    }
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let operation = load_operation(&transaction, operation_id)?;
    authorize_owner(authorization, &operation.owner_scope)?;
    if operation.state == DownloadOperationState::Failed {
        let result = operation
            .result
            .ok_or_else(|| LibraryError::OperationState("missing failure result".to_owned()))?;
        transaction.commit()?;
        return Ok(serde_json::from_value(result)?);
    }
    if operation.state.is_terminal() {
        return Err(LibraryError::OperationState(
            operation.state.as_str().to_owned(),
        ));
    }
    validate_active_lease(&transaction, operation_id, fence_epoch, owner_generation)?;
    let result = FailedDownloadOperation {
        operation_id: operation_id.to_owned(),
        state: DownloadOperationState::Failed,
        error_code: error_code.to_owned(),
        details,
    };
    transaction.execute(
        "UPDATE library_download_operations
         SET state = 'failed', result_json = ?2, error_code = ?3,
             updated_at_ms = ?4, completed_at_ms = ?4
         WHERE operation_id = ?1",
        params![
            operation_id,
            serde_json::to_string(&result)?,
            error_code,
            now
        ],
    )?;
    transaction.execute(
        "UPDATE library_operation_leases
         SET state = 'released', heartbeat_at_ms = ?2, released_at_ms = ?2
         WHERE operation_id = ?1 AND fence_epoch = ?3 AND owner_generation = ?4",
        params![operation_id, now, fence_epoch, owner_generation],
    )?;
    insert_outbox(
        &transaction,
        Some(operation_id),
        "operation.failed",
        "application-events",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn cancel_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
) -> Result<CancelOperationResult, LibraryError> {
    authorization.require_content_write()?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let operation = load_operation(&transaction, operation_id)?;
    authorize_owner(authorization, &operation.owner_scope)?;
    match operation.state {
        DownloadOperationState::Succeeded => {
            return Ok(CancelOperationResult::AlreadyCompleted(
                operation.result.unwrap_or(Value::Null),
            ));
        }
        DownloadOperationState::Cancelled => {
            return Ok(CancelOperationResult::AlreadyCancelled);
        }
        DownloadOperationState::Failed => {
            return Ok(CancelOperationResult::AlreadyCompleted(
                operation.result.unwrap_or(Value::Null),
            ));
        }
        _ => {}
    }
    if operation.mode == DownloadMode::PrepareAndApply {
        let profile = load_profile_gate(&transaction, &operation.profile_id)?;
        let revoked_epoch = profile
            .lease_epoch
            .checked_add(1)
            .ok_or_else(|| LibraryError::InvalidRecord("lease epoch overflow".to_owned()))?;
        transaction.execute(
            "UPDATE library_profiles SET lease_epoch = ?2, updated_at_ms = ?3
             WHERE profile_id = ?1",
            params![operation.profile_id, revoked_epoch, now],
        )?;
    }
    transaction.execute(
        "UPDATE library_operation_leases
         SET state = 'revoked', released_at_ms = ?2, heartbeat_at_ms = ?2
         WHERE operation_id = ?1",
        params![operation_id, now],
    )?;
    transaction.execute(
        "UPDATE library_download_operations
         SET state = 'cancelled', error_code = 'CANCELLED', updated_at_ms = ?2,
             completed_at_ms = ?2
         WHERE operation_id = ?1",
        params![operation_id, now],
    )?;
    transaction.execute(
        "UPDATE library_outbox SET delivery_state = 'cancelled'
         WHERE operation_id = ?1 AND event_type = ?2 AND delivery_state = 'pending'",
        params![operation_id, DOWNLOAD_EVENT],
    )?;
    insert_outbox(
        &transaction,
        Some(operation_id),
        "operation.cancelled",
        KERNEL_JOB_CONTROL_DESTINATION,
        &json!({
            "operationId": operation_id,
            "ownerScope": operation.owner_scope,
            "inputsHash": operation.inputs_hash,
        }),
        now,
    )?;
    transaction.commit()?;
    Ok(CancelOperationResult::Cancelled)
}

pub fn mark_expired_download_leases_suspect(
    &self,
    heartbeat_before_ms: i64,
) -> Result<usize, LibraryError> {
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let operation_ids = {
        let mut statement = transaction.prepare(
            "SELECT operation_id FROM library_operation_leases
             WHERE state = 'active' AND heartbeat_at_ms < ?1 ORDER BY resource_key",
        )?;
        let rows = statement.query_map(params![heartbeat_before_ms], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()?
    };
    for operation_id in &operation_ids {
        transaction.execute(
            "UPDATE library_operation_leases SET state = 'suspect'
             WHERE operation_id = ?1 AND state = 'active'",
            params![operation_id],
        )?;
        transaction.execute(
            "UPDATE library_download_operations
             SET state = 'recovery-required', error_code = 'LEASE_EXPIRED', updated_at_ms = ?2
             WHERE operation_id = ?1 AND state IN ('queued', 'running')",
            params![operation_id, now],
        )?;
        insert_outbox(
            &transaction,
            Some(operation_id),
            "operation.recovery-required",
            "application-events",
            &json!({"operationId": operation_id, "reason": "lease-expired"}),
            now,
        )?;
    }
    transaction.commit()?;
    Ok(operation_ids.len())
}

pub fn recover_download_operations(
    &self,
    current_generation: &str,
) -> Result<usize, LibraryError> {
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let operation_ids = {
        let mut statement = transaction.prepare(
            "SELECT operation_id FROM library_operation_leases
             WHERE state = 'active' AND owner_generation <> ?1 ORDER BY resource_key",
        )?;
        let rows = statement.query_map(params![current_generation], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()?
    };
    for operation_id in &operation_ids {
        transaction.execute(
            "UPDATE library_operation_leases SET state = 'suspect'
             WHERE operation_id = ?1 AND state = 'active'",
            params![operation_id],
        )?;
        transaction.execute(
            "UPDATE library_download_operations
             SET state = 'recovery-required', error_code = 'OWNER_RESTARTED', updated_at_ms = ?2
             WHERE operation_id = ?1 AND state IN ('queued', 'running')",
            params![operation_id, now],
        )?;
        insert_outbox(
            &transaction,
            Some(operation_id),
            "operation.recovery-required",
            "application-events",
            &json!({"operationId": operation_id, "reason": "owner-restarted"}),
            now,
        )?;
    }
    transaction.commit()?;
    Ok(operation_ids.len())
}

pub fn resume_download_operation(
    &self,
    authorization: &LibraryAuthorization,
    operation_id: &str,
    expected_content_revision: u64,
    owner_generation: &str,
) -> Result<DownloadOperationSnapshot, LibraryError> {
    authorization.require_content_write()?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut operation = load_operation(&transaction, operation_id)?;
    authorize_owner(authorization, &operation.owner_scope)?;
    if operation.state != DownloadOperationState::RecoveryRequired {
        return Err(LibraryError::OperationState(
            operation.state.as_str().to_owned(),
        ));
    }
    let next_fence = if operation.mode == DownloadMode::PrepareAndApply {
        let profile = load_profile_gate(&transaction, &operation.profile_id)?;
        ensure_revision(expected_content_revision, profile.content_revision)?;
        ensure_revision(
            operation.expected_content_revision,
            profile.content_revision,
        )?;
        if profile.incarnation_id != operation.target_incarnation_id {
            return Err(LibraryError::StaleTarget(operation.profile_id.clone()));
        }
        let next_fence = profile
            .lease_epoch
            .checked_add(1)
            .ok_or_else(|| LibraryError::InvalidRecord("lease epoch overflow".to_owned()))?;
        transaction.execute(
            "UPDATE library_profiles SET lease_epoch = ?2, updated_at_ms = ?3
             WHERE profile_id = ?1",
            params![operation.profile_id, next_fence, now],
        )?;
        next_fence
    } else {
        operation
            .fence_epoch
            .checked_add(1)
            .ok_or_else(|| LibraryError::InvalidRecord("lease epoch overflow".to_owned()))?
    };
    let changed = transaction.execute(
        "UPDATE library_operation_leases
         SET fence_epoch = ?2, owner_generation = ?3, state = 'active',
             heartbeat_at_ms = ?4, released_at_ms = NULL
         WHERE operation_id = ?1 AND state = 'suspect'",
        params![operation_id, next_fence, owner_generation, now],
    )?;
    if changed == 0 {
        return Err(LibraryError::StaleOperation);
    }
    transaction.execute(
        "UPDATE library_download_operations
         SET fence_epoch = ?2, owner_generation = ?3, state = 'queued',
             error_code = NULL, updated_at_ms = ?4
         WHERE operation_id = ?1",
        params![operation_id, next_fence, owner_generation, now],
    )?;
    insert_outbox(
        &transaction,
        Some(operation_id),
        DOWNLOAD_EVENT,
        KERNEL_JOB_DESTINATION,
        &json!({
            "operationId": operation_id,
            "ownerScope": operation.owner_scope,
            "inputsHash": operation.inputs_hash,
            "profileId": operation.profile_id,
            "mode": operation.mode,
            "fenceEpoch": next_fence,
            "ownerGeneration": owner_generation,
            "resumed": true,
        }),
        now,
    )?;
    operation.fence_epoch = next_fence;
    operation.owner_generation = owner_generation.to_owned();
    operation.state = DownloadOperationState::Queued;
    operation.updated_at_ms = now;
    transaction.commit()?;
    Ok(operation.into_snapshot())
}

pub fn retained_artifact_digests(
    &self,
    authorization: &LibraryAuthorization,
) -> Result<Vec<String>, LibraryError> {
    authorization.require_content_write()?;
    let connection = self.connection()?;
    let mut statement = connection.prepare(
        "SELECT digest FROM library_content_refs WHERE verified = 1 ORDER BY digest",
    )?;
    let digests = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(digests)
}

pub fn profile_content_capability(
    &self,
    authorization: &LibraryAuthorization,
    profile_id: &str,
) -> Result<ProfileContentCapability, LibraryError> {
    let connection = self.connection()?;
    let (owner_scope, content_revision) = connection
        .query_row(
            "SELECT owner_scope, content_revision FROM library_profiles
             WHERE profile_id = ?1 AND deleted_at_ms IS NULL",
            params![profile_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(profile_id.to_owned()))?;
    authorize_owner(authorization, &owner_scope)?;
    let lease = connection
        .query_row(
            "SELECT operation_id, state FROM library_operation_leases
             WHERE resource_key = ?1 AND state IN ('active', 'suspect')",
            params![content_resource_key(profile_id)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(match lease {
        Some((operation_id, state)) => ProfileContentCapability {
            content_revision,
            writable: false,
            busy_operation_id: Some(operation_id),
            recovery_required: state == "suspect",
        },
        None => ProfileContentCapability {
            content_revision,
            writable: true,
            busy_operation_id: None,
            recovery_required: false,
        },
    })
}

pub fn pending_outbox(
    &self,
    destination: &str,
    limit: usize,
) -> Result<Vec<OutboxEvent>, LibraryError> {
    let connection = self.connection()?;
    let mut statement = connection.prepare(
        "SELECT event_seq, event_id, operation_id, event_type, destination,
                payload_json, created_at_ms
         FROM library_outbox
         WHERE destination = ?1 AND delivery_state = 'pending'
         ORDER BY event_seq LIMIT ?2",
    )?;
    let rows = statement.query_map(params![destination, limit as u64], |row| {
        let payload_json: String = row.get(5)?;
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            payload_json,
            row.get::<_, i64>(6)?,
        ))
    })?;
    rows.map(|row| {
        let (
            event_seq,
            event_id,
            operation_id,
            event_type,
            destination,
            payload_json,
            created_at_ms,
        ) = row?;
        Ok(OutboxEvent {
            event_seq,
            event_id,
            operation_id,
            event_type,
            destination,
            payload: serde_json::from_str(&payload_json)?,
            created_at_ms,
        })
    })
    .collect()
}

pub fn mark_outbox_delivered(
    &self,
    event_id: &str,
    delivery_ref: &str,
) -> Result<bool, LibraryError> {
    let connection = self.connection()?;
    let changed = connection.execute(
        "UPDATE library_outbox
         SET delivery_state = 'delivered', delivery_ref = ?2, delivered_at_ms = ?3
         WHERE event_id = ?1 AND delivery_state = 'pending'",
        params![event_id, delivery_ref, now_ms()?],
    )?;
    Ok(changed == 1)
}

#[cfg(test)]
pub(crate) fn profile_content_binding(
    &self,
    profile_mod_id: &str,
) -> Result<Option<(String, String, String)>, LibraryError> {
    let connection = self.connection()?;
    Ok(connection
        .query_row(
            "SELECT version_id, artifact_digest, operation_id
             FROM library_profile_content_bindings WHERE profile_mod_id = ?1",
            params![profile_mod_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
} }

impl StoredOperation {
    fn into_snapshot(self) -> DownloadOperationSnapshot {
        let resource_key = operation_resource_key(&self.operation_id, &self.profile_id, &self.mode);
        DownloadOperationSnapshot {
            operation_id: self.operation_id.clone(),
            owner_scope: self.owner_scope,
            profile_id: self.profile_id.clone(),
            mode: self.mode,
            inputs_hash: self.inputs_hash,
            state: self.state,
            fence: OperationFence {
                operation_id: self.operation_id,
                resource_key,
                fence_epoch: self.fence_epoch,
                owner_generation: self.owner_generation,
                expected_content_revision: self.expected_content_revision,
                target_incarnation_id: self.target_incarnation_id,
            },
            result: self.result,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

fn cache_resource_key(operation_id: &str) -> String {
    format!("download-cache:{operation_id}")
}

fn operation_resource_key(operation_id: &str, profile_id: &str, mode: &DownloadMode) -> String {
    match mode {
        DownloadMode::CacheOnly => cache_resource_key(operation_id),
        DownloadMode::PrepareAndApply => content_resource_key(profile_id),
    }
}

fn selection_kind_name(selection: &VersionSelection) -> &'static str {
    match &selection.kind {
        VersionSelectionKind::Auto => "auto",
        VersionSelectionKind::Pinned => "pinned",
    }
}

fn validate_version_selection(selection: &VersionSelection) -> Result<(), LibraryError> {
    match (&selection.kind, selection.version_id.as_deref()) {
        (VersionSelectionKind::Auto, None) => Ok(()),
        (VersionSelectionKind::Pinned, Some(version_id)) if !version_id.is_empty() => Ok(()),
        (VersionSelectionKind::Auto, Some(_)) => Err(LibraryError::InvalidRecord(
            "automatic version selection cannot include versionId".to_owned(),
        )),
        (VersionSelectionKind::Pinned, _) => Err(LibraryError::InvalidRecord(
            "pinned version selection requires versionId".to_owned(),
        )),
    }
}

fn stored_selection(item: &StoredItem) -> Result<Option<VersionSelection>, LibraryError> {
    match (
        item.apply_selection_kind.as_deref(),
        item.apply_selected_version_id.as_deref(),
    ) {
        (None, None) => Ok(None),
        (Some("auto"), None) => Ok(Some(VersionSelection {
            kind: VersionSelectionKind::Auto,
            version_id: None,
        })),
        (Some("pinned"), Some(version_id)) if !version_id.is_empty() => {
            Ok(Some(VersionSelection {
                kind: VersionSelectionKind::Pinned,
                version_id: Some(version_id.to_owned()),
            }))
        }
        _ => Err(LibraryError::InvalidRecord(format!(
            "operation item {} has an invalid stored selection",
            item.item_id
        ))),
    }
}

fn validate_start_request(request: &StartDownloadOperation) -> Result<(), LibraryError> {
    if request.idempotency_scope.is_empty()
        || request.idempotency_key.is_empty()
        || request.profile_id.is_empty()
        || request.owner_generation.is_empty()
        || request.items.is_empty()
    {
        return Err(LibraryError::InvalidRecord(
            "download operation identifiers and items must not be empty".to_owned(),
        ));
    }
    if !request.module_lock_digest.strip_prefix("sha256:")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
        return Err(LibraryError::InvalidRecord("download requires an exact source module lock digest".to_owned()));
    }
    let mut item_ids = HashSet::with_capacity(request.items.len());
    let mut profile_mod_ids = HashSet::with_capacity(request.items.len());
    for item in &request.items {
        if item.item_id.is_empty()
            || item.profile_mod_id.is_empty()
            || item.mod_id.is_empty()
            || item.source_id.is_empty()
            || item.source_mod_id.is_empty()
            || item.remote_game_key.is_empty()
            || item.resolved_version.is_empty()
        {
            return Err(LibraryError::InvalidRecord(
                "download item identifiers must not be empty".to_owned(),
            ));
        }
        if let Some(selection) = &item.selection {
            validate_version_selection(selection)?;
        }
        if !item_ids.insert(item.item_id.as_str())
            || !profile_mod_ids.insert(item.profile_mod_id.as_str())
        {
            return Err(LibraryError::InvalidRecord(
                "download operation contains duplicate items".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_start_item(
    transaction: &Transaction<'_>,
    profile_id: &str,
    item: &StartOperationItem,
) -> Result<(), LibraryError> {
    let current = transaction
        .query_row(
            "SELECT pm.incarnation_id, pm.mod_id, m.source_id, m.remote_game_key,
                    m.remote_mod_id, v.remote_file_id
             FROM library_profile_mods pm
             JOIN library_mods m ON m.mod_id = pm.mod_id
             JOIN library_mod_versions v ON v.mod_id = m.mod_id AND v.version_id = ?3
             WHERE pm.profile_mod_id = ?1 AND pm.profile_id = ?2",
            params![item.profile_mod_id, profile_id, item.resolved_version],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(item.profile_mod_id.clone()))?;
    if current.0 != item.profile_mod_incarnation_id
        || current.1 != item.mod_id
        || current.2 != item.source_id
        || current.3 != item.remote_game_key
        || current.4 != item.source_mod_id
        || current.5 != item.source_file_id
    {
        return Err(LibraryError::StaleTarget(item.profile_mod_id.clone()));
    }
    Ok(())
}

fn validate_pinned_item(
    transaction: &Transaction<'_>,
    profile_id: &str,
    item: &StoredItem,
) -> Result<(), LibraryError> {
    let pinned = StartOperationItem {
        item_id: item.item_id.clone(),
        profile_mod_id: item.profile_mod_id.clone(),
        profile_mod_incarnation_id: item.profile_mod_incarnation_id.clone(),
        mod_id: item.mod_id.clone(),
        source_id: item.source_id.clone(),
        source_mod_id: item.source_mod_id.clone(),
        remote_game_key: item.remote_game_key.clone(),
        source_file_id: item.source_file_id.clone(),
        resolved_version: item.resolved_version.clone(),
        expected_digest: item.expected_digest.clone(),
        validator: item.validator.clone(),
        selection: stored_selection(item)?,
    };
    validate_start_item(transaction, profile_id, &pinned)
}

fn load_profile_gate(
    transaction: &Transaction<'_>,
    profile_id: &str,
) -> Result<ProfileGateState, LibraryError> {
    transaction
        .query_row(
            "SELECT owner_scope, content_revision, lease_epoch, incarnation_id
             FROM library_profiles WHERE profile_id = ?1 AND deleted_at_ms IS NULL",
            params![profile_id],
            |row| {
                Ok(ProfileGateState {
                    owner_scope: row.get(0)?,
                    content_revision: row.get(1)?,
                    lease_epoch: row.get(2)?,
                    incarnation_id: row.get(3)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(profile_id.to_owned()))
}

fn load_operation(
    transaction: &Transaction<'_>,
    operation_id: &str,
) -> Result<StoredOperation, LibraryError> {
    query_operation(transaction, operation_id)
}

fn load_operation_connection(
    connection: &rusqlite::Connection,
    operation_id: &str,
) -> Result<StoredOperation, LibraryError> {
    query_operation(connection, operation_id)
}

fn query_operation(
    connection: &rusqlite::Connection,
    operation_id: &str,
) -> Result<StoredOperation, LibraryError> {
    let stored = connection
        .query_row(
            "SELECT operation_id, owner_scope, inputs_hash, profile_id, mode,
                    expected_content_revision, target_incarnation_id, owner_generation,
                    fence_epoch, state, result_json, created_at_ms, updated_at_ms
             FROM library_download_operations WHERE operation_id = ?1",
            params![operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, u64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, u64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(operation_id.to_owned()))?;
    Ok(StoredOperation {
        operation_id: stored.0,
        owner_scope: stored.1,
        inputs_hash: stored.2,
        profile_id: stored.3,
        mode: parse_download_mode(&stored.4)?,
        expected_content_revision: stored.5,
        target_incarnation_id: stored.6,
        owner_generation: stored.7,
        fence_epoch: stored.8,
        state: DownloadOperationState::parse(&stored.9)?,
        result: stored
            .10
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        created_at_ms: stored.11,
        updated_at_ms: stored.12,
    })
}

fn load_operation_items(
    transaction: &Transaction<'_>,
    operation_id: &str,
) -> Result<Vec<StoredItem>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT i.item_id, i.profile_mod_id, i.profile_mod_incarnation_id, i.mod_id,
                i.source_id, i.source_mod_id, i.remote_game_key, i.source_file_id,
                i.resolved_version, i.expected_digest, i.validator,
                i.apply_selection_kind, i.apply_selected_version_id,
                i.artifact_digest, c.size
         FROM library_download_operation_items i
         LEFT JOIN library_content_refs c ON c.digest = i.artifact_digest
         WHERE i.operation_id = ?1 ORDER BY i.item_id",
    )?;
    let rows = statement.query_map(params![operation_id], |row| {
        let digest = row.get::<_, Option<String>>(13)?;
        let size = row.get::<_, Option<u64>>(14)?;
        let artifact = match (digest, size) {
            (Some(digest), Some(size)) => Some(ArtifactRef {
                digest,
                size,
                verified: true,
                media_type: None,
            }),
            _ => None,
        };
        Ok(StoredItem {
            item_id: row.get(0)?,
            profile_mod_id: row.get(1)?,
            profile_mod_incarnation_id: row.get(2)?,
            mod_id: row.get(3)?,
            source_id: row.get(4)?,
            source_mod_id: row.get(5)?,
            remote_game_key: row.get(6)?,
            source_file_id: row.get(7)?,
            resolved_version: row.get(8)?,
            expected_digest: row.get(9)?,
            validator: row.get(10)?,
            apply_selection_kind: row.get(11)?,
            apply_selected_version_id: row.get(12)?,
            artifact,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn load_execution_plan(
    transaction: &Transaction<'_>,
    operation_id: &str,
) -> Result<DownloadExecutionPlan, LibraryError> {
    let operation = load_operation(transaction, operation_id)?;
    let items = load_operation_items(transaction, operation_id)?
        .into_iter()
        .map(|item| {
            let selection = stored_selection(&item)?;
            Ok(DownloadExecutionItem {
                item_id: item.item_id,
                profile_mod_id: item.profile_mod_id,
                profile_mod_incarnation_id: item.profile_mod_incarnation_id,
                mod_id: item.mod_id,
                source_id: item.source_id,
                remote_game_key: item.remote_game_key,
                source_mod_id: item.source_mod_id,
                source_file_id: item.source_file_id,
                resolved_version: item.resolved_version,
                expected_digest: item.expected_digest,
                validator: item.validator,
                selection,
                artifact: item.artifact,
            })
        })
        .collect::<Result<Vec<_>, LibraryError>>()?;
    Ok(DownloadExecutionPlan {
        operation: operation.into_snapshot(),
        items,
    })
}

fn persist_verified_artifact(
    transaction: &Transaction<'_>,
    item: &StoredItem,
    descriptor: &ArtifactGrantDescriptor,
    grant_id: &str,
    now: i64,
) -> Result<(), LibraryError> {
    if item
        .expected_digest
        .as_ref()
        .is_some_and(|digest| digest != &descriptor.digest)
    {
        return Err(LibraryError::ArtifactNotVerified(format!(
            "{} digest mismatch",
            item.item_id
        )));
    }
    transaction.execute(
        "INSERT INTO library_content_refs(
           digest, size, verified, grant_id, created_at_ms
         ) VALUES (?1, ?2, 1, ?3, ?4)
         ON CONFLICT(digest) DO NOTHING",
        params![descriptor.digest, descriptor.size, grant_id, now],
    )?;
    let stored_size: u64 = transaction.query_row(
        "SELECT size FROM library_content_refs WHERE digest = ?1 AND verified = 1",
        params![descriptor.digest],
        |row| row.get(0),
    )?;
    if stored_size != descriptor.size {
        return Err(LibraryError::ArtifactNotVerified(format!(
            "{} size mismatch",
            item.item_id
        )));
    }
    let changed = transaction.execute(
        "UPDATE library_mod_versions
         SET artifact_digest = ?3, file_size = ?4, availability = 'remote',
             legacy_location = NULL
         WHERE mod_id = ?1 AND version_id = ?2",
        params![
            item.mod_id,
            item.resolved_version,
            descriptor.digest,
            descriptor.size
        ],
    )?;
    if changed != 1 {
        return Err(LibraryError::StaleTarget(item.item_id.clone()));
    }
    Ok(())
}

fn validate_active_lease(
    transaction: &Transaction<'_>,
    operation_id: &str,
    fence_epoch: u64,
    owner_generation: &str,
) -> Result<(), LibraryError> {
    let lease = transaction
        .query_row(
            "SELECT fence_epoch, owner_generation, state FROM library_operation_leases
             WHERE operation_id = ?1",
            params![operation_id],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(LibraryError::StaleOperation)?;
    if lease.0 != fence_epoch {
        return Err(LibraryError::StaleFence {
            expected: lease.0,
            actual: fence_epoch,
        });
    }
    if lease.1 != owner_generation || lease.2 != "active" {
        return Err(LibraryError::StaleOperation);
    }
    Ok(())
}

fn ensure_resource_available(
    transaction: &Transaction<'_>,
    resource_key: &str,
) -> Result<(), LibraryError> {
    let owner = transaction
        .query_row(
            "SELECT owner_id FROM (
               SELECT operation_id AS owner_id
               FROM library_operation_leases
               WHERE resource_key = ?1 AND state IN ('active', 'suspect')
               UNION ALL
               SELECT guard_id AS owner_id
               FROM library_profile_content_guards
               WHERE resource_key = ?1 AND state = 'active'
             ) LIMIT 1",
            params![resource_key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(owner_operation_id) = owner {
        return Err(LibraryError::ResourceBusy { owner_operation_id });
    }
    Ok(())
}

fn insert_outbox(
    transaction: &Transaction<'_>,
    operation_id: Option<&str>,
    event_type: &str,
    destination: &str,
    payload: &Value,
    now: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO library_outbox(
           event_id, operation_id, event_type, destination, payload_json,
           delivery_state, delivery_ref, created_at_ms, delivered_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, ?6, NULL)",
        params![
            Uuid::new_v4().to_string(),
            operation_id,
            event_type,
            destination,
            serde_json::to_string(payload)?,
            now
        ],
    )?;
    Ok(())
}

fn authorize_owner(
    authorization: &LibraryAuthorization,
    owner_scope: &str,
) -> Result<(), LibraryError> {
    (authorization.owner_scope() == owner_scope)
        .then_some(())
        .ok_or_else(|| LibraryError::PermissionDenied(owner_scope.to_owned()))
}

fn ensure_revision(expected: u64, actual: u64) -> Result<(), LibraryError> {
    (expected == actual)
        .then_some(())
        .ok_or(LibraryError::RevisionConflict { expected, actual })
}

fn content_resource_key(profile_id: &str) -> String {
    format!("{CONTENT_RESOURCE_PREFIX}{profile_id}")
}

fn download_mode_name(mode: &DownloadMode) -> &'static str {
    match mode {
        DownloadMode::CacheOnly => "cache-only",
        DownloadMode::PrepareAndApply => "prepare-and-apply",
    }
}

fn parse_download_mode(value: &str) -> Result<DownloadMode, LibraryError> {
    match value {
        "cache-only" => Ok(DownloadMode::CacheOnly),
        "prepare-and-apply" => Ok(DownloadMode::PrepareAndApply),
        other => Err(LibraryError::InvalidRecord(format!(
            "unknown download mode {other}"
        ))),
    }
}

fn hash_json(value: &Value) -> Result<String, LibraryError> {
    let encoded = serde_json::to_vec(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}

fn now_ms() -> Result<i64, LibraryError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| LibraryError::InvalidRecord(error.to_string()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| LibraryError::InvalidRecord("timestamp overflow".to_owned()))
}
