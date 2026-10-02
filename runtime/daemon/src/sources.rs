use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
use mintcat_game_api::{canonical_digest, GameDescriptor};
use mintcat_runtime_downloads::{DownloadCatalog, DownloadCoordinator};
use mintcat_runtime_jobs::EventUpdate;
use mintcat_runtime_updates::EnvironmentSnapshot;
use mintcat_source_api::SourceWorkerConfig;
use mintcat_source_local::{LocalSource, LocalImportLimits};
use mintcat_source_service::{NativeSourceProvider, SourceApplication, SourceNetworkHost, SourceNetworkPolicy, SourcePackageBinding};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use super::{DaemonState, SourceGeneration, LOCAL_OWNER_SCOPE};

type Result<T> = std::result::Result<T, String>;
fn message(error: impl std::fmt::Display) -> String { error.to_string() }
const MIGRATION: &str = "CREATE TABLE source_generations (digest TEXT PRIMARY KEY, snapshot_json TEXT NOT NULL);";
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct FrozenSources {
    schema_version: u64,
    environment: EnvironmentSnapshot,
    configuration: BTreeMap<String, SourceWorkerConfig>,
    games: Vec<GameDescriptor>,
}

pub(super) fn current_lock(state: &DaemonState) -> Result<String> {
    state.sources.read().map_err(message)?.as_ref().map(|source| source.lock_digest.clone())
        .ok_or_else(|| "signed source generation unavailable".into())
}
pub(super) fn for_job(state: &DaemonState, job_id: &str) -> Result<SourceGeneration> {
    let target = state.jobs.get_job_target(LOCAL_OWNER_SCOPE, job_id).map_err(message)?;
    let digest = target.get("moduleLockDigest").and_then(Value::as_str)
        .ok_or("source job has no frozen module lock")?;
    state.source_generations.read().map_err(message)?.get(digest).cloned()
        .ok_or_else(|| format!("source generation {digest} is unavailable; current code cannot replace it"))
}
fn store(state: &DaemonState, frozen: &FrozenSources) -> Result<String> {
    let digest = canonical_digest(frozen).map_err(message)?;
    let encoded = serde_json::to_string(frozen).map_err(message)?;
    let database = state.jobs.database();
    database.apply_migration("runtime.sources", 1, MIGRATION).map_err(message)?;
    let connection = database.connection().map_err(message)?;
    connection.execute("INSERT INTO source_generations(digest,snapshot_json) VALUES (?1,?2) ON CONFLICT(digest) DO NOTHING", params![digest, encoded]).map_err(message)?;
    let stored: String = connection.query_row("SELECT snapshot_json FROM source_generations WHERE digest=?1", [&digest], |row| row.get(0)).map_err(message)?;
    if stored != encoded { return Err("source lock was reused for different immutable inputs".into()); }
    Ok(digest)
}
fn load(state: &DaemonState, digest: &str) -> Result<FrozenSources> {
    let database = state.jobs.database();
    database.apply_migration("runtime.sources", 1, MIGRATION).map_err(message)?;
    let encoded: Option<String> = database.connection().map_err(message)?.query_row(
        "SELECT snapshot_json FROM source_generations WHERE digest=?1", [digest], |row| row.get(0),
    ).optional().map_err(message)?;
    let frozen: FrozenSources = serde_json::from_str(&encoded.ok_or("source snapshot is missing")?).map_err(message)?;
    if frozen.schema_version != 1 || canonical_digest(&frozen).map_err(message)? != digest {
        return Err("source snapshot identity/schema mismatch".into());
    }
    Ok(frozen)
}
async fn build(state: &Arc<DaemonState>, digest: &str, frozen: &FrozenSources) -> Result<SourceGeneration> {
    let updates = state.updates.as_ref().ok_or("signed update coordinator unavailable")?;
    let policies = frozen.environment.scopes.values().flat_map(|generation| generation.targets.signed.packages.values())
        .filter_map(|record| frozen.configuration.iter().find(|(source, _)| record.manifest.id == format!("source-{source}"))
            .map(|(source, config)| SourceNetworkPolicy {
                module_id: record.manifest.id.clone(), module_digest: record.digest.clone(), source_id: source.clone(),
                api_base: config.api_base.clone(), website_base: config.website_base.clone(), credential: config.credential_ref.clone(), proxy_policy: mintcat_runtime_broker::ProxyPolicy::Direct,
            })).collect::<Vec<_>>();
    let network = Arc::new(SourceNetworkHost::new(state.network_broker.clone(), policies).map_err(message)?);
    let supervisor = updates.supervisor_for_environment(&frozen.environment, network).await.map_err(message)?;
    let digests = updates.source_closure_digests(&frozen.environment).map_err(message)?;
    let lease = if digests.is_empty() { None } else {
        Some(updates.retain_environment_lease(&format!("source-generation:{digest}"), &frozen.environment, &digests).map_err(message)?.lease_id)
    };
    let catalog = DownloadCatalog::default();
    for (scope, generation) in &frozen.environment.scopes {
        if !generation.confirmed { continue; }
        for record in generation.targets.signed.packages.values() {
            for (source_id, config) in &frozen.configuration {
                if record.manifest.id != format!("source-{source_id}") { continue; }
                for descriptor in &frozen.games {
                    if !descriptor.source_bindings.iter().any(|binding| &binding.source_id == source_id) { continue; }
                    NativeSourceProvider::register(&catalog, supervisor.clone(), supervisor.artifacts(), SourcePackageBinding {
                        provider: mintcat_sdk::ScopedProviderBinding { scope_id: scope.clone(), contract_id: format!("source.{source_id}"), contract_version: 1, provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone() },
                        source_id: source_id.clone(), module_lock_digest: digest.into(), game_descriptor: descriptor.clone(),
                    }, serde_json::to_value(config).map_err(message)?, state.interactions.clone()).await.map_err(message)?;
                }
            }
        }
    }
    let local = LocalSource::new(state.artifacts.clone(), state.interactions.clone(), state.layout.runtime_dir.join("local-inputs"), LocalImportLimits::default()).map_err(message)?;
    for descriptor in &frozen.games { mintcat_source_service::LocalInputProvider::register(&catalog, descriptor, digest).map_err(message)?; }
    let application = SourceApplication::new(state.library.clone(), state.library_authorization.clone(), state.jobs.clone(), catalog.clone(), local, digest.into(), state.layout.clone());
    let downloads = DownloadCoordinator::new(state.library.clone(), state.library_authorization.clone(), state.jobs.clone(), state.transfers.clone(), state.artifacts.clone(), catalog);
    Ok(SourceGeneration {
        environment_generation: frozen.environment.generation_id.clone(),
        scope_confirmations: frozen.environment.scopes.values().map(|generation| generation.confirmed).collect(),
        lock_digest: digest.into(), application, downloads, supervisor, lease_id: lease,
    })
}
pub(super) async fn publish(state: &Arc<DaemonState>, environment: EnvironmentSnapshot, games: Vec<GameDescriptor>, configuration: BTreeMap<String, SourceWorkerConfig>) -> Result<()> {
    let frozen = FrozenSources { schema_version: 1, environment, configuration, games };
    let digest = store(state, &frozen)?;
    let existing = state.source_generations.read().map_err(message)?.get(&digest).cloned();
    let generation = match existing { Some(generation) => generation, None => build(state, &digest, &frozen).await? };
    state.source_generations.write().map_err(message)?.insert(digest, generation.clone());
    *state.sources.write().map_err(message)? = Some(generation);
    Ok(())
}
fn pending(state: &DaemonState) -> Result<Vec<(String, String)>> {
    let database = state.jobs.database();
    let connection = database.connection().map_err(message)?;
    let mut statement = connection.prepare("SELECT job_id,target_json FROM jobs WHERE owner_scope=?1 AND command IN ('library.download.apply','mods.add','mods.importLocal') AND state NOT IN ('succeeded','failed','cancelled')").map_err(message)?;
    let rows = statement.query_map([LOCAL_OWNER_SCOPE], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(message)?;
    rows.map(|row| {
        let (job, encoded) = row.map_err(message)?;
        let target: Value = serde_json::from_str(&encoded).map_err(message)?;
        let digest = target.get("moduleLockDigest").and_then(Value::as_str).ok_or("source job lacks its immutable code lock")?;
        Ok((job, digest.into()))
    }).collect()
}
pub(super) async fn restore(state: &Arc<DaemonState>) -> Result<()> {
    let pending = pending(state)?;
    for digest in pending.iter().map(|(_, digest)| digest).collect::<BTreeSet<_>>() {
        if state.source_generations.read().map_err(message)?.contains_key(digest) { continue; }
        let result = async { let frozen = load(state, digest)?; build(state, digest, &frozen).await }.await;
        match result {
            Ok(generation) => { state.source_generations.write().map_err(message)?.insert(digest.clone(), generation); }
            Err(reason) => for (job_id, _) in pending.iter().filter(|(_, lock)| lock == digest) { unavailable(state, job_id, &reason)?; },
        }
    }
    for record in &state.recovered {
        if !matches!(record.snapshot.command.as_str(), "mods.add" | "mods.importLocal") { continue; }
        if let Ok(generation) = for_job(state, &record.snapshot.job_id) {
            let application = generation.application; let record = record.clone();
            tokio::task::spawn_blocking(move || application.recover(&record)).await.map_err(message)?.map_err(message)?;
        }
    }
    Ok(())
}
fn unavailable(state: &DaemonState, job_id: &str, reason: &str) -> Result<()> {
    let mut event = EventUpdate::state("source-generation-unavailable", mintcat_sdk::JobState::RecoveryRequired);
    event.error = Some(mintcat_sdk::AppError { code: "MODULE_UNAVAILABLE".into(), message_key: "source.frozen_generation_unavailable".into(), details: serde_json::json!({"reason":reason}), request_id: job_id.into(), job_id: Some(job_id.into()), retryable: false, recovery_actions: vec![] });
    state.jobs.append_event(job_id, event).map_err(message)?;
    Ok(())
}
/// Called under the composition lock: no new source call can race lease removal.
pub(super) async fn collect_obsolete(state: &Arc<DaemonState>) -> Result<()> {
    let mut retained = pending(state)?.into_iter().map(|(_, digest)| digest).collect::<BTreeSet<_>>();
    if let Ok(current) = current_lock(state) { retained.insert(current); }
    let removed = {
        let mut generations = state.source_generations.write().map_err(message)?;
        let obsolete = generations.keys().filter(|digest| !retained.contains(*digest)).cloned().collect::<Vec<_>>();
        obsolete.into_iter().filter_map(|digest| generations.remove(&digest).map(|generation| (digest, generation))).collect::<Vec<_>>()
    };
    for (digest, generation) in removed {
        generation.supervisor.shutdown_all().await.map_err(message)?;
        if let Some(lease) = generation.lease_id { state.updates.as_ref().ok_or("updates unavailable")?.release_lease(&lease).map_err(message)?; }
        state.jobs.database().connection().map_err(message)?.execute("DELETE FROM source_generations WHERE digest=?1", [digest]).map_err(message)?;
    }
    let updates = state.updates.as_ref().ok_or("updates unavailable")?;
    for lease in updates.leases().map_err(message)? {
        if lease.owner_id.strip_prefix("source-generation:").is_some_and(|digest| !retained.contains(digest)) {
            updates.release_lease(&lease.lease_id).map_err(message)?;
        }
    }
    Ok(())
}
pub(super) async fn shutdown(state: &DaemonState) -> Result<()> {
    let generations = state.source_generations.read().map_err(message)?.values().cloned().collect::<Vec<_>>();
    for generation in generations { generation.supervisor.shutdown_all().await.map_err(message)?; }
    Ok(())
}
