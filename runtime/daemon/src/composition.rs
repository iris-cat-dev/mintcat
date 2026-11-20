use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};

use mintcat_feature_backup::{BackupError, CloudBackupTransport, CloudCredentialProvider};
use mintcat_install::{ModuleClosure, ModuleClosureEntry};
use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_broker::{CredentialRef, Origin};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeLayout};
use mintcat_runtime_release::{LocalRepository, RepositoryVerifier, VerificationPolicy};
use mintcat_runtime_updates::{EnvironmentSnapshot, UpdatesCoordinator};
use mintcat_source_api::SourceConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{DaemonState, LOCAL_OWNER_SCOPE};

type Result<T, E = String> = std::result::Result<T, E>;
fn message(error: impl std::fmt::Display) -> String { error.to_string() }

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ScopePin {
    pub release_id: String,
    pub sequence: u64,
    pub channel: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RuntimeConfiguration {
    schema_version: u64,
    #[serde(default)] trust_root: PathBuf,
    #[serde(default)] verification_policy: PathBuf,
    #[serde(default)] pub repository_path: PathBuf,
    #[serde(default)] scopes: BTreeMap<String, ScopePin>,
    #[serde(default)] sources: BTreeMap<String, SourceConfig>,
    #[serde(default)] cloud: Option<CloudConfiguration>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CloudConfiguration {
    origin: String,
    credential_ref: CredentialRef,
    #[serde(default)] download_origins: Vec<String>,
}
struct CloudCredential(CredentialRef);
impl CloudCredentialProvider for CloudCredential {
    fn mintcat_credential_ref(&self) -> std::result::Result<CredentialRef, BackupError> { Ok(self.0.clone()) }
}

impl RuntimeConfiguration {
    pub fn load(layout: &RuntimeLayout) -> Result<Option<Self>> {
        let explicit = std::env::var_os("MINTCAT_RUNTIME_CONFIG").map(PathBuf::from);
        let provisioned = layout.state_dir.join("trust/runtime.json");
        let path = match explicit {
            Some(path) => path,
            None if provisioned.exists() => provisioned,
            None => {
                let executable = std::env::current_exe().map_err(message)?;
                let Some(directory) = mintcat_runtime_kernel::packaged_resource_directory(&executable).map_err(message)? else { return Ok(None); };
                directory.join("runtime.json")
            },
        };
        let mut configuration: Self = mintcat_runtime_release::read_json(&path).map_err(message)?;
        if !matches!(configuration.schema_version, 1 | 2) {
            return Err("runtime configuration requires schema1 (historical) or schema2".into());
        }
        if configuration.schema_version == 2 && configuration.scopes.keys().any(|scope| !scope.starts_with("game:")) {
            return Err("schema2 runtime configuration admits only independent game scopes".into());
        }
        if !configuration.scopes.is_empty() && [&configuration.trust_root, &configuration.verification_policy, &configuration.repository_path].iter().any(|path| path.as_os_str().is_empty()) {
            return Err("configured game releases require trustRoot, verificationPolicy and repositoryPath".into());
        }
        let base = path.parent().unwrap_or(Path::new("."));
        for value in [&mut configuration.trust_root, &mut configuration.verification_policy, &mut configuration.repository_path] {
            if !value.as_os_str().is_empty() && value.is_relative() { *value = base.join(&*value); }
        }
        Ok(Some(configuration))
    }
    pub fn has_game_runtime(&self) -> bool { !self.trust_root.as_os_str().is_empty() }
    pub fn configured_sources(&self) -> &BTreeMap<String, SourceConfig> { &self.sources }
    pub fn trust(&self) -> Result<TrustStore> {
        if !self.has_game_runtime() { return Ok(TrustStore::default()); }
        mintcat_runtime_release::load_root_rotation_anchor(&self.trust_root).map_err(message)?.artifact_trust_store().map_err(message)
    }
    pub fn updates(&self, layout: &RuntimeLayout, database: KernelDatabase, artifacts: ArtifactStore) -> Result<UpdatesCoordinator> {
        let policy: VerificationPolicy = mintcat_runtime_release::read_json(&self.verification_policy).map_err(message)?;
        if policy.target != target_lexicon::HOST.to_string() || policy.host_api != "1" {
            return Err("runtime policy must match this daemon's exact compiled target and host API".into());
        }
        let verifier = RepositoryVerifier::from_pinned_file(layout.state_dir.join("trust/metadata-state.json"), &self.trust_root).map_err(message)?;
        let identity = layout.state_dir.join("installation-id");
        let installation_id = if identity.exists() {
            let value = std::fs::read_to_string(&identity).map_err(message)?;
            uuid::Uuid::parse_str(&value).map_err(message)?;
            value
        } else {
            use std::io::Write;
            let value = uuid::Uuid::new_v4().to_string();
            let mut options = std::fs::OpenOptions::new(); options.create_new(true).write(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            let mut file = options.open(&identity).map_err(message)?;
            file.write_all(value.as_bytes()).map_err(message)?; file.sync_all().map_err(message)?;
            value
        };
        UpdatesCoordinator::open(layout.clone(), database, artifacts, verifier, policy, installation_id).map_err(message)
    }
    pub fn cloud(&self, broker: mintcat_runtime_broker::NetworkBroker) -> Result<Option<(Arc<CloudBackupTransport>, Arc<dyn CloudCredentialProvider>)>> {
        self.cloud.as_ref().map(|cloud| {
            let origins = cloud.download_origins.iter().map(|origin| Origin::parse(origin).map_err(message)).collect::<Result<Vec<_>>>()?;
            let transport = CloudBackupTransport::new(&cloud.origin, broker, origins).map_err(message)?;
            Ok((Arc::new(transport), Arc::new(CloudCredential(cloud.credential_ref.clone())) as Arc<dyn CloudCredentialProvider>))
        }).transpose()
    }
}


pub(super) async fn bootstrap(state: &Arc<DaemonState>) -> Result<()> {
    if let Some(updates) = &state.updates { updates.recover_startup_readonly().await.map_err(message)?; }
    let cloned = state.clone();
    if let Err(error) = tokio::task::spawn_blocking(move || super::backup::startup_recover(&cloned)).await.map_err(message)? {
        state.backup_startup_blocked.store(error.code == "RECOVERY_REQUIRED", std::sync::atomic::Ordering::Release);
        return Err(format!("{}: {}", error.code, error.message_key));
    }
    state.library.resume_after_recovery().map_err(message)?;
    if let (Some(updates), Some(configuration)) = (&state.updates, &state.configuration) {
        updates.finish_startup_recovery().await.map_err(message)?;
        let environment = updates.environment().map_err(message)?;
        let scopes: Vec<_> = configuration.scopes.iter()
            .filter(|(scope, _)| scope.starts_with("game:") && !environment.scopes.contains_key(*scope)).collect();
        if !scopes.is_empty() {
            let repository = LocalRepository::open(&configuration.repository_path).map_err(message)?;
            for (scope, pin) in scopes {
                let now = super::unix_seconds().map_err(message)?;
                let revision = updates.scope_revision(scope).map_err(message)?;
                updates.authorize_channel(scope, &pin.channel, revision).map_err(message)?;
                let operation = updates.stage_local(&repository, scope, &pin.channel, revision, now).await.map_err(message)?;
                if operation.generation.targets.signed.release.release_id != pin.release_id || operation.generation.targets.signed.release.sequence != pin.sequence {
                    return Err(format!("game scope {scope} differs from its provisioned exact release pin"));
                }
                updates.prepare(&operation.operation_id, now).await.map_err(message)?;
                updates.activate(&operation.operation_id, now).map_err(message)?;
                updates.confirm(&operation.operation_id, now).await.map_err(message)?;
            }
        }
    }
    refresh(state).await?;
    let cloned = state.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        cloned.library.recover_download_operations(&cloned.generation_id).map_err(message)?;
        super::replay_library_outbox(&cloned.library, &cloned.jobs).map_err(message)?;
        Ok(())
    }).await.map_err(message)??;
    restore_install_closures(state).await?;
    state.install.recover(&state.recovered).await.map_err(message)?;
    super::sources::restore(state).await?;
    super::sources::collect_obsolete(state).await?;
    state.features.recover(&state.recovered).map_err(|error| format!("{}: {}", error.code, error.message_key))?;
    if let Some(updates) = &state.updates { updates.finish_lease_reconciliation().map_err(message)?; }
    Ok(())
}

/// Explicit backup recovery may resume only the startup that its marker blocked.
/// An unrelated signature/provider/bootstrap failure must never become "ready".
pub(super) async fn resume_after_backup_recovery(state: &Arc<DaemonState>) -> Result<()> {
    use std::sync::atomic::Ordering;
    if !state.backup_startup_blocked.load(Ordering::Acquire) { return Ok(()); }
    let _serial = state.composition_lock.lock().await;
    if !state.backup_startup_blocked.load(Ordering::Acquire) { return Ok(()); }
    state.backup_startup_blocked.store(false, Ordering::Release);
    let result: Result<()> = async {
        bootstrap(state).await?;
        state.features.resume_recovered().await.map_err(|error| format!("{}: {}", error.code, error.message_key))?;
        resume_downloads(state)?;
        Ok(())
    }.await;
    if let Err(error) = result {
        *state.bootstrap_error.lock() = Some(error.clone());
        return Err(error);
    }
    *state.bootstrap_error.lock() = None;
    Ok(())
}

// Callers serialize refresh against application requests and scope publication.
pub(super) async fn refresh(state: &Arc<DaemonState>) -> Result<()> {
    let environment = match &state.updates {
        Some(updates) => updates.environment().map_err(message)?,
        None => EnvironmentSnapshot { generation_id: "builtin".into(), scopes: BTreeMap::new() },
    };
    let current = state.sources.read().map_err(message)?.as_ref().is_some_and(|generation| {
        generation.environment_generation == environment.generation_id
            && generation.scope_confirmations.iter().copied().eq(environment.scopes.values().map(|scope| scope.confirmed))
    });
    if current { return Ok(()); }
    let mut descriptors = Vec::new();
    let mut adapters = Vec::new();
    let active_games: std::collections::BTreeSet<_> = state.updates.as_ref()
        .map(|updates| updates.module_inventory().map(|inventory| inventory.modules.into_iter().map(|module| module.digest).collect()))
        .transpose().map_err(message)?.unwrap_or_default();
    for (scope, generation) in &environment.scopes {
        if !scope.starts_with("game:") || !generation.confirmed { continue; }
        let updates = state.updates.as_ref().ok_or("game scope has no update authority")?;
        let Some(supervisor) = updates.supervisor_for_generation(scope, &generation.generation_id).map_err(message)? else { continue; };
        let closure = ModuleClosure::new(generation.targets.signed.packages.values().map(|record| ModuleClosureEntry {
            module_id: record.manifest.id.clone(), version: record.manifest.version.clone(), package_digest: record.digest.clone(),
        }).collect()).map_err(message)?;
        for record in generation.targets.signed.packages.values() {
            if active_games.contains(&record.digest) && record.manifest.provides.iter().any(|contract| contract.contract_id == mintcat_game_api::GAME_ADAPTER_CONTRACT_ID) {
                let games: Vec<mintcat_game_api::GameDescriptor> = serde_json::from_value(record.manifest.contributes["games"].clone()).map_err(message)?;
                for descriptor in games {
                    let adapter = state.install.catalog().pin_using(supervisor.clone(), &descriptor.game_id, &record.digest, closure.clone()).await.map_err(message)?;
                    adapters.push(adapter);
                    descriptors.push(descriptor);
                }
            }
        }
    }
    state.install.catalog().publish(adapters);
    let base = state.configuration.as_ref().map(|configuration| configuration.sources.clone()).unwrap_or_default();
    let configuration = super::sources::merge_credential_overrides(state, base)?;
    super::sources::publish(state, environment, descriptors, configuration).await?;
    Ok(())
}

async fn restore_install_closures(state: &Arc<DaemonState>) -> Result<()> {
    let mut requests = state.install.service().active_receipt_build_requests().map_err(message)?;
    for record in &state.recovered {
        let target = state.jobs.get_job_target(LOCAL_OWNER_SCOPE, &record.snapshot.job_id).map_err(message)?;
        if let Some(request) = target.get("request").filter(|_| matches!(record.snapshot.command.as_str(), "install.plan" | "mods.pack")) {
            requests.push(serde_json::from_value(request.clone()).map_err(message)?);
        } else if record.snapshot.command == "install.apply" {
            let plan = target["params"]["planId"].as_str().ok_or("persisted apply target has no planId")?;
            requests.push(state.install.service().recovery_build_request(plan).map_err(message)?);
        } else if record.snapshot.command == "install.uninstall" {
            let receipt = target["request"]["receiptId"].as_str().ok_or("persisted uninstall target has no receiptId")?;
            requests.push(state.install.service().recovery_build_request_for_receipt(receipt).map_err(message)?);
        }
    }
    if requests.is_empty() { return Ok(()); }
    let updates = state.updates.as_ref().ok_or("durable install recovery requires its original signed package authority; provision the historical trust configuration")?;
    for request in requests {
        request.module_closure.verify().map_err(message)?;
        let digests: Vec<_> = request.module_closure.entries.iter().map(|entry| entry.package_digest.clone()).collect();
        let store = updates.artifact_store_for_digests(&digests).map_err(message)?;
        let mut game = None;
        for entry in &request.module_closure.entries {
            let package = store.acquire_module(&entry.package_digest).map_err(message)?;
            if package.module().manifest.id != entry.module_id || package.module().manifest.version != entry.version {
                return Err("durable install closure identity differs from its exact signed bytes".into());
            }
            if package.module().manifest.provides.iter().any(|contract| contract.contract_id == mintcat_game_api::GAME_ADAPTER_CONTRACT_ID) {
                if game.replace(entry.package_digest.clone()).is_some() { return Err("durable install closure has multiple game adapters".into()); }
            }
        }
        let digest = game.ok_or("durable install closure has no game adapter")?;
        let supervisor = mintcat_runtime_supervisor::ModuleSupervisor::new(store, &state.generation_id, &state.layout.runtime_dir).map_err(message)?;
        state.install.catalog().pin_using(supervisor, &request.settings.game_id, &digest, request.module_closure.clone()).await.map_err(message)?;
    }
    Ok(())
}

pub(super) fn resume_downloads(state: &Arc<DaemonState>) -> Result<()> {
    for record in &state.recovered {
        if record.snapshot.command != "library.download.apply" { continue; }
        let job = state.jobs.get_job(LOCAL_OWNER_SCOPE, &record.snapshot.job_id).map_err(message)?;
        if matches!(job.state, mintcat_sdk::JobState::Succeeded | mintcat_sdk::JobState::Failed | mintcat_sdk::JobState::Cancelled) { continue; }
        let downloads = match super::sources::for_job(state, &job.job_id) {
            Ok(source) => source.downloads,
            Err(_) => continue,
        };
        let target = state.jobs.get_job_target(LOCAL_OWNER_SCOPE, &job.job_id).map_err(message)?;
        let operation = target.get("operationId").and_then(Value::as_str).ok_or("download recovery requires its original operation")?.to_owned();
        let continuation = if job.state == mintcat_sdk::JobState::AwaitingApproval {
            let action = state.jobs.get_job_action(LOCAL_OWNER_SCOPE, &job.job_id).map_err(message)?.ok_or("download action missing")?;
            if action.state == mintcat_sdk::ActionState::Pending { continue; }
            Some(action)
        } else { None };
        super::spawn_download(downloads, job.job_id, operation, continuation);
    }
    Ok(())
}
