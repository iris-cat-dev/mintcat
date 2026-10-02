use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_runtime_jobs::{request_hash, ActionBinding, ActionSpec, JobStore};
use mintcat_runtime_kernel::KernelDatabase;
use mintcat_sdk::ActionRequired;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use url::Url;
use uuid::Uuid;

use crate::{BrokerError, Origin};

const MIGRATION: &str = r#"
CREATE TABLE broker_interaction_grants (
  grant_id TEXT PRIMARY KEY,
  action_id TEXT NOT NULL UNIQUE,
  owner_scope TEXT NOT NULL,
  kind TEXT NOT NULL,
  resource_json TEXT NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  consumed_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX broker_grants_owner_state
  ON broker_interaction_grants(owner_scope, consumed_at_ms, expires_at_ms);
"#;

#[derive(Debug, Clone)]
pub struct InteractionContext {
    pub operation_id: String,
    pub target_revision: u64,
    pub plan_hash: String,
    pub module_lock_digest: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct FileSelectionRequest {
    pub purpose: String,
    pub allowed_extensions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DirectorySelectionRequest {
    pub purpose: String,
}

#[derive(Debug)]
pub struct AuthorizedFile {
    file: File,
    file_name: String,
    size: u64,
}

impl AuthorizedFile {
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn into_file(self) -> File {
        self.file
    }
}

#[derive(Debug)]
pub struct AuthorizedDirectory {
    root: PathBuf,
    directory_name: String,
}

#[derive(Debug, Clone, Copy)]
pub struct InputSnapshotLimits {
    pub max_entries: u64,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshotEntry {
    pub relative_path: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshot {
    pub entries: Vec<DirectorySnapshotEntry>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputFingerprint {
    relative_path: String,
    size: u64,
    modified_ns: u128,
}

impl AuthorizedDirectory {
    pub fn snapshot_to(
        &self,
        destination: &Path,
        limits: InputSnapshotLimits,
    ) -> Result<DirectorySnapshot, BrokerError> {
        snapshot_directory(&self.root, destination, limits)
    }

    pub fn directory_name(&self) -> &str {
        &self.directory_name
    }
}

#[derive(Debug, Clone)]
pub struct BrowserAuthorizationRequest {
    pub authorization_url: Url,
    pub allowed_navigation_origins: Vec<Origin>,
    pub callback_origins: Vec<Origin>,
}

#[derive(Debug, Clone)]
pub struct BrowserDownloadRequest {
    pub download_page_url: Url,
    pub allowed_navigation_origins: Vec<Origin>,
    pub callback_origins: Vec<Origin>,
    pub callback_schemes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserNavigationDecision {
    Allow,
    CompleteCallback,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileTarget {
    purpose: String,
    allowed_extensions: Vec<String>,
    expires_at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectoryTarget {
    purpose: String,
    expires_at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserTarget {
    authorization_url: String,
    allowed_navigation_origins: Vec<String>,
    callback_origins: Vec<String>,
    #[serde(default)]
    callback_schemes: Vec<String>,
    expires_at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrantResponse {
    grant_id: String,
}

struct GrantCompletion<'a> {
    owner_scope: &'a str,
    action_id: &'a str,
    expected_revision: u64,
    idempotency_key: &'a str,
    kind: &'static str,
    expires_at_ms: i64,
    resource: Value,
}

#[derive(Clone)]
pub struct InteractionClient {
    database: KernelDatabase,
    jobs: JobStore,
}

#[derive(Clone)]
pub struct InteractionHost {
    database: KernelDatabase,
    jobs: JobStore,
}

pub fn open_interaction_broker(
    database: KernelDatabase,
    jobs: JobStore,
) -> Result<(InteractionClient, InteractionHost), BrokerError> {
    database.apply_migration("runtime.broker", 1, MIGRATION)?;
    Ok((
        InteractionClient {
            database: database.clone(),
            jobs: jobs.clone(),
        },
        InteractionHost { database, jobs },
    ))
}

impl InteractionClient {
    pub fn request_file_selection(
        &self,
        job_id: &str,
        context: &InteractionContext,
        request: FileSelectionRequest,
    ) -> Result<ActionRequired, BrokerError> {
        if request.purpose.trim().is_empty() {
            return Err(BrokerError::InvalidRequest(
                "file selection purpose cannot be empty".to_owned(),
            ));
        }
        let allowed_extensions = normalize_extensions(request.allowed_extensions)?;
        let target = serde_json::to_value(FileTarget {
            purpose: request.purpose,
            allowed_extensions,
            expires_at_ms: context.expires_at_ms,
        })?;
        self.create_action(job_id, context, "select-file", target)
    }

    pub fn request_directory_selection(
        &self,
        job_id: &str,
        context: &InteractionContext,
        request: DirectorySelectionRequest,
    ) -> Result<ActionRequired, BrokerError> {
        if request.purpose.trim().is_empty() {
            return Err(BrokerError::InvalidRequest(
                "directory selection purpose cannot be empty".to_owned(),
            ));
        }
        let target = serde_json::to_value(DirectoryTarget {
            purpose: request.purpose,
            expires_at_ms: context.expires_at_ms,
        })?;
        self.create_action(job_id, context, "select-directory", target)
    }

    pub fn request_browser_authorization(
        &self,
        job_id: &str,
        context: &InteractionContext,
        request: BrowserAuthorizationRequest,
    ) -> Result<ActionRequired, BrokerError> {
        let authorization_origin = Origin::from_url(&request.authorization_url)?;
        if request.allowed_navigation_origins.is_empty()
            || !request
                .allowed_navigation_origins
                .contains(&authorization_origin)
        {
            return Err(BrokerError::InvalidRequest(
                "authorization URL is outside the navigation scope".to_owned(),
            ));
        }
        if request.callback_origins.is_empty() {
            return Err(BrokerError::InvalidRequest(
                "browser authorization needs a callback origin".to_owned(),
            ));
        }
        let target = serde_json::to_value(BrowserTarget {
            authorization_url: request.authorization_url.into(),
            allowed_navigation_origins: request
                .allowed_navigation_origins
                .into_iter()
                .map(|origin| origin.as_str().to_owned())
                .collect(),
            callback_origins: request
                .callback_origins
                .into_iter()
                .map(|origin| origin.as_str().to_owned())
                .collect(),
            callback_schemes: Vec::new(),
            expires_at_ms: context.expires_at_ms,
        })?;
        self.create_action(job_id, context, "browser-authorization", target)
    }

    pub fn request_browser_download(
        &self,
        job_id: &str,
        context: &InteractionContext,
        request: BrowserDownloadRequest,
    ) -> Result<ActionRequired, BrokerError> {
        let start_origin = Origin::from_url(&request.download_page_url)?;
        if request.allowed_navigation_origins.is_empty()
            || !request.allowed_navigation_origins.contains(&start_origin)
        {
            return Err(BrokerError::InvalidRequest(
                "download page URL is outside the navigation scope".to_owned(),
            ));
        }
        let callback_schemes = normalize_schemes(request.callback_schemes)?;
        if request.callback_origins.is_empty() && callback_schemes.is_empty() {
            return Err(BrokerError::InvalidRequest(
                "browser download needs a callback origin or scheme".to_owned(),
            ));
        }
        let target = serde_json::to_value(BrowserTarget {
            authorization_url: request.download_page_url.into(),
            allowed_navigation_origins: request
                .allowed_navigation_origins
                .into_iter()
                .map(|origin| origin.as_str().to_owned())
                .collect(),
            callback_origins: request
                .callback_origins
                .into_iter()
                .map(|origin| origin.as_str().to_owned())
                .collect(),
            callback_schemes,
            expires_at_ms: context.expires_at_ms,
        })?;
        self.create_action(job_id, context, "browser-download", target)
    }

    pub fn open_selected_file(
        &self,
        action_id: &str,
        binding: &ActionBinding,
    ) -> Result<File, BrokerError> {
        self.consume_selected_file(action_id, binding)
            .map(AuthorizedFile::into_file)
    }

    pub fn consume_selected_file(
        &self,
        action_id: &str,
        binding: &ActionBinding,
    ) -> Result<AuthorizedFile, BrokerError> {
        let consumed = self.jobs.consume_action(action_id, binding)?;
        let response: GrantResponse = serde_json::from_value(consumed.response)?;
        let resource =
            self.load_available_grant(&response.grant_id, action_id, "file", &consumed.action)?;
        let path = grant_path(&resource, &response.grant_id)?;
        let file = open_regular_file(&path)?;
        let metadata = file.metadata()?;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| BrokerError::GrantUnavailable(response.grant_id.clone()))?
            .to_owned();
        self.consume_grant(&response.grant_id)?;
        Ok(AuthorizedFile {
            file,
            file_name,
            size: metadata.len(),
        })
    }

    pub fn consume_selected_directory(
        &self,
        action_id: &str,
        binding: &ActionBinding,
    ) -> Result<AuthorizedDirectory, BrokerError> {
        let consumed = self.jobs.consume_action(action_id, binding)?;
        let response: GrantResponse = serde_json::from_value(consumed.response)?;
        let resource = self.load_available_grant(
            &response.grant_id,
            action_id,
            "directory",
            &consumed.action,
        )?;
        let path = grant_path(&resource, &response.grant_id)?;
        let canonical = path.canonicalize()?;
        if canonical != path || !fs::symlink_metadata(&canonical)?.is_dir() {
            return Err(BrokerError::InputChanged);
        }
        let directory_name = canonical
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| BrokerError::GrantUnavailable(response.grant_id.clone()))?
            .to_owned();
        self.consume_grant(&response.grant_id)?;
        Ok(AuthorizedDirectory {
            root: canonical,
            directory_name,
        })
    }

    pub fn consume_browser_callback(
        &self,
        action_id: &str,
        binding: &ActionBinding,
    ) -> Result<Url, BrokerError> {
        let consumed = self.jobs.consume_action(action_id, binding)?;
        let response: GrantResponse = serde_json::from_value(consumed.response)?;
        let resource = self.load_available_grant(
            &response.grant_id,
            action_id,
            "browser-callback",
            &consumed.action,
        )?;
        let callback = resource
            .get("callbackUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| BrokerError::GrantUnavailable(response.grant_id.clone()))?;
        let callback = Url::parse(callback)
            .map_err(|_| BrokerError::GrantUnavailable(response.grant_id.clone()))?;
        self.consume_grant(&response.grant_id)?;
        Ok(callback)
    }

    fn create_action(
        &self,
        job_id: &str,
        context: &InteractionContext,
        kind: &str,
        target: Value,
    ) -> Result<ActionRequired, BrokerError> {
        validate_context(context)?;
        Ok(self.jobs.create_action(
            job_id,
            ActionSpec {
                kind: kind.to_owned(),
                operation_id: context.operation_id.clone(),
                target,
                target_revision: context.target_revision,
                plan_hash: context.plan_hash.clone(),
                module_lock_digest: context.module_lock_digest.clone(),
                expires_at_ms: context.expires_at_ms,
                response_schema: grant_response_schema(),
            },
        )?)
    }

    fn load_available_grant(
        &self,
        grant_id: &str,
        action_id: &str,
        kind: &str,
        action: &ActionRequired,
    ) -> Result<Value, BrokerError> {
        let connection = self.database.connection()?;
        connection
            .query_row(
                "SELECT resource_json FROM broker_interaction_grants
                 WHERE grant_id = ?1 AND action_id = ?2 AND owner_scope = (
                   SELECT owner_scope FROM actions WHERE action_id = ?2
                 ) AND kind = ?3 AND consumed_at_ms IS NULL AND expires_at_ms > ?4",
                params![grant_id, action_id, kind, now_ms()?],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| BrokerError::GrantUnavailable(action.action_id.clone()))
            .and_then(|value| Ok(serde_json::from_str(&value)?))
    }

    fn consume_grant(&self, grant_id: &str) -> Result<(), BrokerError> {
        let connection = self.database.connection()?;
        let updated = connection.execute(
            "UPDATE broker_interaction_grants
             SET consumed_at_ms = ?2, resource_json = 'null'
             WHERE grant_id = ?1 AND consumed_at_ms IS NULL",
            params![grant_id, now_ms()?],
        )?;
        if updated != 1 {
            return Err(BrokerError::GrantUnavailable(grant_id.to_owned()));
        }
        Ok(())
    }
}

impl InteractionHost {
    pub fn validate_browser_navigation(
        &self,
        owner_scope: &str,
        action_id: &str,
        navigation_url: &Url,
    ) -> Result<BrowserNavigationDecision, BrokerError> {
        let action = self.jobs.get_action(owner_scope, action_id)?;
        ensure_browser_action_kind(&action.kind)?;
        let target: BrowserTarget = serde_json::from_value(action.target)?;
        if target
            .callback_schemes
            .iter()
            .any(|allowed| allowed == navigation_url.scheme())
        {
            return Ok(BrowserNavigationDecision::CompleteCallback);
        }
        let origin = Origin::from_url(navigation_url)?;
        if target
            .callback_origins
            .iter()
            .any(|allowed| allowed == origin.as_str())
        {
            return Ok(BrowserNavigationDecision::CompleteCallback);
        }
        if target
            .allowed_navigation_origins
            .iter()
            .any(|allowed| allowed == origin.as_str())
        {
            return Ok(BrowserNavigationDecision::Allow);
        }
        Err(BrokerError::InteractionMismatch(
            "browser navigation is outside the declared origin scope".to_owned(),
        ))
    }

    pub fn complete_file_selection(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        selected_path: &Path,
        idempotency_key: &str,
    ) -> Result<(), BrokerError> {
        let action = self.jobs.get_action(owner_scope, action_id)?;
        if action.kind != "select-file" {
            return Err(BrokerError::InteractionMismatch(action.kind));
        }
        let target: FileTarget = serde_json::from_value(action.target.clone())?;
        let canonical = selected_path.canonicalize()?;
        if !canonical.is_file() {
            return Err(BrokerError::InteractionMismatch(
                "selected resource is not a file".to_owned(),
            ));
        }
        validate_file_extension(&canonical, &target.allowed_extensions)?;
        let resource = json!({
            "path": canonical,
            "size": canonical.metadata()?.len(),
        });
        self.complete_with_grant(GrantCompletion {
            owner_scope,
            action_id,
            expected_revision,
            idempotency_key,
            kind: "file",
            expires_at_ms: target.expires_at_ms,
            resource,
        })
    }

    pub fn complete_directory_selection(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        selected_path: &Path,
        idempotency_key: &str,
    ) -> Result<(), BrokerError> {
        let action = self.jobs.get_action(owner_scope, action_id)?;
        if action.kind != "select-directory" {
            return Err(BrokerError::InteractionMismatch(action.kind));
        }
        let target: DirectoryTarget = serde_json::from_value(action.target.clone())?;
        let canonical = selected_path.canonicalize()?;
        if !canonical.is_dir() {
            return Err(BrokerError::InteractionMismatch(
                "selected resource is not a directory".to_owned(),
            ));
        }
        self.complete_with_grant(GrantCompletion {
            owner_scope,
            action_id,
            expected_revision,
            idempotency_key,
            kind: "directory",
            expires_at_ms: target.expires_at_ms,
            resource: json!({ "path": canonical }),
        })
    }

    pub fn complete_browser_authorization(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        callback_url: &Url,
        idempotency_key: &str,
    ) -> Result<(), BrokerError> {
        self.complete_browser_callback(
            owner_scope,
            action_id,
            expected_revision,
            callback_url,
            idempotency_key,
            "browser-authorization",
        )
    }

    pub fn complete_browser_download(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        callback_url: &Url,
        idempotency_key: &str,
    ) -> Result<(), BrokerError> {
        self.complete_browser_callback(
            owner_scope,
            action_id,
            expected_revision,
            callback_url,
            idempotency_key,
            "browser-download",
        )
    }

    pub fn reject_interaction(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        reason: &str,
        idempotency_key: &str,
    ) -> Result<(), BrokerError> {
        let action = self.jobs.get_action(owner_scope, action_id)?;
        if !matches!(
            action.kind.as_str(),
            "select-file" | "select-directory" | "browser-authorization" | "browser-download"
        ) {
            return Err(BrokerError::InteractionMismatch(action.kind));
        }
        let rejection_hash = request_hash(&json!({
            "actionId": action_id,
            "expectedRevision": expected_revision,
            "reason": reason,
        }))?;
        self.jobs.reject_action(
            owner_scope,
            action_id,
            expected_revision,
            reason,
            idempotency_key,
            &rejection_hash,
        )?;
        Ok(())
    }

    fn complete_browser_callback(
        &self,
        owner_scope: &str,
        action_id: &str,
        expected_revision: u64,
        callback_url: &Url,
        idempotency_key: &str,
        expected_kind: &str,
    ) -> Result<(), BrokerError> {
        let action = self.jobs.get_action(owner_scope, action_id)?;
        if action.kind != expected_kind {
            return Err(BrokerError::InteractionMismatch(action.kind));
        }
        let target: BrowserTarget = serde_json::from_value(action.target.clone())?;
        let scheme_allowed = target
            .callback_schemes
            .iter()
            .any(|scheme| scheme == callback_url.scheme());
        let origin_allowed = Origin::from_url(callback_url)
            .map(|origin| {
                target
                    .callback_origins
                    .iter()
                    .any(|allowed| allowed == origin.as_str())
            })
            .unwrap_or(false);
        if !scheme_allowed && !origin_allowed {
            return Err(BrokerError::InteractionMismatch(
                "callback URL is outside the declared callback scope".to_owned(),
            ));
        }
        self.complete_with_grant(GrantCompletion {
            owner_scope,
            action_id,
            expected_revision,
            idempotency_key,
            kind: "browser-callback",
            expires_at_ms: target.expires_at_ms,
            resource: json!({ "callbackUrl": callback_url }),
        })
    }

    fn complete_with_grant(&self, completion: GrantCompletion<'_>) -> Result<(), BrokerError> {
        if completion.expires_at_ms <= now_ms()? {
            return Err(BrokerError::InteractionMismatch(
                "controlled interaction expired".to_owned(),
            ));
        }
        let grant_id = format!("grant:{}", Uuid::new_v4());
        {
            let mut connection = self.database.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "INSERT INTO broker_interaction_grants(
                   grant_id, action_id, owner_scope, kind, resource_json,
                   expires_at_ms, consumed_at_ms, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
                params![
                    grant_id,
                    completion.action_id,
                    completion.owner_scope,
                    completion.kind,
                    serde_json::to_string(&completion.resource)?,
                    completion.expires_at_ms,
                    now_ms()?
                ],
            )?;
            transaction.commit()?;
        }
        let response = serde_json::to_value(GrantResponse {
            grant_id: grant_id.clone(),
        })?;
        let approval_hash = request_hash(&json!({
            "actionId": completion.action_id,
            "expectedRevision": completion.expected_revision,
            "response": response,
        }))?;
        if let Err(error) = self.jobs.approve_action(
            completion.owner_scope,
            completion.action_id,
            completion.expected_revision,
            &response,
            completion.idempotency_key,
            &approval_hash,
        ) {
            let connection = self.database.connection()?;
            connection.execute(
                "DELETE FROM broker_interaction_grants WHERE grant_id = ?1",
                [&grant_id],
            )?;
            return Err(error.into());
        }
        Ok(())
    }
}

pub fn interaction_binding(action: &ActionRequired) -> ActionBinding {
    ActionBinding {
        target: action.target.clone(),
        target_revision: action.target_revision,
        plan_hash: action.plan_hash.clone(),
        module_lock_digest: action.module_lock_digest.clone(),
    }
}

fn grant_path(resource: &Value, grant_id: &str) -> Result<PathBuf, BrokerError> {
    resource
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| BrokerError::GrantUnavailable(grant_id.to_owned()))
}

fn open_regular_file(path: &Path) -> Result<File, BrokerError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BrokerError::InteractionMismatch(
            "selected input is not a regular file".to_owned(),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(BrokerError::InteractionMismatch(
            "selected input changed file type".to_owned(),
        ));
    }
    Ok(file)
}

fn snapshot_directory(
    root: &Path,
    destination: &Path,
    limits: InputSnapshotLimits,
) -> Result<DirectorySnapshot, BrokerError> {
    if limits.max_entries == 0
        || limits.max_file_bytes == 0
        || limits.max_total_bytes == 0
        || limits.max_depth == 0
    {
        return Err(BrokerError::InvalidRequest(
            "directory snapshot limits must be positive".to_owned(),
        ));
    }
    if destination.exists() {
        return Err(BrokerError::InvalidRequest(
            "directory snapshot destination already exists".to_owned(),
        ));
    }
    let parent = destination.parent().ok_or_else(|| {
        BrokerError::InvalidRequest("directory snapshot destination has no parent".to_owned())
    })?;
    let canonical_parent = parent.canonicalize()?;
    if canonical_parent.starts_with(root) {
        return Err(BrokerError::InvalidRequest(
            "directory snapshot destination cannot be inside the selected input".to_owned(),
        ));
    }
    fs::create_dir(destination)?;
    let outcome = snapshot_directory_inner(root, destination, limits);
    if outcome.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    outcome
}

fn snapshot_directory_inner(
    root: &Path,
    destination: &Path,
    limits: InputSnapshotLimits,
) -> Result<DirectorySnapshot, BrokerError> {
    let first = collect_directory_fingerprints(root, limits)?;
    let mut copied_digests = BTreeMap::new();
    for fingerprint in &first {
        let relative = checked_snapshot_relative(&fingerprint.relative_path)?;
        let source = root.join(&relative);
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let digest = copy_snapshot_file(&source, &target, fingerprint.size, limits.max_file_bytes)?;
        copied_digests.insert(fingerprint.relative_path.clone(), digest);
    }
    let second = collect_directory_fingerprints(root, limits)?;
    if first != second {
        return Err(BrokerError::InputChanged);
    }
    for fingerprint in &second {
        let relative = checked_snapshot_relative(&fingerprint.relative_path)?;
        let digest = hash_regular_file(&root.join(relative), limits.max_file_bytes)?;
        if copied_digests.get(&fingerprint.relative_path) != Some(&digest) {
            return Err(BrokerError::InputChanged);
        }
    }
    Ok(DirectorySnapshot {
        entries: first
            .iter()
            .map(|fingerprint| DirectorySnapshotEntry {
                relative_path: fingerprint.relative_path.clone(),
                size: fingerprint.size,
            })
            .collect(),
        total_bytes: first.iter().map(|fingerprint| fingerprint.size).sum(),
    })
}

fn collect_directory_fingerprints(
    root: &Path,
    limits: InputSnapshotLimits,
) -> Result<Vec<InputFingerprint>, BrokerError> {
    let mut fingerprints = Vec::new();
    let mut case_paths = BTreeMap::new();
    let mut entry_count = 0_u64;
    let mut total_bytes = 0_u64;
    collect_directory_level(
        root,
        root,
        0,
        limits,
        &mut entry_count,
        &mut total_bytes,
        &mut case_paths,
        &mut fingerprints,
    )?;
    fingerprints.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(fingerprints)
}

#[allow(clippy::too_many_arguments)]
fn collect_directory_level(
    root: &Path,
    directory: &Path,
    depth: u32,
    limits: InputSnapshotLimits,
    entry_count: &mut u64,
    total_bytes: &mut u64,
    case_paths: &mut BTreeMap<String, String>,
    fingerprints: &mut Vec<InputFingerprint>,
) -> Result<(), BrokerError> {
    if depth > limits.max_depth {
        return Err(BrokerError::InputLimitExceeded(
            "directory depth".to_owned(),
        ));
    }
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        *entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| BrokerError::InputLimitExceeded("entry count".to_owned()))?;
        if *entry_count > limits.max_entries {
            return Err(BrokerError::InputLimitExceeded("entry count".to_owned()));
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(BrokerError::InteractionMismatch(
                "selected directory contains a symbolic link".to_owned(),
            ));
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(root) {
            return Err(BrokerError::InteractionMismatch(
                "selected directory entry escapes its authorized root".to_owned(),
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| BrokerError::InteractionMismatch("invalid directory entry".to_owned()))?;
        let relative_path = snapshot_relative_string(relative)?;
        let case_key = relative_path.to_ascii_lowercase();
        if let Some(existing) = case_paths.insert(case_key, relative_path.clone()) {
            if existing != relative_path {
                return Err(BrokerError::InteractionMismatch(
                    "selected directory has a case-insensitive path collision".to_owned(),
                ));
            }
        }
        if metadata.is_dir() {
            collect_directory_level(
                root,
                &path,
                depth + 1,
                limits,
                entry_count,
                total_bytes,
                case_paths,
                fingerprints,
            )?;
        } else if metadata.is_file() {
            if metadata.len() > limits.max_file_bytes {
                return Err(BrokerError::InputLimitExceeded(
                    "single file size".to_owned(),
                ));
            }
            *total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| BrokerError::InputLimitExceeded("total bytes".to_owned()))?;
            if *total_bytes > limits.max_total_bytes {
                return Err(BrokerError::InputLimitExceeded("total bytes".to_owned()));
            }
            fingerprints.push(InputFingerprint {
                relative_path,
                size: metadata.len(),
                modified_ns: modified_ns(&metadata),
            });
        } else {
            return Err(BrokerError::InteractionMismatch(
                "selected directory contains a special file".to_owned(),
            ));
        }
    }
    Ok(())
}

fn snapshot_relative_string(path: &Path) -> Result<String, BrokerError> {
    let mut result = String::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(BrokerError::InteractionMismatch(
                "selected directory has a non-relative entry".to_owned(),
            ));
        };
        let value = value.to_str().ok_or_else(|| {
            BrokerError::InteractionMismatch(
                "selected directory path is not valid UTF-8".to_owned(),
            )
        })?;
        if value.is_empty() || value.contains([':', '\\']) {
            return Err(BrokerError::InteractionMismatch(
                "selected directory path is not portable".to_owned(),
            ));
        }
        if !result.is_empty() {
            result.push('/');
        }
        result.push_str(value);
    }
    Ok(result)
}

fn checked_snapshot_relative(value: &str) -> Result<PathBuf, BrokerError> {
    let path = Path::new(value);
    if value.is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BrokerError::InteractionMismatch(
            "snapshot entry is not a relative path".to_owned(),
        ));
    }
    Ok(path.to_path_buf())
}

fn copy_snapshot_file(
    source: &Path,
    destination: &Path,
    expected_size: u64,
    max_file_bytes: u64,
) -> Result<[u8; 32], BrokerError> {
    let mut input = open_regular_file(source)?;
    if input.metadata()?.len() != expected_size {
        return Err(BrokerError::InputChanged);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| BrokerError::InputLimitExceeded("single file size".to_owned()))?;
        if copied > max_file_bytes {
            return Err(BrokerError::InputLimitExceeded(
                "single file size".to_owned(),
            ));
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    if copied != expected_size || input.metadata()?.len() != expected_size {
        return Err(BrokerError::InputChanged);
    }
    output.sync_all()?;
    Ok(hasher.finalize().into())
}

fn hash_regular_file(path: &Path, max_file_bytes: u64) -> Result<[u8; 32], BrokerError> {
    let mut input = open_regular_file(path)?;
    let mut hasher = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(count as u64)
            .ok_or_else(|| BrokerError::InputLimitExceeded("single file size".to_owned()))?;
        if read_bytes > max_file_bytes {
            return Err(BrokerError::InputLimitExceeded(
                "single file size".to_owned(),
            ));
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}

fn modified_ns(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

fn ensure_browser_action_kind(kind: &str) -> Result<(), BrokerError> {
    matches!(kind, "browser-authorization" | "browser-download")
        .then_some(())
        .ok_or_else(|| BrokerError::InteractionMismatch(kind.to_owned()))
}

fn normalize_schemes(values: Vec<String>) -> Result<Vec<String>, BrokerError> {
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim().trim_end_matches(':').to_ascii_lowercase();
        let mut characters = value.chars();
        let valid = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && characters.all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
            });
        if !valid {
            return Err(BrokerError::InvalidRequest(
                "browser callback schemes must be valid URL schemes".to_owned(),
            ));
        }
        normalized.push(value);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_extensions(values: Vec<String>) -> Result<Vec<String>, BrokerError> {
    let mut normalized = values
        .into_iter()
        .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
        .collect::<Vec<_>>();
    if normalized.iter().any(String::is_empty) {
        return Err(BrokerError::InvalidRequest(
            "file extensions cannot be empty".to_owned(),
        ));
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn validate_file_extension(path: &Path, allowed: &[String]) -> Result<(), BrokerError> {
    if allowed.is_empty() {
        return Ok(());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            BrokerError::InteractionMismatch("selected file has no allowed extension".to_owned())
        })?;
    allowed.contains(&extension).then_some(()).ok_or_else(|| {
        BrokerError::InteractionMismatch(
            "selected file extension is outside the request".to_owned(),
        )
    })
}

fn grant_response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["grantId"],
        "properties": {
            "grantId": { "type": "string", "minLength": 1 }
        }
    })
}

fn validate_context(context: &InteractionContext) -> Result<(), BrokerError> {
    if context.operation_id.trim().is_empty()
        || context.plan_hash.trim().is_empty()
        || context.module_lock_digest.trim().is_empty()
        || context.expires_at_ms <= now_ms()?
    {
        return Err(BrokerError::InvalidRequest(
            "controlled interaction binding is incomplete or expired".to_owned(),
        ));
    }
    Ok(())
}

fn now_ms() -> Result<i64, BrokerError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| BrokerError::InvalidRequest(error.to_string()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| BrokerError::InvalidRequest("timestamp overflow".to_owned()))
}

pub fn interaction_plan_hash(value: &Value) -> Result<String, BrokerError> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value)?)
    ))
}
