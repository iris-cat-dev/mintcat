use std::path::PathBuf;

use super::*;
use mintcat_feature_backup::MaintenanceGate;
use mintcat_sdk::ApplicationCommand as Command;
use serde::{de::DeserializeOwned, Serialize};

type Result<T, E = Box<AppError>> = std::result::Result<T, E>;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoveryRoot {
    root_path: PathBuf,
    variant_id: String,
}

fn failure(id: &str, code: &str, key: &str, details: Value) -> Box<AppError> {
    Box::new(AppError { code: code.into(), message_key: key.into(), details, request_id: id.into(), job_id: None, retryable: false, recovery_actions: Vec::new() })
}
fn value<T: Serialize>(id: &str, value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Box::new(internal_error(id, error.to_string())))
}
fn params<T: DeserializeOwned>(request: &ValidatedRequest) -> Result<T> {
    serde_json::from_value(request.envelope.params.clone()).map_err(|error| failure(&rpc_id_string(&request.envelope.id), "INVALID_ARGUMENT", "command.params", json!({"message":error.to_string()})))
}
fn updates<'a>(state: &'a DaemonState, id: &str) -> Result<&'a UpdatesCoordinator> {
    state.updates.as_ref().ok_or_else(|| failure(id, "MODULE_UNAVAILABLE", "updates.unprovisioned", json!({"required":"configured signed game repository"})))
}
fn update_error(id: &str, error: mintcat_runtime_updates::UpdateError) -> Box<AppError> {
    use mintcat_runtime_updates::UpdateError;
    let code = match &error {
        UpdateError::Busy(_) | UpdateError::Maintenance(mintcat_feature_backup::BackupError::Busy(_)) => "RESOURCE_BUSY",
        UpdateError::ArtifactRevoked(_) => "ARTIFACT_REVOKED",
        UpdateError::MetadataExpired { .. } => "METADATA_EXPIRED",
        UpdateError::Blocked(_) => "RECOVERY_REQUIRED",
        _ => "UPDATE_FAILED",
    };
    failure(id, code, "updates.operation", json!({"message":error.to_string()}))
}
fn source(state: &DaemonState, id: &str) -> Result<SourceApplication> {
    state.sources.read().map_err(|error| Box::new(internal_error(id,error.to_string())))?.as_ref()
        .map(|source| source.application.clone()).ok_or_else(|| failure(id,"SOURCE_UNAVAILABLE","source.unprovisioned",json!({"required":"configured builtin sources"})))
}
fn source_error(id: &str, error: mintcat_source_service::SourceApplicationError) -> Box<AppError> {
    use mintcat_source_service::SourceApplicationError;
    match error {
        SourceApplicationError::Library(error) => Box::new(library_error(id,error)),
        SourceApplicationError::Job(error) => Box::new(job_error(id,error)),
        SourceApplicationError::Source(error) => {
            failure(id,error.app_code(),"source.operation",json!({"reason":error.diagnostic_reason()}))
        }
        error => {
            let code = match &error {
                SourceApplicationError::Invalid(_) => "INVALID_ARGUMENT",
                SourceApplicationError::Maintenance(_) => "RESOURCE_BUSY",
                _ => "SOURCE_UNAVAILABLE",
            };
            let reason = match error {
                SourceApplicationError::Invalid(_) => "source_configuration",
                SourceApplicationError::Maintenance(_) => "resource_busy",
                _ => "source_unavailable",
            };
            failure(id,code,"source.operation",json!({"reason":reason}))
        }
    }
}

fn source_credential_error(id: &str, error: super::sources::CredentialMutationError) -> Box<AppError> {
    use super::sources::CredentialMutationError;
    match error {
        CredentialMutationError::Empty => failure(id, "INVALID_ARGUMENT", "source.credential_empty", json!({"reason": "credential_empty"})),
        CredentialMutationError::InvalidFormat => failure(id, "INVALID_ARGUMENT", "source.credential_format", json!({"reason": "credential_invalid_format"})),
        CredentialMutationError::NotConfigured(source_id) => failure(id, "NOT_FOUND", "source.not_configured", json!({"sourceId": source_id})),
        CredentialMutationError::VaultUnavailable => failure(id, "SOURCE_UNAVAILABLE", "source.credential_vault", json!({"reason": "credential_vault_unavailable"})),
        CredentialMutationError::Unavailable => failure(id, "SOURCE_UNAVAILABLE", "source.credential_operation", json!({"reason": "source_unavailable"})),
    }
}

pub(super) async fn handle_request(state: Arc<DaemonState>, request: ValidatedRequest, principal: Principal) -> Result<Value> {
    let _source_generation = if matches!(request.descriptor.method, "sources.list" | "sources.credentials.set" | "sources.credentials.clear" | "mods.add" | "mods.importLocal" | "mods.setVersion") {
        Some(state.composition_lock.lock().await)
    } else { None };
    dispatch(state.clone(), request, principal).await
}
async fn dispatch(state: Arc<DaemonState>, request: ValidatedRequest, principal: Principal) -> Result<Value> {
    let id = rpc_id_string(&request.envelope.id);
    let method = request.descriptor.method;
    if principal == Principal::Agent && !request.descriptor.agent_exposed {
        return Err(failure(&id,"PERMISSION_DENIED","agent.command_not_exposed",json!({"method":method})));
    }
    let rescue = state.bootstrap_error.lock().clone();
    if let Some(reason) = rescue {
        let readable = request.descriptor.classification == "read";
        if !readable && !matches!(method,"backup.recover"|"updates.rollback") {
            return Err(failure(&id,"RECOVERY_REQUIRED","runtime.readonly_rescue",json!({"reason":reason,"method":method})));
        }
    }
    if let Some(result) = agent_proxy::handle(&state, &request).await {
        return result;
    }
    if method.starts_with("backup.") || matches!(method,"library.import-legacy"|"library.config-import") {
        return backup::handle(state.clone(),method,request.envelope.params.clone(),request.envelope.idempotency_key.as_deref(),&id).await;
    }
    if method.starts_with("updates.") {
        return update_command(state, &request).await;
    }
    match &request.command {
        Command::ModulesList(_) => {
            let id2 = id.clone();
            // Signed inventory hashes package bytes. Keep it off IPC workers so
            // concurrent authenticated handshakes retain their bounded deadline.
            tokio::task::spawn_blocking(move || {
                if state.updates.is_none() { return Ok(json!({"generationId":state.generation_id,"modules":[]})); }
                value(&id2, updates(&state, &id2)?.module_inventory().map_err(|error| update_error(&id2, error))?)
            }).await.map_err(|error| Box::new(internal_error(&id, error.to_string())))?
        }
        Command::ModulesInspect(input) => {
            let coordinator=updates(&state,&id)?;
            let environment=coordinator.environment().map_err(|error|update_error(&id,error))?;
            let record=environment.scopes.values().flat_map(|scope|scope.targets.signed.packages.values())
                .find(|record|record.manifest.id==input.module_id)
                .ok_or_else(||failure(&id,"NOT_FOUND","modules.not_found",json!({"moduleId":input.module_id})))?;
            value(&id,mintcat_sdk::ModuleInspectResult {manifest:record.manifest.clone(),status:"active".into(),reason:None})
        }
        Command::SourcesList(_) => super::sources::list(&state).map_err(|message|failure(&id,"SOURCE_UNAVAILABLE","source.configuration",json!({"message":message}))),
        Command::SourcesCredentialsSet(input) => {
            let source_id = input.source_id.clone();
            let secret = super::sources::parse_secret_input(&input.secret).map_err(|error| source_credential_error(&id, error))?;
            let id2 = id.clone();
            tokio::task::spawn_blocking(move || {
                let _admission = MaintenanceGate::enter_mutation(&state.layout).map_err(|error| failure(&id2, "RESOURCE_BUSY", "runtime.maintenance", json!({"message":error.to_string()})))?;
                let result = super::sources::set_credential(&state, &source_id, secret).map_err(|error| source_credential_error(&id2, error))?;
                value(&id2, result)
            }).await.map_err(|error| Box::new(internal_error(&id, error.to_string())))?
        }
        Command::SourcesCredentialsClear(input) => {
            let source_id = input.source_id.clone();
            let id2 = id.clone();
            tokio::task::spawn_blocking(move || {
                let _admission = MaintenanceGate::enter_mutation(&state.layout).map_err(|error| failure(&id2, "RESOURCE_BUSY", "runtime.maintenance", json!({"message":error.to_string()})))?;
                let result = super::sources::clear_credential(&state, &source_id).map_err(|error| source_credential_error(&id2, error))?;
                value(&id2, result)
            }).await.map_err(|error| Box::new(internal_error(&id, error.to_string())))?
        }
        Command::GamesRegister(input) => {
            let input=input.clone(); let id2=id.clone(); let key=request.envelope.idempotency_key.clone().expect("validated mutation key");
            let adapter=state.install.catalog().current(&input.game_id).map_err(|error|Box::new(mintcat_install::install_app_error(&id,&error)))?;
            tokio::task::spawn_blocking(move || {
                let _admission=MaintenanceGate::enter_mutation(&state.layout).map_err(|error|failure(&id2,"RESOURCE_BUSY","runtime.maintenance",json!({"message":error.to_string()})))?;
                let installation=adapter.validate_selected_installation(Path::new(&input.root_path),&input.variant_id).map_err(|message|failure(&id2,"INVALID_ARGUMENT","game.installation",json!({"message":message})))?;
                library_value(&id2,state.library.register_game(&state.library_authorization,&key,&mintcat_library_service::GameEnrollmentRequest {signed_descriptor:adapter.descriptor().clone(),installation:Some(installation)}))
            }).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::GamesDiscover(input) => {
            let input=input.clone(); let id2=id.clone();
            let adapter=state.install.catalog().current(&input.game_id).map_err(|error|Box::new(mintcat_install::install_app_error(&id,&error)))?;
            tokio::task::spawn_blocking(move || {
                let roots = input.roots.unwrap_or_default().into_iter()
                    .map(serde_json::from_value::<DiscoveryRoot>)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|error| failure(&id2, "INVALID_ARGUMENT", "game.discovery_roots", json!({"message":error.to_string()})))?
                    .into_iter().map(|hint| (hint.root_path, hint.variant_id)).collect::<Vec<_>>();
                let items=adapter.discover_installations(&roots,input.include_steam.unwrap_or(false)).map_err(|message|failure(&id2,"INVALID_ARGUMENT","game.discovery",json!({"message":message})))?;
                value(&id2,json!({"items":items}))
            }).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::SourcesSearch(input) => {
            let application = {
                let _source_generation = state.composition_lock.lock().await;
                source(&state, &id)?
            };
            let result = application
                .search(
                    &mintcat_source_service::SourceSearchRequest {
                        source_id: input.source_id.clone(),
                        game_id: input.game_id.clone(),
                        remote_game_key: input.remote_game_key.clone(),
                        query: input.query.clone(),
                        cursor: input.cursor.clone(),
                        page_size: input.page_size.unwrap_or(20) as u32,
                    },
                    &mintcat_runtime_broker::CancellationToken::default(),
                )
                .await
                .map_err(|error| source_error(&id, error))?;
            value(&id, result)
        }
        Command::ModsAdd(_) => {
            let application=source(&state,&id)?; let input:mintcat_library_service::SourceAddRequest=params(&request)?;
            let key=request.envelope.idempotency_key.clone().expect("validated mutation key"); let id2=id.clone();
            tokio::task::spawn_blocking(move || tokio::runtime::Handle::current().block_on(async {value(&id2,application.add(&key,input).await.map_err(|error|source_error(&id2,error))?)})).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::ModsImportLocal(_) => {
            let application=source(&state,&id)?; let input:mintcat_source_service::ImportLocalRequest=params(&request)?;
            let key=request.envelope.idempotency_key.clone().expect("validated mutation key"); let id2=id.clone();
            tokio::task::spawn_blocking(move || value(&id2,application.import_local(&key,input).map_err(|error|source_error(&id2,error))?)).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::SettingsGet(input) => {
            let input=input.clone(); let id2=id.clone();
            tokio::task::spawn_blocking(move || library_value(&id2,state.library.get_settings(&state.library_authorization,&input))).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::SettingsUpdate(input) => {
            let input=input.clone(); let key=request.envelope.idempotency_key.clone().expect("validated mutation key"); let id2=id.clone();
            let scope=serde_json::to_value(&input.scope).map_err(|error|Box::new(internal_error(&id,error.to_string())))?;
            let schema=if scope["kind"]=="game" && input.section=="game" {
                let game=scope["gameId"].as_str().ok_or_else(||failure(&id,"INVALID_ARGUMENT","settings.game",json!({})))?;
                Some(state.install.catalog().current(game).map_err(|error|Box::new(mintcat_install::install_app_error(&id,&error)))?.descriptor().settings_schema.clone())
            } else {None};
            tokio::task::spawn_blocking(move || {
                let _admission=MaintenanceGate::enter_mutation(&state.layout).map_err(|error|failure(&id2,"RESOURCE_BUSY","runtime.maintenance",json!({"message":error.to_string()})))?;
                library_value(&id2,state.library.update_settings(&state.library_authorization,&key,&input,schema.as_ref()))
            }).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::InstallPlan(_) | Command::ModsPack(_) | Command::InstallApply(_) | Command::InstallUninstall(_) => {
            let method=method.to_owned(); let key=request.envelope.idempotency_key.clone().expect("validated mutation key"); let id2=id.clone();
            tokio::task::spawn_blocking(move || {
                let _admission=MaintenanceGate::enter_mutation(&state.layout).map_err(|error|failure(&id2,"RESOURCE_BUSY","runtime.maintenance",json!({"message":error.to_string()})))?;
                let job=match method.as_str() {
                    "install.plan" => state.install.submit_build(params(&request)?,false,&key),
                    "mods.pack" => state.install.submit_build(params(&request)?,true,&key),
                    "install.apply" => state.install.submit_apply(params(&request)?,&key),
                    "install.uninstall" => state.install.submit_uninstall(params(&request)?,&key),
                    _ => unreachable!(),
                }.map_err(|error|Box::new(mintcat_install::install_app_error(&id2,&error)))?;
                value(&id2,job)
            }).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
        Command::ActionsApprove(_) | Command::ActionsReject(_) | Command::JobsCancel(_) => action_command(state,request).await,
        _ => {
            let mutating=request.descriptor.classification=="mutation";
            let id2=id.clone();
            tokio::task::spawn_blocking(move || {
                let _admission=if mutating {Some(MaintenanceGate::enter_mutation(&state.layout).map_err(|error|failure(&id2,"RESOURCE_BUSY","runtime.maintenance",json!({"message":error.to_string()})))?)}else{None};
                handle_core_request(&state,request)
            }).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        }
    }
}

async fn action_command(state:Arc<DaemonState>,request:ValidatedRequest)->Result<Value> {
    let id=rpc_id_string(&request.envelope.id);
    if let Command::ActionsApprove(input)=&request.command {
        let action=state.jobs.get_action(LOCAL_OWNER_SCOPE,&input.action_id).map_err(|error|Box::new(job_error(&id,error)))?;
        let key=request.envelope.idempotency_key.as_deref().expect("validated mutation key");
        if matches!(action.kind.as_str(),"select-file"|"select-directory") {
            let path=input.response.get("selectedPath").and_then(Value::as_str).ok_or_else(||failure(&id,"INVALID_ARGUMENT","interaction.selected_path",json!({"required":"selectedPath"})))?;
            let input=input.clone(); let path=PathBuf::from(path); let state2=state.clone(); let key=key.to_owned(); let id2=id.clone();
            tokio::task::spawn_blocking(move || {
                let job=state2.jobs.action_job(LOCAL_OWNER_SCOPE,&input.action_id).map_err(|error|Box::new(job_error(&id2,error)))?;
                let generation=sources::for_job(&state2,&job.job_id).map_err(|message| failure(&id2,"MODULE_UNAVAILABLE","source.frozen_generation",json!({"message":message})))?;
                let result=if action.kind=="select-file" {state2.interaction_host.complete_file_selection(LOCAL_OWNER_SCOPE,&input.action_id,input.expected_revision,&path,&key)}else{state2.interaction_host.complete_directory_selection(LOCAL_OWNER_SCOPE,&input.action_id,input.expected_revision,&path,&key)};
                result.map_err(|error|failure(&id2,"INVALID_ARGUMENT","interaction.selection",json!({"message":error.to_string()})))?;
                let approved=state2.jobs.get_action(LOCAL_OWNER_SCOPE,&input.action_id).map_err(|error|Box::new(job_error(&id2,error)))?;
                generation.application.continue_local_import(&job.job_id,&approved).map_err(|error|source_error(&id2,error))?;
                value(&id2,job)
            }).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))?
        } else {
            let state2=state.clone(); let request2=request.clone();
            let result=tokio::task::spawn_blocking(move ||handle_core_request(&state2,request2)).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))??;
            let approved=state.jobs.get_action(LOCAL_OWNER_SCOPE,&input.action_id).map_err(|error|Box::new(job_error(&id,error)))?;
            if backup::owns_action(&approved) {backup::resume_action(state.clone(),&approved)?;}
            state.install.resume_action(&approved).map_err(|error|Box::new(mintcat_install::install_app_error(&id,&error)))?;
            state.features.resume_action(&input.action_id).await?;
            Ok(result)
        }
    } else {
        let job_id=match &request.command {
            Command::JobsCancel(input)=>Some(input.job_id.clone()),
            Command::ActionsReject(input)=>Some(state.jobs.action_job(LOCAL_OWNER_SCOPE,&input.action_id).map_err(|error|Box::new(job_error(&id,error)))?.job_id),
            _=>None,
        };
        let state2=state.clone();
        let result=tokio::task::spawn_blocking(move ||handle_core_request(&state2,request)).await.map_err(|error|Box::new(internal_error(&id,error.to_string())))??;
        if let Some(job_id)=job_id {
            let snapshot = state.jobs.get_job(LOCAL_OWNER_SCOPE, &job_id).map_err(|error| Box::new(job_error(&id, error)))?;
            if matches!(snapshot.command.as_str(), "install.plan" | "mods.pack" | "install.apply" | "install.uninstall") {
                state.install.cancel(&job_id).map_err(|error|Box::new(mintcat_install::install_app_error(&id,&error)))?;
            }
            state.features.cancel(&job_id).await?;
            backup::release_terminal_lease(&state,&job_id)?;
        }
        Ok(result)
    }
}

async fn update_command(state:Arc<DaemonState>,request:&ValidatedRequest)->Result<Value> {
    let id=rpc_id_string(&request.envelope.id);
    let coordinator=updates(&state,&id)?.clone();
    if request.descriptor.method=="updates.operations" {
        let (recovery_reason,scope_blockers)=coordinator.rescue_status().map_err(|error|update_error(&id,error))?;
        return value(&id,json!({"operations":coordinator.operations().map_err(|error|update_error(&id,error))?,"recoveryReason":recovery_reason,"scopeBlockers":scope_blockers}));
    }
    if request.descriptor.method=="updates.gc" {
        let _serial=state.composition_lock.lock().await;
        sources::collect_obsolete(&state).await.map_err(|message|failure(&id,"RECOVERY_REQUIRED","source.collection",json!({"message":message})))?;
        let now = unix_seconds().map_err(|error| Box::new(internal_error(&id, error.to_string())))?;
        let state2 = state.clone();
        let retained = tokio::task::spawn_blocking(move || -> std::result::Result<_, String> {
            let mut retained = std::collections::HashSet::new();
            retained.extend(state2.library.retained_artifact_digests(&state2.library_authorization).map_err(|error| error.to_string())?);
            retained.extend(state2.transfers.retained_artifact_digests().map_err(|error| error.to_string())?);
            let now_ms = now.checked_mul(1_000).and_then(|value| i64::try_from(value).ok()).ok_or("GC timestamp overflows milliseconds")?;
            retained.extend(state2.install.service().retained_artifact_digests(now_ms).map_err(|error| error.to_string())?);
            Ok(retained)
        })
            .await.map_err(|error| Box::new(internal_error(&id, error.to_string())))?
            .map_err(|message| Box::new(internal_error(&id, message)))?;
        return value(&id, coordinator.collect_garbage(now, &retained).await.map_err(|error| update_error(&id, error))?);
    }
    let key=request.envelope.idempotency_key.as_deref().expect("validated mutation key");
    let hash=request_hash(&request.envelope.params).map_err(|error|Box::new(job_error(&id,error)))?;
    let job=state.jobs.register_job(RegisterJob {command:request.descriptor.method.into(),owner_scope:LOCAL_OWNER_SCOPE.into(),idempotency_key:key.into(),request_hash:hash,target:request.envelope.params.clone(),confirmation_policy:if request.descriptor.confirmation=="required"{ConfirmationPolicy::Required}else{ConfirmationPolicy::None}}).map_err(|error|Box::new(job_error(&id,error)))?;
    if job.created {
        let state=state.clone(); let request=request.clone(); let job_id=job.job.job_id.clone();
        // Lifecycle callbacks verify signed bytes and may wait on native owners.
        // Keep their synchronous work off the authenticated IPC executor.
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || runtime.block_on(async move {
            let _serial = state.composition_lock.lock().await;
            let result:Result<Value>=async {
                state.jobs.append_event(&job_id,mintcat_runtime_jobs::EventUpdate::state("update-preparing",JobState::Preparing)).map_err(|error|Box::new(job_error(&job_id,error)))?;
                state.jobs.append_event(&job_id,mintcat_runtime_jobs::EventUpdate::state("update-running",JobState::Running)).map_err(|error|Box::new(job_error(&job_id,error)))?;
                let now=unix_seconds().map_err(|error|Box::new(internal_error(&job_id,error.to_string())))?;
                let result=match &request.command {
                    Command::UpdatesStage(input)=> {
                        if let Some(path)=&input.repository_path {
                            let repository=mintcat_runtime_release::LocalRepository::open(Path::new(path)).map_err(|error|failure(&job_id,"UPDATE_FAILED","updates.repository",json!({"message":error.to_string()})))?;
                            value(&job_id,coordinator.stage_local(&repository,&input.scope_id,&input.channel,input.expected_scope_revision,now).await.map_err(|error|update_error(&job_id,error))?)?
                        } else if let Some(url)=&input.repository_url {
                            let base = url::Url::parse(url).map_err(|error| failure(&job_id,"INVALID_ARGUMENT","updates.repository_url",json!({"message":error.to_string()})))?;
                            let origins = vec![mintcat_runtime_broker::Origin::from_url(&base).map_err(|error| failure(&job_id,"INVALID_ARGUMENT","updates.repository_origin",json!({"message":error.to_string()})))?];
                            let bundle = coordinator.fetch_repository(&state.network_broker,&base,origins.clone()).await.map_err(|error|update_error(&job_id,error))?;
                            value(&job_id,coordinator.stage_network(bundle,&base,origins,&state.transfers,&input.scope_id,&input.channel,input.expected_scope_revision,now).await.map_err(|error|update_error(&job_id,error))?)?
                        } else {return Err(failure(&job_id,"INVALID_ARGUMENT","updates.repository_required",json!({"required":"repositoryPath or repositoryUrl"})));}
                    },
                    Command::UpdatesPrepare(input)=>value(&job_id,coordinator.prepare(&input.operation_id,now).await.map_err(|error|update_error(&job_id,error))?)?,
                    Command::UpdatesActivate(input)=>value(&job_id,coordinator.activate(&input.operation_id,now).map_err(|error|update_error(&job_id,error))?)?,
                    Command::UpdatesConfirm(input)=>value(&job_id,coordinator.confirm(&input.operation_id,now).await.map_err(|error|update_error(&job_id,error))?)?,
                    Command::UpdatesRollback(input)=>value(&job_id,coordinator.rollback(&input.operation_id,input.reason.as_deref().unwrap_or("explicit human rollback")).await.map_err(|error|update_error(&job_id,error))?)?,
                    Command::UpdatesChannel(input)=>{coordinator.authorize_channel(&input.scope_id,&input.channel,input.expected_scope_revision).map_err(|error|update_error(&job_id,error))?;json!({"scopeId":input.scope_id,"channel":input.channel})},
                    _=>return Err(failure(&job_id,"INVALID_ARGUMENT","updates.unsupported",json!({"method":request.descriptor.method}))),
                };
                composition::refresh(&state).await.map_err(|message|failure(&job_id,"RECOVERY_REQUIRED","runtime.composition",json!({"message":message})))?;
                Ok(result)
            }.await;
            let mut event=mintcat_runtime_jobs::EventUpdate::state(if result.is_ok(){"update-complete"}else{"update-failed"},if result.is_ok(){JobState::Succeeded}else{JobState::Failed});
            match result {Ok(result)=>event.result=Some(result),Err(error)=>event.error=Some(*error)};
            if let Err(error)=state.jobs.append_event(&job_id,event) {eprintln!("update terminal persistence failed for {job_id}: {error}");}
        }));
    }
    value(&id,job.job)
}

#[cfg(test)]
mod source_error_tests {
    use super::*;
    use mintcat_source_api::SourceError;
    use mintcat_source_service::SourceApplicationError;

    #[test]
    fn source_rpc_preserves_category_safe_reason_and_request_id() {
        for (error,code,reason) in [
            (SourceError::Unauthorized,"SOURCE_UNAUTHORIZED","credential_missing_or_invalid"),
            (SourceError::InvalidArgument("private configuration".into()),"INVALID_ARGUMENT","source_configuration"),
            (SourceError::RateLimited,"SOURCE_RATE_LIMITED","source_rate_limited"),
            (SourceError::InvalidResponse("private response".into()),"SOURCE_INVALID_RESPONSE","source_response"),
            (SourceError::Network(mintcat_runtime_broker::BrokerError::Transport {
                request_id:"private-network-request".into(),
                endpoint:"https://example.invalid/?token=private-value".into(),
            }),"SOURCE_UNAVAILABLE","source_network"),
            (SourceError::Network(mintcat_runtime_broker::BrokerError::CredentialStore),"SOURCE_UNAVAILABLE","credential_vault_unavailable"),
        ] {
            let error = source_error("request-source-test",SourceApplicationError::Source(error));
            assert_eq!(error.code,code);
            assert_eq!(error.message_key,"source.operation");
            assert_eq!(error.request_id,"request-source-test");
            assert_eq!(error.details,json!({"reason":reason}));
            assert!(error.job_id.is_none());
        }
    }
}
