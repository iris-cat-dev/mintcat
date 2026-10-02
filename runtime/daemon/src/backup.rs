use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, sync::Arc};

use mintcat_feature_backup::{BackupBlockers, BackupError, BackupRuntimeState, BackupService, CloudBackupTransport, CloudCredentialProvider, CloudMetadata, MaintenanceGate, MaintenanceMarker, MaintenancePhase, ModuleVersion, PortableOwners, PortableSnapshot};
use mintcat_library_service::{CredentialVault as LibraryVault, LegacyCredential, LibrarySnapshot};
use mintcat_runtime_broker::{CredentialRef, SecretValue};
use mintcat_runtime_jobs::{request_hash, ActionBinding, ActionSpec, EventUpdate, RegisterJob, ConfirmationPolicy};
use mintcat_runtime_updates::UpdatePhase;
use mintcat_sdk::{ActionRequired, AppError, JobRef, JobState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{DaemonState, LOCAL_OWNER_SCOPE};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    method: String,
    params: Value,
    operation_id: String,
    frozen_source: Option<PathBuf>,
    source_digest: Option<String>,
    library_digest: Option<String>,
    library_revision: Option<u64>,
    owner_epochs: Option<Value>,
    module_lock_digest: Option<String>,
    codec_digest: String,
    lease_id: Option<String>,
}

/// The parent decodes the canonical command, enforces the human principal, then routes here.
/// Mutation jobs outlive the IPC request; only independent actions.approve releases an import.
pub(super) async fn handle(state: Arc<DaemonState>, method: &str, params: Value, key: Option<&str>, request_id: &str) -> Result<Value, Box<AppError>> {
    if method == "backup.cloud.list" {
        let (cloud, credentials) = cloud(&state, request_id)?;
        return serde_json::to_value(cloud.list(credentials.as_ref()).await.map_err(|e| error(request_id, e))?)
            .map_err(|e| error(request_id, BackupError::Json(e)));
    }
    let key = key.ok_or_else(|| error(request_id, BackupError::Invalid("idempotency key required".into())))?;
    let hash = request_hash(&params).map_err(|e| Box::new(super::job_error(request_id, e)))?;
    if state.jobs.find_operation(LOCAL_OWNER_SCOPE, method, key)
        .map_err(|e| Box::new(super::job_error(request_id, e)))?.is_some() {
        // register_job is the authority for exact request-hash replay, including
        // same-key/different-source conflicts. Do not read a deleted input on replay.
        let replay = state.jobs.register_job(RegisterJob {
            command: method.into(), owner_scope: LOCAL_OWNER_SCOPE.into(), idempotency_key: key.into(),
            request_hash: hash, target: Value::Null,
            confirmation_policy: if requires_approval(method) { ConfirmationPolicy::Required } else { ConfirmationPolicy::None },
        }).map_err(|e| Box::new(super::job_error(request_id, e)))?;
        return Ok(json!(replay.job));
    }
    if method.starts_with("backup.cloud.") { cloud(&state, request_id)?; }
    let approved = requires_approval(method);
    let admitted_codec=codec(&state,request_id)?;
    let operation_id = Uuid::new_v4().to_string();
    let mut target = Target { method: method.into(), params, operation_id, frozen_source: None, source_digest: None, library_digest: None, library_revision:None,owner_epochs:None,module_lock_digest: None,codec_digest:admitted_codec.provider_digest().into(),lease_id:None };
    // Freeze the explicitly selected input before asking for approval. Approval is never
    // authority over a pathname whose content can change while a human reads the prompt.
    if approved {
        let state2 = state.clone(); let request2 = request_id.to_owned();
        let codec2=admitted_codec.clone();
        target = tokio::task::spawn_blocking(move || {
        let state = state2; let request_id = request2.as_str();
        let gate = MaintenanceGate::exclusive(&state.layout, false).map_err(|e| error(request_id, e))?;
        let runtime = inspect(&state, None).map_err(|e| error(request_id, e))?;
        ensure_quiescent(&runtime.blockers)?;
        let snapshot = state.library.backup_snapshot().map_err(|e| error(request_id, e.into()))?;
        if snapshot != LibrarySnapshot::default() { return Err(error(request_id, BackupError::Library(mintcat_library_service::LibraryError::LibraryNotEmpty))); }
        state.features.ensure_empty().map_err(|e|error(request_id,e))?;
        target.owner_epochs=Some(state.features.portable_epochs()?);
        let bound_snapshot=if legacy(&target) {digest_json(&snapshot)} else {portable_snapshot(&state).and_then(|snapshot|digest_json(&snapshot))};
        target.library_digest = Some(bound_snapshot.map_err(|e| error(request_id, e))?);
        target.library_revision=Some(state.library.write_epoch().map_err(|e|error(request_id,e.into()))?);
        target.module_lock_digest = Some(module_lock(&state).map_err(|e| error(request_id, e))?);
        let source = required_path(&target.params, "sourcePath", request_id)?;
        let directory = state.layout.state_dir.join("mintcat.feature-backup").join("inputs");
        fs::create_dir_all(&directory).map_err(|e| error(request_id, e.into()))?;
        let frozen = directory.join(format!("{}.input", target.operation_id));
        if legacy(&target) {
            mintcat_library_service::LibraryStore::snapshot_legacy_source(&source, &frozen).map_err(|e| error(request_id, e.into()))?;
            private_file(&frozen).map_err(|e| error(request_id, e.into()))?;
            target.source_digest = Some(mintcat_library_service::LibraryStore::legacy_source_digest(&frozen).map_err(|e| error(request_id, e.into()))?);
        } else {
            let bytes = fs::read(source).map_err(|e| error(request_id, e.into()))?;
            let (_,decoded)=codec2.decode(&bytes,&runtime.modules).map_err(|e|error(request_id,e))?;
            state.features.validate(&decoded.owners).map_err(|e|error(request_id,e))?;
            if decoded.owners.keys().ne(state.features.snapshot().map_err(|e|error(request_id,e))?.keys()) {return Err(error(request_id,BackupError::Module("portable owner scope mismatch".into())));}
            target.source_digest = Some(digest(&bytes));
            write_new(&frozen, &bytes).map_err(|e| error(request_id, e))?;
        }
        target.frozen_source = Some(frozen);
        drop(gate);
        Ok::<_,Box<AppError>>(target)
        }).await.map_err(|e|Box::new(super::internal_error(request_id,e.to_string())))??;
    }
    let updates=state.updates.as_ref().ok_or_else(||error(request_id,BackupError::Module("updates owner unavailable".into())))?;
    if method!="backup.recover" {
        let environment=updates.environment().map_err(|e|Box::new(super::internal_error(request_id,e.to_string())))?;
        let generation=environment.scopes.get(admitted_codec.scope_id()).ok_or_else(||error(request_id,BackupError::Module("backup scope generation unavailable".into())))?;
        if !generation.targets.signed.packages.values().any(|package|package.digest==admitted_codec.provider_digest()) {
            if let Some(source)=&target.frozen_source {let _=fs::remove_file(source);}
            return Err(error(request_id,BackupError::Invalid("backup codec no longer belongs to admitted signed closure".into())));
        }
        let lease=updates.acquire_lease(Some(admitted_codec.scope_id()),&generation.generation_id,&format!("backup:{}",target.operation_id),mintcat_runtime_updates::LeaseKind::Task,None)
            .map_err(|e|{
                if let Some(source)=&target.frozen_source {let _=fs::remove_file(source);}
                Box::new(super::internal_error(request_id,e.to_string()))
            })?;
        target.lease_id=Some(lease.lease_id);
    }
    let registration = state.jobs.register_job(RegisterJob {
        command: method.into(), owner_scope: LOCAL_OWNER_SCOPE.into(), idempotency_key: key.into(), request_hash: hash,
        target: serde_json::to_value(&target).map_err(|e| error(request_id, e.into()))?,
        confirmation_policy: if approved { ConfirmationPolicy::Required } else { ConfirmationPolicy::None },
    }).map_err(|e| {
        if let Some(lease)=&target.lease_id {let _=updates.release_lease(lease);}
        if let Some(source)=&target.frozen_source {let _=fs::remove_file(source);}
        Box::new(super::job_error(request_id,e))
    })?;
    if !registration.created {
        if let Some(lease)=&target.lease_id {let _=updates.release_lease(lease);}
        if let Some(path) = target.frozen_source { let _ = fs::remove_file(path); }
        return Ok(json!(registration.job));
    }
    state.jobs.append_event(&registration.job.job_id, EventUpdate::state("backup-preparing", JobState::Preparing))
        .map_err(|e| Box::new(super::job_error(request_id, e)))?;
    if approved {
        let publication=(||->Result<(),Box<AppError>> {
        let binding = binding(&state, &target).map_err(|e| error(request_id, e))?;
        state.jobs.create_action(&registration.job.job_id, ActionSpec {
            kind: "backup-owner-import".into(), operation_id: target.operation_id.clone(), target: binding.target,
            target_revision: binding.target_revision, plan_hash: binding.plan_hash, module_lock_digest: binding.module_lock_digest,
            expires_at_ms: super::now_ms().map_err(|e| error(request_id, BackupError::Io(e)))?.saturating_add(30 * 60 * 1000),
            response_schema: json!({"type":"object","properties":{"approved":{"const":true},"importCredentials":{"type":"boolean"}},"required":["approved","importCredentials"],"additionalProperties":false}),
        }).map_err(|e| Box::new(super::job_error(request_id, e)))?;
        Ok(())
        })();
        if let Err(mut failure)=publication {
            failure.job_id=Some(registration.job.job_id.clone());
            let mut event=EventUpdate::state("backup-approval-failed",JobState::Failed); event.error=Some((*failure).clone());
            let _=state.jobs.append_event(&registration.job.job_id,event);
            let _=release_terminal_lease(&state,&registration.job.job_id);
            return Err(failure);
        }
        state.backup_pending_codecs.lock().expect("backup pending registry poisoned").insert(registration.job.job_id.clone(),admitted_codec);
    } else {
        spawn(state, registration.job.clone(), target, None,admitted_codec);
    }
    Ok(json!(registration.job))
}

pub(super) fn owns_action(action: &ActionRequired) -> bool { action.kind == "backup-owner-import" }

pub(super) fn resume_action(state: Arc<DaemonState>, action: &ActionRequired) -> Result<(), Box<AppError>> {
    if !owns_action(action) { return Err(error("backup-action", BackupError::Invalid("foreign action".into()))); }
    let job = state.jobs.action_job(LOCAL_OWNER_SCOPE, &action.action_id).map_err(|e| Box::new(super::job_error("backup-action", e)))?;
    let snapshot = state.jobs.get_job(LOCAL_OWNER_SCOPE, &job.job_id).map_err(|e| Box::new(super::job_error("backup-action", e)))?;
    if matches!(snapshot.state, JobState::Succeeded | JobState::Failed | JobState::Cancelled | JobState::RecoveryRequired) { return Ok(()); }
    let target: Target = serde_json::from_value(state.jobs.get_job_target(LOCAL_OWNER_SCOPE, &job.job_id)
        .map_err(|e| Box::new(super::job_error("backup-action", e)))?).map_err(|e| error("backup-action", e.into()))?;
    if !requires_approval(&target.method) || action.operation_id != target.operation_id || snapshot.command != target.method {
        return Err(error("backup-action", BackupError::Invalid("action operation mismatch".into())));
    }
    let admitted=state.backup_pending_codecs.lock().expect("backup pending registry poisoned").get(&job.job_id).cloned().map(Ok)
        .unwrap_or_else(||codec(&state,"backup-action"))?;
    if admitted.provider_digest()!=target.codec_digest {return Err(error("backup-action",BackupError::Invalid("approved backup codec package changed".into())));}
    spawn(state, job, target, Some(action.action_id.clone()),admitted);
    Ok(())
}

fn spawn(state: Arc<DaemonState>, job: JobRef, target: Target, action: Option<String>,admitted_codec:Arc<mintcat_feature_backup::NativeBackupCodec>) {
    let mut running = state.backup_running.lock().expect("backup running registry poisoned");
    if !running.insert(job.job_id.clone()) { return; }
    drop(running);
    tokio::spawn(async move {
        let job_id = job.job_id.clone();
        let result = execute(state.clone(), &job_id, &target, action.as_deref(),admitted_codec).await;
        if let Err(failure) = result {
            let mut update = EventUpdate::state("backup-failed", JobState::Failed);
            let mut failure = *failure;
            failure.job_id = Some(job_id.clone());
            update.error = Some(failure);
            // A commit may already have happened: preserve recovery-required instead of
            // advertising a retriable failure that might duplicate a remote upload/import.
            let committing = state.jobs.commit_state(LOCAL_OWNER_SCOPE, &job_id).ok()
                .is_some_and(|state| matches!(state, mintcat_runtime_jobs::CommitState::InProgress | mintcat_runtime_jobs::CommitState::Unknown));
            if committing && target.method == "backup.recover" {
                let _ = state.jobs.finish_uncommitted(&job_id, update.error);
            } else if committing {
                let _ = state.jobs.mark_commit_unknown(&job_id);
                update.state=JobState::RecoveryRequired; update.phase="backup-inspection-required".into();
                let _ = state.jobs.append_event(&job_id,update);
            }
            else { let _ = state.jobs.append_event(&job_id, update); }
        }
        state.backup_running.lock().expect("backup running registry poisoned").remove(&job_id);
        let _=release_terminal_lease(&state,&job_id);
    });
}

async fn execute(state: Arc<DaemonState>, job_id: &str, target: &Target, action: Option<&str>,admitted_codec:Arc<mintcat_feature_backup::NativeBackupCodec>) -> Result<(), Box<AppError>> {
    let request_id = job_id;
    let service = service(&state,request_id,admitted_codec.clone())?;
    if requires_approval(&target.method) {
        let state2 = state.clone(); let target2 = target.clone(); let job2 = job_id.to_owned();
        let action = action.ok_or_else(|| error(request_id, BackupError::Invalid("independent approval required".into())))?.to_owned();
        let result = tokio::task::spawn_blocking(move || import(&state2, &job2, &target2, &action,admitted_codec)).await
            .map_err(|e| Box::new(super::internal_error(request_id, e.to_string())))??;
        state.jobs.finish_commit(job_id, result).map_err(|e| Box::new(super::job_error(request_id, e)))?;
        return Ok(());
    }
    state.jobs.append_event(job_id, EventUpdate::state("backup-started", JobState::Running)).map_err(|e| Box::new(super::job_error(request_id, e)))?;
    let value = match target.method.as_str() {
        "backup.export" => {
            let path = required_path(&target.params, "targetPath", request_id)?;
            let state2 = state.clone(); let job2 = job_id.to_owned();
            tokio::task::spawn_blocking(move || {
                let (bytes,result) = service.export_archive(|| inspect(&state2,Some(&job2))).map_err(|e|error(&job2,e))?;
                commit_output(&state2,&job2,&path,&bytes,json!(result))
            }).await.map_err(|e|Box::new(super::internal_error(request_id,e.to_string())))??
        }
        "backup.recover" => {
            let state2 = state.clone(); let job2 = job_id.to_owned();
            state.jobs.begin_commit(job_id).map_err(|e| Box::new(super::job_error(request_id, e)))?;
            let recovered = tokio::task::spawn_blocking(move || recover(&state2, &job2)).await
                .map_err(|e| Box::new(super::internal_error(request_id, e.to_string())))??;
            super::composition::resume_after_backup_recovery(&state).await.map_err(|message|Box::new(super::internal_error(request_id,message)))?;
            json!({"recovered": recovered})
        }
        "backup.cloud.upload" => {
            let (cloud, credentials) = cloud(&state, request_id)?;
            let state2 = state.clone(); let job2 = job_id.to_owned();
            let bytes = tokio::task::spawn_blocking(move || service.export_bytes(|| inspect(&state2, Some(&job2)))).await
                .map_err(|e| Box::new(super::internal_error(request_id, e.to_string())))?.map_err(|e| error(request_id, e))?;
            let metadata: CloudMetadata = serde_json::from_value(target.params.get("metadata").cloned().ok_or_else(|| error(request_id, BackupError::Invalid("metadata required".into())))?)
                .map_err(|e| error(request_id, e.into()))?;
            // Never retry an ambiguous POST: the server has no specified keyed upload API.
            state.jobs.begin_commit(job_id).map_err(|e| Box::new(super::job_error(request_id, e)))?;
            json!(cloud.upload(bytes, metadata, credentials.as_ref()).await.map_err(|e| error(request_id, e))?)
        }
        "backup.cloud.download" => {
            let (cloud, credentials) = cloud(&state, request_id)?;
            let id = target.params.get("backupId").and_then(Value::as_str).ok_or_else(|| error(request_id, BackupError::Invalid("backupId required".into())))?;
            let path = required_path(&target.params, "targetPath", request_id)?;
            let bytes = cloud.download(id, credentials.as_ref()).await.map_err(|e| error(request_id, e))?;
            let state2=state.clone(); let job2=job_id.to_owned();
            tokio::task::spawn_blocking(move || {
                let gate=MaintenanceGate::exclusive(&state2.layout,false).map_err(|e|error(&job2,e))?;
                let runtime=inspect(&state2,Some(&job2)).map_err(|e|error(&job2,e))?;
                ensure_quiescent(&runtime.blockers)?;
                let result=admitted_codec.validate(&bytes,&runtime.modules).map_err(|e|error(&job2,e))?;
                let result=commit_output(&state2,&job2,&path,&bytes,json!(result))?;
                drop(gate); Ok::<_,Box<AppError>>(result)
            }).await.map_err(|e|Box::new(super::internal_error(request_id,e.to_string())))??
        }
        _ => return Err(error(request_id, BackupError::Invalid("unsupported backup operation".into()))),
    };
    state.jobs.finish_commit(job_id, value).map_err(|e| Box::new(super::job_error(request_id, e)))?;
    Ok(())
}

fn import(state: &DaemonState, job_id: &str, target: &Target, action_id: &str,admitted_codec:Arc<mintcat_feature_backup::NativeBackupCodec>) -> Result<Value, Box<AppError>> {
    if legacy(target) {
        let gate = MaintenanceGate::exclusive(&state.layout, false).map_err(|e| error(job_id, e))?;
        let runtime = inspect(state, Some(job_id)).map_err(|e| error(job_id, e))?;
        ensure_quiescent(&runtime.blockers)?;
        let binding = binding(state, target).map_err(|e| error(job_id, e))?;
        verify_binding(state, target, &binding).map_err(|e| error(job_id, e))?;
        let consumed = state.jobs.consume_action(action_id, &binding).map_err(|e| Box::new(super::job_error(job_id, e)))?;
        if consumed.job.job_id != job_id || consumed.response.get("approved") != Some(&Value::Bool(true)) { return Err(error(job_id, BackupError::Invalid("approval does not authorize this job".into()))); }
        state.jobs.begin_commit(job_id).map_err(|e| Box::new(super::job_error(job_id, e)))?;
        // Empty-before-commit is the only safe automatic conclusion until the owner
        // supplies the exact committed snapshot digest. A crash in this interval must
        // not clear maintenance merely because an import_run happens to be present.
        gate.mark(&MaintenanceMarker { format_version: 1, owner_id: "mintcat.library".into(), operation_id: Some(target.operation_id.clone()), phase: MaintenancePhase::ImportingEmptyOwner,
            snapshot_digest: target.library_digest.clone().ok_or_else(|| error(job_id, BackupError::Invalid("target digest missing".into())))? }).map_err(|e| error(job_id, e))?;
        let vault = ImportVault { credentials: &state.credential_vault, permitted: consumed.response.get("importCredentials") == Some(&Value::Bool(true)) };
        let source = target.frozen_source.as_ref().ok_or_else(|| error(job_id, BackupError::Invalid("immutable source missing".into())))?;
        let expected_digest=target.source_digest.as_deref().ok_or_else(||error(job_id,BackupError::Invalid("approved source digest missing".into())))?;
        let report = state.library.import_legacy_checked(source,expected_digest,&vault).map_err(|e| error(job_id, e.into()))?;
        let snapshot = state.library.backup_snapshot().map_err(|e| error(job_id, e.into()))?;
        save_committed_checkpoint(state, job_id, json!(report), &snapshot)?;
        gate.mark(&MaintenanceMarker { format_version: 1, owner_id: "mintcat.library".into(), operation_id: Some(target.operation_id.clone()), phase: MaintenancePhase::RestoreCommitted, snapshot_digest: digest_json(&snapshot).map_err(|e| error(job_id, e))? }).map_err(|e| error(job_id, e))?;
        gate.clear().map_err(|e| error(job_id, e))?;
        Ok(json!(report))
    } else {
        let source = target.frozen_source.as_ref().ok_or_else(|| error(job_id, BackupError::Invalid("immutable source missing".into())))?;
        let bytes = fs::read(source).map_err(|e| error(job_id, e.into()))?;
        if Some(digest(&bytes).as_str())!=target.source_digest.as_deref() {
            return Err(error(job_id,BackupError::Invalid("archive bytes no longer match the independently approved source digest".into())));
        }
        let service = service(state,job_id,admitted_codec)?;
        let result = service.restore_bytes_for_operation(&bytes,Some(&target.operation_id), || {
            let runtime = inspect(state, Some(job_id))?;
            let binding = binding(state, target)?;
            verify_binding(state, target, &binding)?;
            let consumed = state.jobs.consume_action(action_id, &binding).map_err(|e| BackupError::Invalid(e.to_string()))?;
            if consumed.job.job_id != job_id || consumed.response.get("approved") != Some(&Value::Bool(true)) { return Err(BackupError::Invalid("approval job mismatch".into())); }
            state.jobs.begin_commit(job_id).map_err(|e| BackupError::Invalid(e.to_string()))?;
            Ok(runtime)
        }).map_err(|e| error(job_id, e))?;

        Ok(json!(result))
    }
}
fn commit_output(state:&DaemonState,job_id:&str,path:&Path,bytes:&[u8],result:Value)->Result<Value,Box<AppError>> {
    let job=state.jobs.get_job(LOCAL_OWNER_SCOPE,job_id).map_err(|e|Box::new(super::job_error(job_id,e)))?;
    state.jobs.save_checkpoint(job_id,job.last_seq,&mintcat_runtime_jobs::WorkerCheckpoint {
        step:"backup-output-prepared".into(),payload:json!({"outputDigest":digest(bytes),"targetPath":path,"result":result}),
        resume_policy:mintcat_runtime_jobs::ResumePolicy::InspectBeforeResume,
    }).map_err(|e|Box::new(super::job_error(job_id,e)))?;
    state.jobs.begin_commit(job_id).map_err(|e|Box::new(super::job_error(job_id,e)))?;
    write_new(path,bytes).map_err(|e|error(job_id,e))?;
    Ok(result)
}

fn save_committed_checkpoint(state: &DaemonState, job_id: &str, result: Value, snapshot: &LibrarySnapshot) -> Result<(), Box<AppError>> {
    let job = state.jobs.get_job(LOCAL_OWNER_SCOPE, job_id).map_err(|e| Box::new(super::job_error(job_id, e)))?;
    state.jobs.save_checkpoint(job_id, job.last_seq, &mintcat_runtime_jobs::WorkerCheckpoint {
        step: "owner-import-committed".into(), payload: json!({"result":result,"snapshotDigest":digest_json(snapshot).map_err(|e| error(job_id,e))?,"writeEpoch":state.library.write_epoch().map_err(|e|error(job_id,e.into()))?}),
        resume_policy: mintcat_runtime_jobs::ResumePolicy::InspectBeforeResume,
    }).map_err(|e| Box::new(super::job_error(job_id, e)))?;
    Ok(())
}

/// Inspection never reapplies an import. Exact owner evidence can repair an interrupted
/// maintenance marker and complete a durable job; changed owner bytes remain blocked.
fn recover(state: &DaemonState, job_id: &str) -> Result<bool, Box<AppError>> {
    let gate = MaintenanceGate::exclusive(&state.layout, true).map_err(|e| error(job_id,e))?;
    let runtime = inspect(state, Some(job_id)).map_err(|e| error(job_id,e))?;
    ensure_quiescent(&runtime.blockers)?;
    let snapshot = state.library.backup_snapshot().map_err(|e| error(job_id,e.into()))?;
    let snapshot_digest = digest_json(&snapshot).map_err(|e| error(job_id,e))?;
    let write_epoch = state.library.write_epoch().map_err(|e| error(job_id,e.into()))?;
    let portable=portable_snapshot(state).map_err(|e|error(job_id,e))?;
    let connection = state.jobs.database().connection().map_err(|e| Box::new(super::internal_error(job_id,e.to_string())))?;
    let mut statement = connection.prepare("SELECT job_id,target_json,checkpoint_json FROM jobs WHERE owner_scope=?1 AND state='recovery-required' AND command IN ('backup.restore','library.import-legacy','library.config-import','backup.export','backup.cloud.download','backup.recover') ORDER BY job_id")
        .map_err(|e| Box::new(super::internal_error(job_id,e.to_string())))?;
    let records = statement.query_map([LOCAL_OWNER_SCOPE], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?)))
        .map_err(|e| Box::new(super::internal_error(job_id,e.to_string())))?
        .collect::<Result<Vec<_>,_>>().map_err(|e| Box::new(super::internal_error(job_id,e.to_string())))?;
    let marker = gate.read().map_err(|e| error(job_id,e))?;
    let mut proven = Vec::new();
    let mut uncommitted = Vec::new();
    for (id, encoded, checkpoint) in records {
        let target: Target = serde_json::from_str(&encoded).map_err(|e| error(job_id,e.into()))?;
        let checkpoint: Option<Value> = checkpoint.map(|encoded| serde_json::from_str(&encoded)).transpose().map_err(|e| error(job_id,e.into()))?;
        if matches!(target.method.as_str(),"backup.export" | "backup.cloud.download") {
            if let Some(checkpoint)=checkpoint {
                let path=checkpoint["targetPath"].as_str().map(PathBuf::from).ok_or_else(||error(job_id,BackupError::RecoveryRequired))?;
                if path.try_exists().map_err(|e|error(job_id,e.into()))? {
                    let output_digest=digest_file(&path).map_err(|e|error(job_id,e))?;
                    if checkpoint["outputDigest"].as_str()!=Some(output_digest.as_str()) {return Err(error(job_id,BackupError::RecoveryRequired));}
                    proven.push((id,checkpoint["result"].clone()));
                } else {uncommitted.push(id);}
            } else {uncommitted.push(id);}
            continue;
        }
        if target.method=="backup.recover" {uncommitted.push(id);continue;}
        let result = if let Some(checkpoint) = checkpoint.filter(|checkpoint| legacy(&target)
            && checkpoint["snapshotDigest"].as_str() == Some(snapshot_digest.as_str())
            && checkpoint["writeEpoch"].as_u64() == Some(write_epoch)) {
            Some(checkpoint["result"].clone())
        } else if !legacy(&target) {
            let source = target.frozen_source.as_ref().ok_or_else(|| error(job_id,BackupError::RecoveryRequired))?;
            let bytes = fs::read(source).map_err(|e| error(job_id,e.into()))?;
            if Some(digest(&bytes).as_str()) != target.source_digest.as_deref() { return Err(error(job_id,BackupError::RecoveryRequired)); }
            let (manifest,mut expected) = retained_codec(state,&id,&target)?.decode(&bytes,&runtime.modules).map_err(|e| error(job_id,e))?;
            let result = mintcat_feature_backup::BackupResult { manifest,counts:expected.counts() };
            expected.clear_credentials();
            let mut expected_epochs=target.owner_epochs.clone().ok_or_else(||error(job_id,BackupError::RecoveryRequired))?;
            for id in ["mintcat.feature.agent","mintcat.feature.proxy"] {
                let increment=id=="mintcat.feature.agent" || expected.owners.get(id).is_some_and(|owner|owner.content.as_object().is_some_and(|values|!values.is_empty()));
                let epoch=expected_epochs[id].as_u64().ok_or_else(||error(job_id,BackupError::RecoveryRequired))?;
                expected_epochs[id]=json!(epoch.checked_add(u64::from(increment)).ok_or_else(||error(job_id,BackupError::RecoveryRequired))?);
            }
            let unchanged_fence=state.library.write_epoch().map_err(|e|error(job_id,e.into()))?==target.library_revision.and_then(|epoch|epoch.checked_add(1)).ok_or_else(||error(job_id,BackupError::RecoveryRequired))?
                && state.features.portable_epochs()?==expected_epochs;
            if portable == expected && unchanged_fence { Some(json!(result)) } else { None }
        } else {
            let digest = target.source_digest.as_deref().ok_or_else(|| error(job_id,BackupError::RecoveryRequired))?;
            let outcome = state.library.legacy_import_outcome(digest).map_err(|e| error(job_id,e.into()))?;
            outcome.filter(|outcome| outcome.snapshot_digest == snapshot_digest).map(|outcome|json!(outcome.result))
        };
        if let Some(result) = result {
            if marker.as_ref().is_some_and(|marker| marker.owner_id == "mintcat.library" && marker.operation_id.as_deref() == Some(target.operation_id.as_str())) {
                gate.mark(&MaintenanceMarker { format_version:1,owner_id:"mintcat.library".into(),operation_id:Some(target.operation_id),phase:MaintenancePhase::RestoreCommitted,snapshot_digest:snapshot_digest.clone() }).map_err(|e| error(job_id,e))?;
            }
            proven.push((id, result));
        } else if snapshot != LibrarySnapshot::default() || state.features.ensure_empty().is_err() {
            // Neither a count nor an import_run proves absence of newer writes.
            return Err(error(job_id,BackupError::RecoveryRequired));
        } else {
            uncommitted.push(id);
        }
    }
    drop(statement); drop(connection); drop(gate);
    let service = service(state,job_id,codec(state,job_id)?)?;
    let cleared = service.recover_maintenance(|| inspect(state,Some(job_id))).map_err(|e| error(job_id,e))?;
    for (id,result) in proven {
        state.jobs.finish_recovered_commit(&id,result).map_err(|e| Box::new(super::job_error(job_id,e)))?;
        release_terminal_lease(state,&id)?;
    }
    for id in uncommitted {
        let failure = *error(job_id,BackupError::Invalid("explicit recovery proved import did not commit; submit a new independently approved request".into()));
        let commit = state.jobs.commit_state(LOCAL_OWNER_SCOPE,&id).map_err(|e| Box::new(super::job_error(job_id,e)))?;
        if matches!(commit,mintcat_runtime_jobs::CommitState::Unknown | mintcat_runtime_jobs::CommitState::InProgress) {
            state.jobs.finish_uncommitted(&id,Some(failure)).map_err(|e| Box::new(super::job_error(job_id,e)))?;
        } else {
            let mut event = EventUpdate::state("recovered-uncommitted",JobState::Failed); event.error=Some(failure);
            state.jobs.append_event(&id,event).map_err(|e| Box::new(super::job_error(job_id,e)))?;
        }
        release_terminal_lease(state,&id)?;
    }
    Ok(cleared)
}

/// Run before *any* Library startup mutation, including download/guard recovery.
/// Failure is read-only rescue evidence for the parent; never fail daemon binding
/// or open owner writes just to make startup recovery appear successful.
pub(super) fn startup_recover(state:&DaemonState)->Result<bool,Box<AppError>> {
    let gate=MaintenanceGate::exclusive(&state.layout,true).map_err(|e|error("backup-startup",e))?;
    let marker=gate.read().map_err(|e|error("backup-startup",e))?;
    drop(gate);
    if marker.is_some_and(|marker|["mintcat.library","mintcat.backup.portable"].contains(&marker.owner_id.as_str())) {Err(error("backup-startup",BackupError::RecoveryRequired))} else {Ok(false)}
}

pub(super) fn release_terminal_lease(state:&DaemonState,job_id:&str)->Result<(),Box<AppError>> {
    let snapshot=state.jobs.get_job(LOCAL_OWNER_SCOPE,job_id).map_err(|e|Box::new(super::job_error(job_id,e)))?;
    if !matches!(snapshot.state,JobState::Succeeded | JobState::Cancelled | JobState::Failed) {return Ok(());}
    if !matches!(snapshot.command.as_str(),"backup.export" | "backup.restore" | "backup.recover" | "backup.cloud.upload" | "backup.cloud.download" | "library.import-legacy" | "library.config-import") {return Ok(());}
    let target:Target=serde_json::from_value(state.jobs.get_job_target(LOCAL_OWNER_SCOPE,job_id).map_err(|e|Box::new(super::job_error(job_id,e)))?).map_err(|e|error(job_id,e.into()))?;
    state.backup_pending_codecs.lock().expect("backup pending registry poisoned").remove(job_id);
    if let Some(lease)=target.lease_id {
        if let Some(updates)=&state.updates {
            if updates.leases().map_err(|e|Box::new(super::internal_error(job_id,e.to_string())))?.iter().any(|known|known.lease_id==lease) {
                updates.release_lease(&lease).map_err(|e|Box::new(super::internal_error(job_id,e.to_string())))?;
            }
        }
    }
    if let Some(source)=target.frozen_source {
        match fs::remove_file(source) {Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(error(job_id,e.into()))}
    }
    Ok(())
}

fn binding(state: &DaemonState, target: &Target) -> Result<ActionBinding, BackupError> {
    let snapshot = state.library.backup_snapshot()?;
    let library_digest = if legacy(target) {digest_json(&snapshot)?} else {digest_json(&portable_snapshot(state)?)?};
    let module_digest = module_lock(state)?;
    let source = target.frozen_source.as_ref().ok_or_else(|| BackupError::Invalid("immutable source missing".into()))?;
    let source_digest = if legacy(target) { mintcat_library_service::LibraryStore::legacy_source_digest(source)? } else { digest_file(source)? };
    let bound_target = json!({"ownerId":if legacy(target) {"mintcat.library"} else {"mintcat.backup.portable"},"ownerSchemas":{"mintcat.library":5,"mintcat.feature.agent":1,"mintcat.feature.proxy":1},"ownerEpochs":state.features.portable_epochs().map_err(|e|BackupError::Invalid(format!("{}: {}",e.code,e.message_key)))?,"dataDirectory":state.layout.state_dir,"method":target.method,"sourceDigest":source_digest,"targetDigest":library_digest});
    let plan_hash = digest_json(&bound_target)?;
    let target_revision = state.library.write_epoch()?;
    Ok(ActionBinding { target: bound_target, target_revision, plan_hash, module_lock_digest: module_digest })
}

fn verify_binding(state: &DaemonState, target: &Target, current: &ActionBinding) -> Result<(), BackupError> {
    if current.target["sourceDigest"].as_str() != target.source_digest.as_deref()
        || current.target["targetDigest"].as_str() != target.library_digest.as_deref()
        || Some(current.target_revision)!=target.library_revision
        || Some(&current.target["ownerEpochs"])!=target.owner_epochs.as_ref()
        || Some(current.module_lock_digest.as_str()) != target.module_lock_digest.as_deref() {
        return Err(BackupError::Invalid("approval source, target revision or signed module closure changed".into()));
    }
    if state.library.snapshot()? != LibrarySnapshot::default() { return Err(mintcat_library_service::LibraryError::LibraryNotEmpty.into()); }
    state.features.ensure_empty()?;
    Ok(())
}

fn codec(state:&DaemonState,request_id:&str)->Result<Arc<mintcat_feature_backup::NativeBackupCodec>,Box<AppError>> {
    state.backup_codec.read().expect("backup codec registry poisoned").clone().ok_or_else(||error(request_id,BackupError::Module("signed mintcat-backup-worker backup.worker@2 provider unavailable".into())))
}
fn service(state:&DaemonState,request_id:&str,codec:Arc<mintcat_feature_backup::NativeBackupCodec>)->Result<BackupService,Box<AppError>> {
    BackupService::with_native_backend(state.layout.clone(),state.library.clone(),codec).map(|service|service.with_owners(state.features.clone())).map_err(|e|error(request_id,e))
}

/// Only called on the blocking recovery path. A durable task lease, not current
/// routing or a caller-selected digest, authorizes reconstruction of a prior codec.
fn retained_codec(state:&DaemonState,job_id:&str,target:&Target)->Result<Arc<mintcat_feature_backup::NativeBackupCodec>,Box<AppError>> {
    if let Some(codec)=state.backup_pending_codecs.lock().expect("backup pending registry poisoned").get(job_id).cloned() {
        if codec.provider_digest()==target.codec_digest {return Ok(codec);}
    }
    if let Ok(current)=codec(state,job_id) {
        if current.provider_digest()==target.codec_digest {return Ok(current);}
    }
    let updates=state.updates.as_ref().ok_or_else(||error(job_id,BackupError::RecoveryRequired))?;
    let lease_id=target.lease_id.as_deref().ok_or_else(||error(job_id,BackupError::RecoveryRequired))?;
    let lease=updates.leases().map_err(|e|Box::new(super::internal_error(job_id,e.to_string())))?
        .into_iter().find(|lease|lease.lease_id==lease_id && lease.owner_id==format!("backup:{}",target.operation_id))
        .ok_or_else(||error(job_id,BackupError::RecoveryRequired))?;
    for (scope,generation) in lease.environment.scopes {
        if generation.targets.signed.packages.values().any(|package|package.digest==target.codec_digest && package.manifest.id=="mintcat.feature.backup") {
            let supervisor=updates.supervisor_for_generation(&scope,&generation.generation_id).map_err(|e|Box::new(super::internal_error(job_id,e.to_string())))?
                .ok_or_else(||error(job_id,BackupError::Module("retained backup realm is not activated; keep read-only rescue until its verified generation is restored".into())))?;
            let codec=Arc::new(mintcat_feature_backup::NativeBackupCodec::new(supervisor,scope,target.codec_digest.clone()).map_err(|e|error(job_id,e))?);
            state.backup_pending_codecs.lock().expect("backup pending registry poisoned").insert(job_id.into(),codec.clone());
            return Ok(codec);
        }
    }
    Err(error(job_id,BackupError::RecoveryRequired))
}

/// Called only by BackupService after it owns exclusive admission (or while our import owns it).
fn inspect(state: &DaemonState, own_job: Option<&str>) -> Result<BackupRuntimeState, BackupError> {
    let recovering = own_job==Some("backup-startup") || own_job.and_then(|id| state.jobs.get_job(LOCAL_OWNER_SCOPE, id).ok())
        .is_some_and(|job| job.command == "backup.recover");
    let connection = state.jobs.database().connection().map_err(|e| BackupError::Invalid(e.to_string()))?;
    let mut blockers = BackupBlockers::default();
    let mut statement = connection.prepare("SELECT job_id,command,state FROM jobs WHERE state NOT IN ('succeeded','failed','cancelled') ORDER BY job_id")
        .map_err(|e| BackupError::Invalid(e.to_string()))?;
    let jobs = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(|e| BackupError::Invalid(e.to_string()))?;
    for job in jobs {
        let (id, command, phase) = job.map_err(|e| BackupError::Invalid(e.to_string()))?;
        if Some(id.as_str()) == own_job { continue; }
        // Pending human intent has not admitted an owner mutation. Recovery-required
        // maintenance jobs are inspected by their owner marker, never resumed implicitly.
        if phase == "awaiting-approval" { continue; }
        if command.starts_with("install.") || command == "mods.pack" { blockers.unfinished_install_jobs.push(id.clone()); }
        if !(recovering && (requires_approval(&command) || matches!(command.as_str(),"backup.export" | "backup.cloud.download" | "backup.recover")) && phase == "recovery-required") {
            blockers.active_jobs.push(id);
        }
    }
    drop(statement);
    let has_journals: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='file_transactions')", [], |row| row.get(0))
        .map_err(|e| BackupError::Invalid(e.to_string()))?;
    if has_journals {
        let mut journals = connection.prepare("SELECT transaction_id FROM file_transactions WHERE state IN ('prepared','applying','recovery-required') ORDER BY transaction_id")
            .map_err(|e| BackupError::Invalid(e.to_string()))?;
        blockers.file_journals = journals.query_map([], |row| row.get(0)).map_err(|e| BackupError::Invalid(e.to_string()))?
            .collect::<Result<Vec<String>, _>>().map_err(|e| BackupError::Invalid(e.to_string()))?;
    }
    let mut plan_ids=Vec::new();
        let has_plans: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='install_plans')", [], |row| row.get(0))
            .map_err(|e| BackupError::Invalid(e.to_string()))?;
        if has_plans {
            let mut plans = connection.prepare("SELECT plan_id FROM install_plans WHERE state='applying' ORDER BY plan_id")
                .map_err(|e| BackupError::Invalid(e.to_string()))?;
            plan_ids = plans.query_map([],|row| row.get::<_,String>(0)).map_err(|e| BackupError::Invalid(e.to_string()))?
                .collect::<Result<Vec<_>,_>>().map_err(|e| BackupError::Invalid(e.to_string()))?;
        }
    drop(connection);
    if state.install.service().has_unresolved_file_transactions().map_err(|e| BackupError::Invalid(e.to_string()))? && blockers.file_journals.is_empty() {
        return Err(BackupError::Busy("file transaction owner reports unresolved journal".into()));
    }
    for id in plan_ids {
        // Consult each owner only after releasing the shared Kernel database guard.
        let _unresolved=state.install.service().has_unresolved_plan(&id).map_err(|e|BackupError::Invalid(e.to_string()))?;
        blockers.unfinished_install_jobs.push(id);
    }
    // Install plan authority uses these journals as its unresolved commit boundary;
    // admitted unfinished plans without a journal are also represented by actual jobs.
    if let Some(updates) = &state.updates {
        for operation in updates.operations().map_err(|e| BackupError::Invalid(e.to_string()))? {
            if !matches!(operation.phase, UpdatePhase::Confirmed | UpdatePhase::Rejected | UpdatePhase::RolledBack) { blockers.module_transitions.push(operation.operation_id); }
        }
    }
    // Schema inspection must not read/serialize entire Agent history twice.
    // Service.snapshot checks actual owner quiescence under this same admission.
    let versions=modules(state)?;
    if versions.get("mintcat.feature.agent").map(|version|version.schema_version)!=Some(1) {
        return Err(BackupError::Module("signed portable owner unavailable: mintcat.feature.agent".into()));
    }
    if versions.get("mintcat.feature.proxy").is_some_and(|version|version.schema_version != 1) {
        return Err(BackupError::Module("signed Proxy portable owner schema is incompatible".into()));
    }
    Ok(BackupRuntimeState { blockers, modules: versions })
}
fn portable_snapshot(state:&DaemonState)->Result<PortableSnapshot,BackupError> {
    let mut snapshot=PortableSnapshot {library:state.library.backup_snapshot()?,owners:state.features.snapshot()?};
    snapshot.exclude_execution_authority()?;
    Ok(snapshot)
}

fn modules(state: &DaemonState) -> Result<BTreeMap<String, ModuleVersion>, BackupError> {
    let updates = state.updates.as_ref().ok_or_else(|| BackupError::Module("signed installed module closure unavailable".into()))?;
    let environment = updates.environment().map_err(|e| BackupError::Invalid(e.to_string()))?;
    let mut modules = BTreeMap::new();
    for generation in environment.scopes.values() {
        for package in generation.targets.signed.packages.values() {
            let version = ModuleVersion { version: package.manifest.version.clone(), schema_version: package.manifest.state.schema_version.try_into().map_err(|_| BackupError::Invalid("schema version overflow".into()))? };
            if let Some(previous) = modules.insert(package.manifest.id.clone(), version.clone()) {
                if previous != version { return Err(BackupError::Module(format!("multiple incompatible owner versions: {}", package.manifest.id))); }
            }
        }
    }
    // Stateless packages legitimately use zero; the archive's owner-version map
    // includes only stateful modules. The approval lock below binds every package.
    modules.retain(|_, version| version.schema_version != 0);
    if !modules.contains_key("mintcat.library") { return Err(BackupError::Module("mintcat.library signed owner unavailable".into())); }
    Ok(modules)
}

fn module_lock(state: &DaemonState) -> Result<String, BackupError> {
    let updates = state.updates.as_ref().ok_or_else(|| BackupError::Module("signed module closure unavailable".into()))?;
    digest_json(&updates.environment().map_err(|e| BackupError::Invalid(e.to_string()))?)
}
fn ensure_quiescent(blockers: &BackupBlockers) -> Result<(), Box<AppError>> {
    if !blockers.active_jobs.is_empty() || !blockers.unfinished_install_jobs.is_empty() || !blockers.file_journals.is_empty() || !blockers.module_transitions.is_empty() || !blockers.nonempty_owners.is_empty() {
        return Err(error("backup", BackupError::Busy(serde_json::to_string(blockers).unwrap_or_default())));
    }
    Ok(())
}
fn cloud(state: &DaemonState, request_id: &str) -> Result<(Arc<CloudBackupTransport>, Arc<dyn CloudCredentialProvider>), Box<AppError>> {
    let transport = state.backup_cloud.clone().ok_or_else(|| prerequisite(request_id, "A deployed HTTPS Mintcat /v1/backups endpoint and signed-download origin allowlist must be configured by the daemon operator."))?;
    let credentials = state.backup_cloud_credentials.clone().ok_or_else(|| prerequisite(request_id, "Sign in to Mintcat OAuth; a daemon-owned credential vault reference is required. Application arguments cannot supply endpoint or bearer-token authority."))?;
    credentials.mintcat_credential_ref().map_err(|e| error(request_id, e))?;
    Ok((transport, credentials))
}
fn prerequisite(request_id: &str, reason: &str) -> Box<AppError> {
    Box::new(AppError { code: "CLOUD_PREREQUISITE".into(), message_key: "backup.cloud.prerequisite".into(), details: json!({"reason":reason}), request_id: request_id.into(), job_id: None, retryable: false, recovery_actions: vec![] })
}
fn requires_approval(method: &str) -> bool { matches!(method, "backup.restore" | "library.import-legacy" | "library.config-import") }
fn legacy(target: &Target) -> bool { target.method == "library.import-legacy" || (target.method == "library.config-import" && target.params["format"] == "legacy-sqlite") }
fn required_path(params: &Value, field: &str, request_id: &str) -> Result<PathBuf, Box<AppError>> {
    let path = params.get(field).and_then(Value::as_str).filter(|path| !path.is_empty()).ok_or_else(|| error(request_id, BackupError::Invalid(format!("{field} required"))))?;
    let path = PathBuf::from(path);
    if !path.is_absolute() { return Err(error(request_id, BackupError::Invalid(format!("{field} must be an explicit absolute filesystem path")))); }
    Ok(path)
}
fn digest(bytes: &[u8]) -> String { format!("sha256:{:x}", Sha256::digest(bytes)) }
fn digest_json(value: &impl Serialize) -> Result<String, BackupError> { Ok(digest(&serde_json::to_vec(value)?)) }
fn digest_file(path: &Path) -> Result<String, BackupError> {
    let mut file = File::open(path)?; let mut hash = Sha256::new(); let mut buffer = [0u8; 64 * 1024];
    loop { let size = file.read(&mut buffer)?; if size == 0 { break; } hash.update(&buffer[..size]); }
    Ok(format!("sha256:{:x}", hash.finalize()))
}
fn private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(path, fs::Permissions::from_mode(0o600))?; }
    File::open(path)?.sync_all()?;
    Ok(())
}
fn write_new(target: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let parent = target.parent().ok_or_else(|| BackupError::Invalid("target parent missing".into()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".mintcat-backup-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new(); options.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&temporary)?; file.write_all(bytes)?; file.sync_all()?; drop(file);
        fs::hard_link(&temporary, target)?;
        #[cfg(unix)] File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
struct ImportVault<'a> { credentials: &'a mintcat_runtime_broker::CredentialVault, permitted: bool }
impl LibraryVault for ImportVault<'_> {
    fn store(&self, credential: LegacyCredential<'_>) -> Result<String, String> {
        if !self.permitted || credential.owner_scope != LOCAL_OWNER_SCOPE { return Err("Human approval to import legacy credentials is required".into()); }
        self.credentials.store(SecretValue::new(credential.secret.as_bytes().to_vec()).map_err(|_| "invalid legacy credential")?)
            .map(|reference| reference.as_str().to_owned()).map_err(|_| "credential vault rejected import".into())
    }
    fn delete(&self, reference: &str) -> Result<(), String> {
        let reference: CredentialRef = serde_json::from_value(json!(reference)).map_err(|_| "invalid vault reference")?;
        self.credentials.delete(&reference).map_err(|_| "credential vault cleanup failed".into())
    }
}
fn error(request_id: &str, failure: BackupError) -> Box<AppError> {
    let (code, key, retryable) = match &failure {
        BackupError::Busy(_) => ("BUSY", "backup.busy", true), BackupError::RecoveryRequired => ("RECOVERY_REQUIRED", "backup.recovery_required", false),
        BackupError::Module(_) => ("INCOMPATIBLE_CONTRACT", "backup.owner_unavailable", false), BackupError::Invalid(_) | BackupError::Json(_) => ("INVALID_ARGUMENT", "backup.invalid", false),
        BackupError::Library(mintcat_library_service::LibraryError::LibraryNotEmpty) => ("CONFLICT", "library.not_empty", false),
        BackupError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ("CONFLICT", "backup.target_exists", false),
        BackupError::Cloud(_) => ("CLOUD_FAILURE", "backup.cloud.failed", false), _ => ("INTERNAL", "backup.failed", false),
    };
    Box::new(AppError { code: code.into(), message_key: key.into(), details: json!({"reason":failure.to_string()}), request_id: request_id.into(), job_id: None, retryable, recovery_actions: vec![] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mintcat_runtime_kernel::{RuntimeEnvironment,RuntimePaths};
    use std::{sync::mpsc,time::Duration};

    #[tokio::test]
    async fn unavailable_signed_owner_inspection_returns_error_for_empty_and_own_recovery_job() {
        let sandbox=tempfile::tempdir().unwrap();
        let paths=RuntimePaths::new(sandbox.path().join("data")).unwrap();
        let daemon=crate::BoundDaemon::bind(&paths,RuntimeEnvironment::Test).unwrap();
        let (sender,receiver)=mpsc::channel();
        let worker=std::thread::spawn(move|| {
            let empty=inspect(&daemon.state,None);
            let job=daemon.state.jobs.register_job(RegisterJob {
                command:"backup.recover".into(),owner_scope:LOCAL_OWNER_SCOPE.into(),
                idempotency_key:"inspector-recovery".into(),request_hash:request_hash(&json!({})).unwrap(),
                target:json!({}),confirmation_policy:ConfirmationPolicy::None,
            }).unwrap().job;
            let recovery=inspect(&daemon.state,Some(&job.job_id));
            let state=daemon.state.jobs.get_job(LOCAL_OWNER_SCOPE,&job.job_id).unwrap().state;
            sender.send((empty,recovery,state)).unwrap();
        });
        let (empty,recovery,state)=receiver.recv_timeout(Duration::from_secs(5))
            .expect("backup inspection must release Kernel admission before consulting its owners");
        worker.join().unwrap();
        for outcome in [empty,recovery] {
            let failure=outcome.expect_err("a fixture without signed owners must reject portable backup");
            assert!(matches!(failure,BackupError::Module(_)));
            assert_eq!(error("inspection",failure).code,"INCOMPATIBLE_CONTRACT");
        }
        assert_eq!(state,JobState::Queued,"inspection must not resume a recovery job");
    }
}
