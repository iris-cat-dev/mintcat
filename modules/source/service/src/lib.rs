mod builtin;
pub use builtin::register_builtin_sources;

use std::{collections::BTreeMap, sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use mintcat_feature_backup::{BackupError, MaintenanceGate};
use mintcat_runtime_kernel::RuntimeLayout;
use mintcat_library_service::{LibraryAuthorization, LibraryError, LibraryStore, RemoteModMetadataUpdate, SourceAddMetadata, SourceAddRequest, SourceAddResult, SourceAddTarget, SourceAddVersion};
use mintcat_runtime_broker::{CancellationToken, InteractionContext};
use mintcat_runtime_downloads::{DownloadCatalog, DownloadCoordinatorError};
use mintcat_runtime_jobs::{request_hash, CommitState, ConfirmationPolicy, EventUpdate, JobError, JobStore, RecoveryRecord, RegisterJob, ResumePolicy, WorkerCheckpoint};
use mintcat_sdk::{ActionRequired, AppError, DownloadMode, JobRef, JobState, ModDetails, ModRef, ModUpdateCheckItem, ModsCheckUpdatesResult, ModsGetParams, ModsListParams, ModsUpdateBatchParams};
use mintcat_source_api::{SearchRequest, SearchSortField, SortDirection, SourceError, SourceGameBinding, SourceMod, SourceVersion};
use mintcat_source_local::{LocalSource, LocalSourceError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum SourceApplicationError {
    #[error(transparent)] Library(#[from] LibraryError),
    #[error(transparent)] Catalog(#[from] DownloadCoordinatorError),
    #[error(transparent)] Source(#[from] SourceError),
    #[error(transparent)] Local(#[from] LocalSourceError),
    #[error(transparent)] Job(#[from] JobError),
    #[error(transparent)] Maintenance(#[from] BackupError),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error("source application request is invalid: {0}")] Invalid(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSearchRequest {
    pub source_id: String,
    pub game_id: String,
    pub remote_game_key: Option<String>,
    pub query: String,
    pub cursor: Option<String>,
    pub page_size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_field: Option<SearchSortField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_direction: Option<SortDirection>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSearchItem {
    pub display_name: String,
    pub reference: ModRef,
    #[serde(skip_serializing_if = "Option::is_none")] pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub downloads: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub subscribers: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub rating: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub profile_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub logo_url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSearchResult {
    pub items: Vec<SourceSearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LocalSelectionKind { File, Directory }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportLocalRequest {
    pub profile_id: String,
    pub remote_game_key: String,
    pub input_kind: LocalSelectionKind,
    pub expected_content_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalJobTarget { request: ImportLocalRequest, target: SourceAddTarget, module_lock_digest: String }
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceCommitCheckpoint {
    key: String,
    request: SourceAddRequest,
    local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    download: Option<ModsUpdateBatchParams>,
}

type SubmitDownloadResult = Result<Option<JobRef>, String>;

#[derive(Clone)]
pub struct SourceApplication {
    library: LibraryStore,
    authorization: LibraryAuthorization,
    jobs: JobStore,
    catalog: DownloadCatalog,
    local: LocalSource,
    local_games: Arc<BTreeMap<String, SourceGameBinding>>,
    module_lock_digest: String,
    layout: RuntimeLayout,
}
impl SourceApplication {
    pub fn new(library: LibraryStore, authorization: LibraryAuthorization, jobs: JobStore, download_catalog: DownloadCatalog, local: LocalSource, local_games: BTreeMap<String, SourceGameBinding>, module_lock_digest: String, layout: RuntimeLayout) -> Self {
        Self { library, authorization, jobs, catalog: download_catalog, local, local_games: Arc::new(local_games), module_lock_digest, layout }
    }
    pub async fn search(&self, request: &SourceSearchRequest, cancellation: &CancellationToken) -> Result<SourceSearchResult, SourceApplicationError> {
        if request.page_size == 0 || request.page_size > 100 { return Err(SourceApplicationError::Invalid("pageSize must be between 1 and 100".into())); }
        let (provider, binding, lock) = self.catalog.lookup(&request.source_id,&request.game_id,request.remote_game_key.as_deref())?;
        if lock != self.module_lock_digest { return Err(SourceApplicationError::Invalid("source provider belongs to another active module lock".into())); }
        let page = provider.search(&binding,&SearchRequest { query:request.query.clone(),cursor:request.cursor.clone(),page_size:request.page_size,sort_field:request.sort_field,sort_direction:request.sort_direction },cancellation).await?;
        let mut items = Vec::with_capacity(page.items.len());
        for item in page.items {
            validate_reference(&binding,&request.source_id,&item.mod_ref)?;
            items.push(to_search_item(item));
        }
        Ok(SourceSearchResult { items,next_cursor:page.next_cursor })
    }
    pub async fn add(
        &self,
        idempotency_key: &str,
        request: SourceAddRequest,
        submit_download: impl FnOnce(&str, &ModsUpdateBatchParams) -> SubmitDownloadResult + Send + 'static,
    ) -> Result<JobRef, SourceApplicationError> {
        let input = serde_json::to_value(&request)?;
        let hash = request_hash(&input)?;
        if self.jobs.find_operation(self.authorization.owner_scope(),"mods.add",idempotency_key)?.is_some() {
            return Ok(self.jobs.register_job(RegisterJob { command:"mods.add".into(), owner_scope:self.authorization.owner_scope().into(), idempotency_key:idempotency_key.into(), request_hash:hash, target:input, confirmation_policy:ConfirmationPolicy::None })?.job);
        }
        if request.reference.source_id == "local" { return Err(SourceApplicationError::Invalid("local content requires an explicit input grant through mods.importLocal".into())); }
        let admission = MaintenanceGate::enter_mutation(&self.layout)?;
        let target = self.library.source_add_target(&self.authorization,&request)?;
        let (_, _, lock) = self.catalog.lookup(&request.reference.source_id,&target.game_id,Some(&request.reference.remote_game_key))?;
        if lock != self.module_lock_digest { return Err(SourceApplicationError::Invalid("source provider belongs to a different active module lock".into())); }
        let registration = self.jobs.register_job(RegisterJob { command:"mods.add".into(),owner_scope:self.authorization.owner_scope().into(),idempotency_key:idempotency_key.into(),request_hash:hash,target:json!({"request":request,"target":target,"moduleLockDigest":self.module_lock_digest}),confirmation_policy:ConfirmationPolicy::None })?;
        if registration.created {
            let application = self.clone();
            let key = idempotency_key.to_owned();
            let job_id = registration.job.job_id.clone();
            tokio::spawn(async move {
                let _admission = admission;
                if let Err(error) = application.execute_add(&job_id, &key, &request, &target, submit_download).await { application.fail(&job_id,&error); }
            });
        }
        Ok(registration.job)
    }
    async fn execute_add(
        &self, job_id: &str, key: &str, request: &SourceAddRequest, target: &SourceAddTarget,
        submit_download: impl FnOnce(&str, &ModsUpdateBatchParams) -> SubmitDownloadResult,
    ) -> Result<(), SourceApplicationError> {
        self.start(job_id)?;
        let (provider,binding,lock) = self.catalog.lookup(&request.reference.source_id,&target.game_id,Some(&request.reference.remote_game_key))?;
        if lock != self.module_lock_digest { return Err(SourceApplicationError::Invalid("active source module lock changed".into())); }
        let cancellation = CancellationToken::default();
        let item = provider.get_mod(&binding,&request.reference,&cancellation).await?;
        validate_reference(&binding,&request.reference.source_id,&item.mod_ref)?;
        if item.mod_ref.remote_mod_id != request.reference.remote_mod_id { return Err(SourceError::InvalidResponse("source metadata returned a different mod".into()).into()); }
        let versions = provider.list_versions(&binding,&request.reference,&cancellation).await?;
        let selected = if let Some(version) = &request.reference.version_id {
            Some(versions.iter().find(|entry| &entry.version_id == version).ok_or_else(|| SourceError::NotFound("requested version".into()))?)
        } else if let Some(file) = &request.reference.remote_file_id {
            Some(versions.iter().find(|entry| &entry.remote_file_id == file).ok_or_else(|| SourceError::NotFound("requested file".into()))?)
        } else {
            item.latest_file
                .as_ref()
                .and_then(|latest| versions.iter().find(|version| version.version_id == latest.version_id))
                .or_else(|| latest_source_version(&versions))
        };
        if request.reference.remote_file_id.as_ref().is_some_and(|file| selected.is_none_or(|version| &version.remote_file_id != file)) { return Err(SourceError::InvalidResponse("requested source file/version mismatch".into()).into()); }
        let (tags, approval_status) = parse_source_tags_and_approval(&item.tags);
        let online_update_at_ms = item.updated_at.max(0).saturating_mul(1_000);
        let selected_version_id = selected.map(|version| version.version_id.clone());
        let metadata = SourceAddMetadata {
            display_name: item.name,
            profile_url: item.profile_url,
            tags,
            approval_status,
            online_update_at_ms,
            selected_version_id,
            versions: versions.into_iter().map(to_source_add_version).collect(),
        };
        self.checkpoint_commit(job_id, key, request, false)?;
        self.jobs.begin_commit(job_id)?;
        let mut result = self.library.commit_source_add(&self.authorization,key,request,target,&metadata)?;
        let mut profile_mod_ids = vec![result.profile_mod_id.clone()];
        if request.include_dependencies.unwrap_or(true) {
            const MAX_DEPENDENCIES: usize = 20;
            let mut visited = std::collections::HashSet::new();
            visited.insert((
                request.reference.source_id.clone(),
                request.reference.remote_game_key.clone(),
                request.reference.remote_mod_id.clone(),
            ));
            let mut queue = std::collections::VecDeque::new();
            queue.push_back(request.reference.clone());
            let mut added_deps = 0usize;
            while let Some(current_ref) = queue.pop_front() {
                if added_deps >= MAX_DEPENDENCIES {
                    break;
                }
                let Ok(dependencies) = provider.get_dependencies(&binding, &current_ref, &cancellation).await else {
                    continue;
                };
                for dependency in dependencies {
                    if !dependency.required || added_deps >= MAX_DEPENDENCIES {
                        continue;
                    }
                    if validate_reference(&binding, &request.reference.source_id, &dependency.mod_ref).is_err() {
                        continue;
                    }
                    let dep_key = (
                        dependency.mod_ref.source_id.clone(),
                        dependency.mod_ref.remote_game_key.clone(),
                        dependency.mod_ref.remote_mod_id.clone(),
                    );
                    if !visited.insert(dep_key) {
                        continue;
                    }
                    let Ok(dep_item) = provider.get_mod(&binding, &dependency.mod_ref, &cancellation).await else {
                        continue;
                    };
                    if validate_reference(&binding, &request.reference.source_id, &dep_item.mod_ref).is_err() {
                        continue;
                    }
                    let Ok(dep_versions) = provider.list_versions(&binding, &dependency.mod_ref, &cancellation).await else {
                        continue;
                    };
                    let dep_selected = dep_item
                        .latest_file
                        .as_ref()
                        .and_then(|latest| dep_versions.iter().find(|v| v.version_id == latest.version_id))
                        .or_else(|| latest_source_version(&dep_versions));
                    let (dep_tags, dep_approval) = parse_source_tags_and_approval(&dep_item.tags);
                    let dep_metadata = SourceAddMetadata {
                        display_name: dep_item.name,
                        profile_url: dep_item.profile_url,
                        tags: dep_tags,
                        approval_status: dep_approval,
                        online_update_at_ms: dep_item.updated_at.max(0).saturating_mul(1_000),
                        selected_version_id: dep_selected.map(|v| v.version_id.clone()),
                        versions: dep_versions.into_iter().map(to_source_add_version).collect(),
                    };
                    let dep_request = SourceAddRequest {
                        profile_id: request.profile_id.clone(),
                        reference: dep_item.mod_ref.clone(),
                        expected_content_revision: result.content_revision,
                        group_id: request.group_id.clone(),
                        include_dependencies: Some(false),
                    };
                    let Ok(dep_target) = self.library.source_add_target(&self.authorization, &dep_request) else {
                        continue;
                    };
                    let dep_idem = format!("{key}:dep:{}", dep_item.mod_ref.remote_mod_id);
                    if let Ok(dep_committed) = self.library.commit_source_add(
                        &self.authorization,
                        &dep_idem,
                        &dep_request,
                        &dep_target,
                        &dep_metadata,
                    ) {
                        profile_mod_ids.push(dep_committed.profile_mod_id);
                        result.content_revision = dep_committed.content_revision;
                        added_deps += 1;
                        queue.push_back(dep_item.mod_ref);
                    }
                }
            }
        }
        let download = ModsUpdateBatchParams {
            profile_id: result.profile_id.clone(),
            expected_content_revision: result.content_revision,
            profile_mod_ids: Some(profile_mod_ids),
            version_policy: None,
            mode: Some(DownloadMode::PrepareAndApply),
        };
        self.save_commit_checkpoint(job_id, SourceCommitCheckpoint {
            key: key.to_owned(), request: request.clone(), local: false, download: Some(download.clone()),
        })?;
        let result = Self::add_result(job_id, result, Some(&download), submit_download)?;
        self.jobs.finish_commit(job_id, result)?;
        Ok(())
    }
    pub async fn refresh_mod_versions(
        &self,
        profile_id: &str,
        profile_mod_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<ModDetails, SourceApplicationError> {
        let details = self.library.get_mod(
            &self.authorization,
            &ModsGetParams {
                profile_id: profile_id.to_owned(),
                profile_mod_id: profile_mod_id.to_owned(),
            },
        )?;
        let summary = &details.summary;
        if summary.r#mod.source_id == "local" {
            return Ok(details);
        }
        let profile = self
            .library
            .snapshot()?
            .profiles
            .into_iter()
            .find(|item| item.profile_id == profile_id && item.deleted_at_ms.is_none())
            .ok_or_else(|| LibraryError::NotFound(profile_id.to_owned()))?;
        let (provider, binding, lock) = self.catalog.lookup(
            &summary.r#mod.source_id,
            &profile.game_id,
            Some(&summary.r#mod.remote_game_key),
        )?;
        if lock != self.module_lock_digest {
            return Err(SourceApplicationError::Invalid(
                "active source module lock changed".into(),
            ));
        }
        let item = provider.get_mod(&binding, &summary.r#mod, cancellation).await?;
        validate_reference(&binding, &summary.r#mod.source_id, &item.mod_ref)?;
        let versions = provider.list_versions(&binding, &summary.r#mod, cancellation).await?;
        let latest_id = item
            .latest_file
            .as_ref()
            .and_then(|latest| versions.iter().find(|v| v.version_id == latest.version_id))
            .or_else(|| latest_source_version(&versions))
            .map(|v| v.version_id.clone());
        let (tags, approval_status) = parse_source_tags_and_approval(&item.tags);
        self.library.sync_remote_mod_metadata(
            &self.authorization,
            &RemoteModMetadataUpdate {
                mod_id: summary.mod_id.clone(),
                profile_url: item.profile_url,
                tags: Some(tags),
                approval_status: Some(approval_status),
                online_update_at_ms: Some(item.updated_at.max(0).saturating_mul(1_000)),
                online_available: true,
                versions: versions.into_iter().map(to_source_add_version).collect(),
                current_version_id: latest_id,
            },
        )?;
        Ok(self.library.get_mod(
            &self.authorization,
            &ModsGetParams {
                profile_id: profile_id.to_owned(),
                profile_mod_id: profile_mod_id.to_owned(),
            },
        )?)
    }

    pub async fn check_updates(
        &self,
        profile_id: &str,
        profile_mod_ids: Option<&[String]>,
        cancellation: &CancellationToken,
    ) -> Result<ModsCheckUpdatesResult, SourceApplicationError> {
        let profile = self
            .library
            .snapshot()?
            .profiles
            .into_iter()
            .find(|item| item.profile_id == profile_id && item.deleted_at_ms.is_none())
            .ok_or_else(|| LibraryError::NotFound(profile_id.to_owned()))?;
        let mut cursor = None;
        let mut mods = Vec::new();
        loop {
            let page = self.library.list_mods(
                &self.authorization,
                &ModsListParams {
                    profile_id: profile_id.to_owned(),
                    page_size: 100,
                    cursor: cursor.clone(),
                    enabled: None,
                    group_id: None,
                    query: None,
                    source_id: None,
                },
            )?;
            mods.extend(page.items);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let filter: Option<std::collections::HashSet<&str>> = profile_mod_ids
            .filter(|ids| !ids.is_empty())
            .map(|ids| ids.iter().map(String::as_str).collect());
        let mut items = Vec::new();
        let mut checked_count: u64 = 0;
        let mut updated_count: u64 = 0;
        let mut unavailable_count: u64 = 0;

        for mod_summary in mods {
            if mod_summary.r#mod.source_id == "local" {
                continue;
            }
            if let Some(allowed) = &filter {
                if !allowed.contains(mod_summary.profile_mod_id.as_str()) {
                    continue;
                }
            }
            checked_count += 1;
            let lookup = self.catalog.lookup(
                &mod_summary.r#mod.source_id,
                &profile.game_id,
                Some(&mod_summary.r#mod.remote_game_key),
            );
            let Ok((provider, binding, lock)) = lookup else {
                items.push(ModUpdateCheckItem {
                    profile_mod_id: mod_summary.profile_mod_id,
                    mod_id: mod_summary.mod_id,
                    online_available: mod_summary.online_available.unwrap_or(true),
                    has_update: false,
                    latest_version_id: mod_summary.current_version_id,
                    latest_display_version: mod_summary.current_display_version,
                    applied_version_id: mod_summary.applied_version_id,
                    error_code: Some("SOURCE_UNAVAILABLE".to_owned()),
                });
                continue;
            };
            if lock != self.module_lock_digest {
                return Err(SourceApplicationError::Invalid(
                    "active source module lock changed".into(),
                ));
            }
            match provider.get_mod(&binding, &mod_summary.r#mod, cancellation).await {
                Ok(remote_mod) => {
                    validate_reference(&binding, &mod_summary.r#mod.source_id, &remote_mod.mod_ref)?;
                    let versions = match provider.list_versions(&binding, &mod_summary.r#mod, cancellation).await {
                        Ok(list) => list,
                        Err(error) => {
                            items.push(ModUpdateCheckItem {
                                profile_mod_id: mod_summary.profile_mod_id,
                                mod_id: mod_summary.mod_id,
                                online_available: true,
                                has_update: false,
                                latest_version_id: mod_summary.current_version_id,
                                latest_display_version: mod_summary.current_display_version,
                                applied_version_id: mod_summary.applied_version_id,
                                error_code: Some(error.app_code().to_owned()),
                            });
                            continue;
                        }
                    };
                    let latest = remote_mod
                        .latest_file
                        .as_ref()
                        .and_then(|latest| versions.iter().find(|v| v.version_id == latest.version_id))
                        .or_else(|| latest_source_version(&versions))
                        .cloned();
                    let latest_version_id = latest.as_ref().map(|v| v.version_id.clone());
                    let latest_display_version = latest
                        .as_ref()
                        .map(|v| format_display_version(v))
                        .filter(|value| !value.is_empty());
                    let (tags, approval_status) = parse_source_tags_and_approval(&remote_mod.tags);
                    self.library.sync_remote_mod_metadata(
                        &self.authorization,
                        &RemoteModMetadataUpdate {
                            mod_id: mod_summary.mod_id.clone(),
                            profile_url: remote_mod.profile_url,
                            tags: Some(tags),
                            approval_status: Some(approval_status),
                            online_update_at_ms: Some(remote_mod.updated_at.max(0).saturating_mul(1_000)),
                            online_available: true,
                            versions: versions.into_iter().map(to_source_add_version).collect(),
                            current_version_id: latest_version_id.clone(),
                        },
                    )?;
                    let has_update = latest_version_id.as_ref().is_some_and(|latest_id| {
                        mod_summary
                            .applied_version_id
                            .as_ref()
                            .is_some_and(|applied| applied != latest_id)
                            || (mod_summary.applied_version_id.is_none()
                                && mod_summary
                                    .current_version_id
                                    .as_ref()
                                    .is_some_and(|current| current != latest_id))
                    });
                    if has_update {
                        updated_count += 1;
                    }
                    items.push(ModUpdateCheckItem {
                        profile_mod_id: mod_summary.profile_mod_id,
                        mod_id: mod_summary.mod_id,
                        online_available: true,
                        has_update,
                        latest_version_id,
                        latest_display_version,
                        applied_version_id: mod_summary.applied_version_id,
                        error_code: None,
                    });
                }
                Err(error @ (SourceError::NotFound(_) | SourceError::Unavailable(_))) => {
                    let _ = self.library.sync_remote_mod_metadata(
                        &self.authorization,
                        &RemoteModMetadataUpdate {
                            mod_id: mod_summary.mod_id.clone(),
                            profile_url: None,
                            tags: None,
                            approval_status: None,
                            online_update_at_ms: None,
                            online_available: false,
                            versions: Vec::new(),
                            current_version_id: None,
                        },
                    );
                    unavailable_count += 1;
                    items.push(ModUpdateCheckItem {
                        profile_mod_id: mod_summary.profile_mod_id,
                        mod_id: mod_summary.mod_id,
                        online_available: false,
                        has_update: false,
                        latest_version_id: mod_summary.current_version_id,
                        latest_display_version: mod_summary.current_display_version,
                        applied_version_id: mod_summary.applied_version_id,
                        error_code: Some(error.app_code().to_owned()),
                    });
                }
                Err(error) => {
                    items.push(ModUpdateCheckItem {
                        profile_mod_id: mod_summary.profile_mod_id,
                        mod_id: mod_summary.mod_id,
                        online_available: mod_summary.online_available.unwrap_or(true),
                        has_update: false,
                        latest_version_id: mod_summary.current_version_id,
                        latest_display_version: mod_summary.current_display_version,
                        applied_version_id: mod_summary.applied_version_id,
                        error_code: Some(error.app_code().to_owned()),
                    });
                }
            }
        }
        Ok(ModsCheckUpdatesResult {
            profile_id: profile_id.to_owned(),
            checked_count,
            updated_count,
            unavailable_count,
            items,
        })
    }

    pub async fn resolve_url(
        &self,
        game_id: &str,
        raw_url: &str,
        cancellation: &CancellationToken,
    ) -> Result<SourceSearchItem, SourceApplicationError> {
        let trimmed = raw_url.trim();
        let parsed = Url::parse(trimmed)
            .map_err(|_| SourceApplicationError::Invalid("invalid mod URL".into()))?;
        if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(SourceApplicationError::Invalid(
                "only credential-free HTTPS mod URLs are supported".into(),
            ));
        }
        let providers = self.catalog.providers_for_game(game_id)?;
        for (provider, binding, lock) in providers {
            if lock != self.module_lock_digest {
                continue;
            }
            if let Some(mod_ref) = provider.parse_url(&binding, &parsed)? {
                let item = provider.get_mod(&binding, &mod_ref, cancellation).await?;
                validate_reference(&binding, provider.source_id(), &item.mod_ref)?;
                return Ok(to_search_item(item));
            }
        }
        Err(SourceApplicationError::Source(SourceError::NotFound(
            "no configured source matches this URL for the active game".into(),
        )))
    }

    pub async fn list_subscribed(
        &self,
        source_id: &str,
        game_id: &str,
        remote_game_key: Option<&str>,
        cursor: Option<&str>,
        page_size: u32,
        cancellation: &CancellationToken,
    ) -> Result<SourceSearchResult, SourceApplicationError> {
        if page_size == 0 || page_size > 100 {
            return Err(SourceApplicationError::Invalid(
                "pageSize must be between 1 and 100".into(),
            ));
        }
        let (provider, binding, lock) = self.catalog.lookup(source_id, game_id, remote_game_key)?;
        if lock != self.module_lock_digest {
            return Err(SourceApplicationError::Invalid(
                "source provider belongs to another active module lock".into(),
            ));
        }
        let page = provider
            .list_subscribed(&binding, cursor, page_size, cancellation)
            .await?;
        let mut items = Vec::with_capacity(page.items.len());
        for item in page.items {
            validate_reference(&binding, source_id, &item.mod_ref)?;
            items.push(to_search_item(item));
        }
        Ok(SourceSearchResult {
            items,
            next_cursor: page.next_cursor,
        })
    }
    pub fn import_local(&self, key: &str, request: ImportLocalRequest) -> Result<JobRef, SourceApplicationError> {
        let input = serde_json::to_value(&request)?;
        let hash = request_hash(&input)?;
        if self.jobs.find_operation(self.authorization.owner_scope(),"mods.importLocal",key)?.is_some() {
            return Ok(self.jobs.register_job(RegisterJob { command:"mods.importLocal".into(), owner_scope:self.authorization.owner_scope().into(), idempotency_key:key.into(), request_hash:hash, target:input, confirmation_policy:ConfirmationPolicy::Required })?.job);
        }
        let _admission = MaintenanceGate::enter_mutation(&self.layout)?;
        let source_request = SourceAddRequest { profile_id:request.profile_id.clone(),reference:ModRef { source_id:"local".into(),remote_game_key:request.remote_game_key.clone(),remote_mod_id:"selection-pending".into(),version_id:None,remote_file_id:None },expected_content_revision:request.expected_content_revision, group_id: request.group_id.clone(), include_dependencies: None };
        let target = self.library.source_add_target(&self.authorization,&source_request)?;
        self.local_games.get(&target.game_id)
            .filter(|binding| binding.remote_game_key == request.remote_game_key)
            .ok_or_else(|| SourceApplicationError::Invalid("game has no matching local input binding".into()))?;
        let registration = self.jobs.register_job(RegisterJob { command:"mods.importLocal".into(),owner_scope:self.authorization.owner_scope().into(),idempotency_key:key.into(),request_hash:hash.clone(),target:json!({"request":&request,"target":&target,"moduleLockDigest":&self.module_lock_digest}),confirmation_policy:ConfirmationPolicy::Required })?;
        if registration.created {
            self.start(&registration.job.job_id)?;
            let context = InteractionContext { operation_id:registration.job.job_id.clone(),target_revision:request.expected_content_revision,plan_hash:hash,module_lock_digest:self.module_lock_digest.clone(),expires_at_ms:now_ms()+15*60*1000 };
            let action = match request.input_kind {
                LocalSelectionKind::File => self.local.request_file_input(&registration.job.job_id,&context,"Import local mod file"),
                LocalSelectionKind::Directory => self.local.request_directory_input(&registration.job.job_id,&context,"Import local mod directory"),
            };
            if let Err(error) = action {
                let error = SourceApplicationError::Local(error);
                self.fail(&registration.job.job_id,&error);
                return Err(error);
            }
        }
        Ok(registration.job)
    }
    pub fn continue_local_import(&self, job_id: &str, action: &ActionRequired) -> Result<SourceAddResult, SourceApplicationError> {
        let result = self.continue_local_import_inner(job_id, action);
        if let Err(error) = &result { self.fail(job_id, error); }
        result
    }

    fn continue_local_import_inner(&self, job_id: &str, action: &ActionRequired) -> Result<SourceAddResult, SourceApplicationError> {
        let stored: LocalJobTarget = serde_json::from_value(self.jobs.get_job_target(self.authorization.owner_scope(),job_id)?)?;
        if action.operation_id != job_id || action.module_lock_digest != stored.module_lock_digest || stored.module_lock_digest != self.module_lock_digest || action.target_revision != stored.request.expected_content_revision { return Err(SourceApplicationError::Invalid("local input grant is not bound to this job and active module lock".into())); }
        let snapshot = self.jobs.get_job(self.authorization.owner_scope(),job_id)?;
        if snapshot.state == JobState::Succeeded {
            return Ok(serde_json::from_value(snapshot.result.ok_or_else(|| SourceApplicationError::Invalid("completed import is missing its result".into()))?)?);
        }
        // Selection grants do not hold a mutation lock while awaiting a human;
        // snapshot creation and owner commit share this one admission instead.
        let _admission = MaintenanceGate::enter_mutation(&self.layout)?;
        let imported = match stored.request.input_kind { LocalSelectionKind::File => self.local.import_file(action,&stored.request.remote_game_key)?,LocalSelectionKind::Directory => self.local.import_directory(action,&stored.request.remote_game_key)? };
        let version_id = imported.artifact.digest.clone();
        let metadata = SourceAddMetadata { display_name:imported.display_name,profile_url:None,tags:Vec::new(),approval_status:String::new(),online_update_at_ms:0,versions:vec![SourceAddVersion { version_id:version_id.clone(),remote_file_id:None,file_size:imported.artifact.size,display_version:String::new() }],selected_version_id:Some(version_id) };
        let request = SourceAddRequest { profile_id:stored.request.profile_id,reference:imported.mod_ref,expected_content_revision:stored.request.expected_content_revision, group_id: stored.request.group_id, include_dependencies: None };
        self.checkpoint_commit(job_id, job_id, &request, true)?;
        self.jobs.begin_commit(job_id)?;
        let result = self.library.commit_local_source_add(&self.authorization,job_id,&request,&stored.target,&metadata,&imported.artifact,self.jobs.generation_id())?;
        self.jobs.finish_commit(job_id,serde_json::to_value(&result)?)?;
        Ok(result)
    }
    fn checkpoint_commit(&self, job_id:&str, key:&str, request:&SourceAddRequest, local:bool)->Result<(),SourceApplicationError> {
        self.save_commit_checkpoint(job_id, SourceCommitCheckpoint {
            key: key.to_owned(), request: request.clone(), local, download: None,
        })
    }

    fn save_commit_checkpoint(&self, job_id: &str, checkpoint: SourceCommitCheckpoint) -> Result<(), SourceApplicationError> {
        let snapshot=self.jobs.get_job(self.authorization.owner_scope(),job_id)?;
        self.jobs.save_checkpoint(job_id,snapshot.last_seq,&WorkerCheckpoint {
            step:"source-commit".into(), resume_policy:ResumePolicy::InspectBeforeResume,
            payload: serde_json::to_value(checkpoint)?,
        })?;
        Ok(())
    }

    fn add_result(
        job_id: &str,
        result: SourceAddResult,
        download: Option<&ModsUpdateBatchParams>,
        submit_download: impl FnOnce(&str, &ModsUpdateBatchParams) -> SubmitDownloadResult,
    ) -> Result<serde_json::Value, SourceApplicationError> {
        let mut value = serde_json::to_value(result)?;
        if let Some(request) = download {
            match submit_download(job_id, request) {
                Ok(Some(download)) => value["downloadJobId"] = json!(download.job_id),
                Ok(None) => {}
                Err(code) => value["downloadErrorCode"] = json!(code),
            }
        }
        Ok(value)
    }

    /// Inspect the Library-owned transaction journal, never replay bytes or
    /// infer rollback from a crashed/lost native owner reply.
    pub fn recover(
        &self, record: &RecoveryRecord,
        submit_download: impl FnOnce(&str, &ModsUpdateBatchParams) -> SubmitDownloadResult,
    ) -> Result<bool, SourceApplicationError> {
        if !matches!(record.snapshot.command.as_str(),"mods.add"|"mods.importLocal") {return Ok(false);}
        let _admission=MaintenanceGate::enter_mutation(&self.layout)?;
        let job_id=&record.snapshot.job_id;
        let state=self.jobs.commit_state(self.authorization.owner_scope(),job_id)?;
        if record.disposition == mintcat_runtime_jobs::RecoveryDisposition::AwaitingAction {
            let action = self.jobs.get_job_action(self.authorization.owner_scope(), job_id)?
                .ok_or_else(|| SourceApplicationError::Invalid("source action recovery lacks its durable action".into()))?;
            if action.module_lock_digest != self.module_lock_digest {
                return Err(SourceApplicationError::Invalid("source action requires its retained exact module generation".into()));
            }
            match action.state {
                mintcat_sdk::ActionState::Pending => return Ok(true),
                mintcat_sdk::ActionState::Approved => {
                    self.continue_local_import(job_id, &action)?;
                    return Ok(true);
                }
                _ => {}
            }
        }
        if let Some(checkpoint)=record.checkpoint.as_ref().filter(|checkpoint|checkpoint.step=="source-commit") {
            let checkpoint:SourceCommitCheckpoint=serde_json::from_value(checkpoint.payload.clone())?;
            if let Some(result)=self.library.source_add_result(&self.authorization,&checkpoint.key,&checkpoint.request,checkpoint.local)? {
                let result = Self::add_result(job_id, result, checkpoint.download.as_ref(), submit_download)?;
                if state==CommitState::Unknown {
                    self.jobs.finish_recovered_commit(job_id, result)?;
                } else { self.jobs.finish_commit(job_id, result)?; }
                return Ok(true);
            }
        } else if matches!(state,CommitState::Unknown|CommitState::InProgress) {
            return Err(SourceApplicationError::Invalid("source commit recovery lacks its durable owner-journal identity".into()));
        }
        let error=AppError {code:"SOURCE_INTERRUPTED".into(),message_key:"source.operation.interrupted".into(),details:json!({"commitProvenAbsent":true}),request_id:job_id.clone(),job_id:Some(job_id.clone()),retryable:false,recovery_actions:vec![]};
        if matches!(state,CommitState::Unknown|CommitState::InProgress) {
            self.jobs.finish_uncommitted(job_id,Some(error))?;
        } else {
            let mut update=EventUpdate::state("source-interrupted",JobState::Failed);update.error=Some(error);
            self.jobs.append_event(job_id,update)?;
        }
        Ok(true)
    }
    fn start(&self, job_id: &str) -> Result<(), JobError> {
        self.jobs.append_event(job_id,EventUpdate::state("source-prepare",JobState::Preparing))?;
        self.jobs.append_event(job_id,EventUpdate::state("source-fetch",JobState::Running))?;
        Ok(())
    }
    fn fail(&self, job_id: &str, error: &SourceApplicationError) {
        let (code,reason) = match error {
            SourceApplicationError::Source(error) => (error.app_code(),error.diagnostic_reason()),
            SourceApplicationError::Invalid(_) => ("INVALID_ARGUMENT","source_configuration"),
            _ => ("SOURCE_OPERATION_FAILED","source_operation"),
        };
        let app_error = AppError { code:code.into(),message_key:"source.operation.failed".into(),details:json!({"reason":reason}),request_id:job_id.into(),job_id:Some(job_id.into()),retryable:false,recovery_actions:Vec::new() };
        if self.jobs.commit_state(self.authorization.owner_scope(),job_id).is_ok_and(|state| matches!(state,mintcat_runtime_jobs::CommitState::InProgress)) {
            if matches!(error, SourceApplicationError::Library(
                LibraryError::PermissionDenied(_) | LibraryError::RevisionConflict { .. }
                | LibraryError::ResourceBusy { .. } | LibraryError::InvalidRecord(_)
                | LibraryError::NotFound(_) | LibraryError::IdempotencyConflict
                | LibraryError::StaleTarget(_) | LibraryError::ArtifactNotVerified(_)
            )) {
                let _ = self.jobs.finish_uncommitted(job_id,Some(app_error));
            } else {
                // A lost native-owner reply may follow a committed transaction.
                let _ = self.jobs.mark_commit_unknown(job_id);
            }
        } else { let mut update = EventUpdate::state("source-failed",JobState::Failed);update.error=Some(app_error);let _=self.jobs.append_event(job_id,update); }
    }
}
fn validate_reference(binding: &SourceGameBinding, source_id: &str, reference: &ModRef) -> Result<(),SourceError> {
    if reference.source_id != source_id || reference.remote_game_key != binding.remote_game_key || reference.remote_mod_id.trim().is_empty() { return Err(SourceError::InvalidResponse("source returned a reference outside the signed game binding".into())); }
    Ok(())
}
fn parse_source_tags_and_approval(raw_tags: &[String]) -> (Vec<String>, String) {
    let mut tags = Vec::with_capacity(raw_tags.len());
    let mut approval = String::new();
    for raw in raw_tags {
        let tag = raw.trim();
        if tag.is_empty() {
            continue;
        }
        match tag {
            "Verified" | "Auto-Verified" => approval = "Verified".to_owned(),
            "Approved" => approval = "Approved".to_owned(),
            "Sandbox" => approval = "Sandbox".to_owned(),
            _ => tags.push(tag.to_owned()),
        }
    }
    (tags, approval)
}
fn latest_source_version(versions: &[SourceVersion]) -> Option<&SourceVersion> {
    versions
        .iter()
        .max_by(|left, right| {
            left.published_at
                .cmp(&right.published_at)
                .then_with(|| left.version_id.cmp(&right.version_id))
        })
}
fn format_display_version(version: &SourceVersion) -> String {
    let display = version.display_version.trim();
    if !display.is_empty() {
        return display.to_owned();
    }
    let filename = version.filename.trim();
    if !filename.is_empty() {
        return filename.to_owned();
    }
    String::new()
}
fn to_source_add_version(version: SourceVersion) -> SourceAddVersion {
    let display_version = format_display_version(&version);
    SourceAddVersion {
        version_id: version.version_id,
        remote_file_id: Some(version.remote_file_id),
        file_size: version.file_size,
        display_version,
    }
}
fn to_search_item(item: SourceMod) -> SourceSearchItem {
    SourceSearchItem {
        display_name: item.name,
        reference: item.mod_ref,
        summary: (!item.summary.is_empty()).then_some(item.summary),
        author: (!item.author.trim().is_empty()).then_some(item.author),
        downloads: (item.downloads > 0).then_some(item.downloads),
        subscribers: (item.subscribers > 0).then_some(item.subscribers),
        rating: (item.rating.is_finite() && item.rating > 0.0).then_some(item.rating),
        updated_at: (item.updated_at > 0).then_some(item.updated_at),
        profile_url: item.profile_url,
        logo_url: item.logo_url,
    }
}
fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|time|time.as_millis() as i64).unwrap_or(0) }
