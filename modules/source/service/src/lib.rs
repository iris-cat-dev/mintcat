mod local_binding;
pub use local_binding::LocalInputProvider;
mod network_host;
pub use network_host::{SourceNetworkHost, SourceNetworkPolicy};
mod native;
pub use native::{NativeSourceProvider, SourcePackageBinding};

use std::time::{SystemTime, UNIX_EPOCH};
use mintcat_feature_backup::{BackupError, MaintenanceGate};
use mintcat_runtime_kernel::RuntimeLayout;
use mintcat_library_service::{LibraryAuthorization, LibraryError, LibraryStore, SourceAddMetadata, SourceAddRequest, SourceAddResult, SourceAddTarget, SourceAddVersion};
use mintcat_runtime_broker::{CancellationToken, InteractionContext};
use mintcat_runtime_downloads::{DownloadCatalog, DownloadCoordinatorError};
use mintcat_runtime_jobs::{request_hash, CommitState, ConfirmationPolicy, EventUpdate, JobError, JobStore, RecoveryRecord, RegisterJob, ResumePolicy, WorkerCheckpoint};
use mintcat_sdk::{ActionRequired, AppError, JobRef, JobState, ModRef};
use mintcat_source_api::{SearchRequest, SourceError, SourceGameBinding};
use mintcat_source_local::{LocalSource, LocalSourceError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSearchItem {
    pub display_name: String,
    pub reference: ModRef,
    #[serde(skip_serializing_if = "Option::is_none")] pub summary: Option<String>,
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
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalJobTarget { request: ImportLocalRequest, target: SourceAddTarget, module_lock_digest: String }
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceCommitCheckpoint { key: String, request: SourceAddRequest, local: bool }

#[derive(Clone)]
pub struct SourceApplication {
    library: LibraryStore,
    authorization: LibraryAuthorization,
    jobs: JobStore,
    catalog: DownloadCatalog,
    local: LocalSource,
    module_lock_digest: String,
    layout: RuntimeLayout,
}
impl SourceApplication {
    pub fn new(library: LibraryStore, authorization: LibraryAuthorization, jobs: JobStore, download_catalog: DownloadCatalog, local: LocalSource, module_lock_digest: String, layout: RuntimeLayout) -> Self {
        Self { library, authorization, jobs, catalog: download_catalog, local, module_lock_digest, layout }
    }
    pub async fn search(&self, request: &SourceSearchRequest, cancellation: &CancellationToken) -> Result<SourceSearchResult, SourceApplicationError> {
        if request.page_size == 0 || request.page_size > 100 { return Err(SourceApplicationError::Invalid("pageSize must be between 1 and 100".into())); }
        let (provider, binding, lock) = self.catalog.lookup(&request.source_id,&request.game_id,request.remote_game_key.as_deref())?;
        if lock != self.module_lock_digest { return Err(SourceApplicationError::Invalid("source provider belongs to another active module lock".into())); }
        let page = provider.search(&binding,&SearchRequest { query:request.query.clone(),cursor:request.cursor.clone(),page_size:request.page_size,sort_field:None,sort_direction:None },cancellation).await?;
        let mut items = Vec::with_capacity(page.items.len());
        for item in page.items {
            validate_reference(&binding,&request.source_id,&item.mod_ref)?;
            items.push(SourceSearchItem { display_name:item.name,reference:item.mod_ref,summary:(!item.summary.is_empty()).then_some(item.summary),profile_url:item.profile_url,logo_url:item.logo_url });
        }
        Ok(SourceSearchResult { items,next_cursor:page.next_cursor })
    }
    pub async fn add(&self, idempotency_key: &str, request: SourceAddRequest) -> Result<JobRef, SourceApplicationError> {
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
                if let Err(error) = application.execute_add(&job_id,&key,&request,&target).await { application.fail(&job_id,&error); }
            });
        }
        Ok(registration.job)
    }
    async fn execute_add(&self, job_id: &str, key: &str, request: &SourceAddRequest, target: &SourceAddTarget) -> Result<(), SourceApplicationError> {
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
        } else { item.latest_file.as_ref().and_then(|latest| versions.iter().find(|version|version.version_id == latest.version_id)) };
        if request.reference.remote_file_id.as_ref().is_some_and(|file| selected.is_none_or(|version| &version.remote_file_id != file)) { return Err(SourceError::InvalidResponse("requested source file/version mismatch".into()).into()); }
        let metadata = SourceAddMetadata { display_name:item.name,profile_url:item.profile_url,tags:item.tags,selected_version_id:selected.map(|version|version.version_id.clone()),versions:versions.into_iter().map(|version|SourceAddVersion { version_id:version.version_id,remote_file_id:Some(version.remote_file_id),file_size:version.file_size }).collect() };
        self.checkpoint_commit(job_id, key, request, false)?;
        self.jobs.begin_commit(job_id)?;
        let result = self.library.commit_source_add(&self.authorization,key,request,target,&metadata)?;
        self.jobs.finish_commit(job_id,serde_json::to_value(result)?)?;
        Ok(())
    }
    pub fn import_local(&self, key: &str, request: ImportLocalRequest) -> Result<JobRef, SourceApplicationError> {
        let input = serde_json::to_value(&request)?;
        let hash = request_hash(&input)?;
        if self.jobs.find_operation(self.authorization.owner_scope(),"mods.importLocal",key)?.is_some() {
            return Ok(self.jobs.register_job(RegisterJob { command:"mods.importLocal".into(), owner_scope:self.authorization.owner_scope().into(), idempotency_key:key.into(), request_hash:hash, target:input, confirmation_policy:ConfirmationPolicy::Required })?.job);
        }
        let _admission = MaintenanceGate::enter_mutation(&self.layout)?;
        let source_request = SourceAddRequest { profile_id:request.profile_id.clone(),reference:ModRef { source_id:"local".into(),remote_game_key:request.remote_game_key.clone(),remote_mod_id:"selection-pending".into(),version_id:None,remote_file_id:None },expected_content_revision:request.expected_content_revision };
        let target = self.library.source_add_target(&self.authorization,&source_request)?;
        // Local must also have an exact signed game descriptor, never a guessed slug.
        let (_, _, lock) = self.catalog.lookup("local",&target.game_id,Some(&request.remote_game_key))?;
        if lock != self.module_lock_digest { return Err(SourceApplicationError::Invalid("local source module lock differs".into())); }
        let registration = self.jobs.register_job(RegisterJob { command:"mods.importLocal".into(),owner_scope:self.authorization.owner_scope().into(),idempotency_key:key.into(),request_hash:hash.clone(),target:json!({"request":&request,"target":&target,"moduleLockDigest":&lock}),confirmation_policy:ConfirmationPolicy::Required })?;
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
        let metadata = SourceAddMetadata { display_name:imported.display_name,profile_url:None,tags:Vec::new(),versions:vec![SourceAddVersion { version_id:version_id.clone(),remote_file_id:None,file_size:imported.artifact.size }],selected_version_id:Some(version_id) };
        let request = SourceAddRequest { profile_id:stored.request.profile_id,reference:imported.mod_ref,expected_content_revision:stored.request.expected_content_revision };
        self.checkpoint_commit(job_id, job_id, &request, true)?;
        self.jobs.begin_commit(job_id)?;
        let result = self.library.commit_local_source_add(&self.authorization,job_id,&request,&stored.target,&metadata,&imported.artifact,self.jobs.generation_id())?;
        self.jobs.finish_commit(job_id,serde_json::to_value(&result)?)?;
        Ok(result)
    }
    fn checkpoint_commit(&self, job_id:&str, key:&str, request:&SourceAddRequest, local:bool)->Result<(),SourceApplicationError> {
        let snapshot=self.jobs.get_job(self.authorization.owner_scope(),job_id)?;
        self.jobs.save_checkpoint(job_id,snapshot.last_seq,&WorkerCheckpoint {
            step:"source-commit".into(), resume_policy:ResumePolicy::InspectBeforeResume,
            payload:json!({"key":key,"request":request,"local":local}),
        })?;
        Ok(())
    }

    /// Inspect the Library-owned transaction journal, never replay bytes or
    /// infer rollback from a crashed/lost native owner reply.
    pub fn recover(&self, record:&RecoveryRecord)->Result<bool,SourceApplicationError> {
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
                if state==CommitState::Unknown {
                    self.jobs.finish_recovered_commit(job_id,serde_json::to_value(result)?)?;
                } else { self.jobs.finish_commit(job_id,serde_json::to_value(result)?)?; }
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
        let app_error = AppError { code:match error { SourceApplicationError::Source(SourceError::Unauthorized)=>"SOURCE_UNAUTHORIZED",SourceApplicationError::Source(SourceError::Unavailable(_))=>"SOURCE_UNAVAILABLE",_=>"SOURCE_OPERATION_FAILED" }.into(),message_key:"source.operation.failed".into(),details:json!({"message":error.to_string()}),request_id:job_id.into(),job_id:Some(job_id.into()),retryable:false,recovery_actions:Vec::new() };
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
fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|time|time.as_millis() as i64).unwrap_or(0) }
