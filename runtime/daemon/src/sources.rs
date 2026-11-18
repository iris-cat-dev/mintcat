use std::{collections::{BTreeMap, BTreeSet}, io::Read, sync::{Arc, LazyLock}};
use mintcat_game_api::{canonical_digest, GameDescriptor};
use mintcat_runtime_downloads::{DownloadCatalog, DownloadCoordinator};
use mintcat_runtime_jobs::EventUpdate;
use mintcat_runtime_updates::EnvironmentSnapshot;
use mintcat_source_api::SourceConfig;
use mintcat_source_local::{LocalSource, LocalImportLimits};
use mintcat_source_service::{register_builtin_sources, SourceApplication};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use super::{DaemonState, SourceGeneration, LOCAL_OWNER_SCOPE};

type Result<T, E = String> = std::result::Result<T, E>;
fn message(error: impl std::fmt::Display) -> String { error.to_string() }

fn application_digest() -> Result<&'static str> {
    static DIGEST: LazyLock<Result<String>> = LazyLock::new(|| {
        let mut file = std::fs::File::open(std::env::current_exe().map_err(message)?).map_err(message)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            let size = file.read(&mut buffer).map_err(message)?;
            if size == 0 { break; }
            hash.update(&buffer[..size]);
        }
        Ok(format!("sha256:{:x}", hash.finalize()))
    });
    DIGEST.as_ref().map(String::as_str).map_err(Clone::clone)
}
const MIGRATION: &str = "CREATE TABLE source_generations (digest TEXT PRIMARY KEY, snapshot_json TEXT NOT NULL);";
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct FrozenSources {
    schema_version: u64,
    application_digest: String,
    configuration: BTreeMap<String, SourceConfig>,
    games: Vec<GameDescriptor>,
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct LegacySources {
    schema_version: u64,
    environment: EnvironmentSnapshot,
    configuration: BTreeMap<String, SourceConfig>,
    games: Vec<GameDescriptor>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredSources { Builtin(FrozenSources), Legacy(LegacySources) }

pub(super) fn list(state: &DaemonState) -> Result<Value> {
    let generation = state.sources.read().map_err(message)?;
    let generation = generation.as_ref().ok_or("source configuration unavailable")?;
    Ok(json!({"sources": generation.configured_sources.iter().map(|source| json!({"sourceId": source})).collect::<Vec<_>>()}))
}
pub(super) fn current_lock(state: &DaemonState) -> Result<String> {
    state.sources.read().map_err(message)?.as_ref().map(|source| source.lock_digest.clone())
        .ok_or_else(|| "source configuration unavailable".into())
}
pub(super) fn for_job(state: &DaemonState, job_id: &str) -> Result<SourceGeneration> {
    let target = state.jobs.get_job_target(LOCAL_OWNER_SCOPE, job_id).map_err(message)?;
    let digest = target.get("moduleLockDigest").and_then(Value::as_str)
        .ok_or("source job has no frozen configuration")?;
    let generation = state.source_generations.read().map_err(message)?.get(digest).cloned()
        .ok_or_else(|| format!("source configuration {digest} is unavailable"))?;
    if !generation.remote_downloads_available && state.jobs.get_job(LOCAL_OWNER_SCOPE, job_id).map_err(message)?.command == "library.download.apply" {
        return Err("interrupted signed-source download must be cancelled and resubmitted with the built-in source".into());
    }
    Ok(generation)
}
fn store(state: &DaemonState, frozen: &FrozenSources) -> Result<String> {
    let digest = canonical_digest(frozen).map_err(message)?;
    let encoded = serde_json::to_string(frozen).map_err(message)?;
    let database = state.jobs.database();
    database.apply_migration("runtime.sources", 1, MIGRATION).map_err(message)?;
    let connection = database.connection().map_err(message)?;
    connection.execute("INSERT INTO source_generations(digest,snapshot_json) VALUES (?1,?2) ON CONFLICT(digest) DO NOTHING", params![digest, encoded]).map_err(message)?;
    let stored: String = connection.query_row("SELECT snapshot_json FROM source_generations WHERE digest=?1", [&digest], |row| row.get(0)).map_err(message)?;
    if stored != encoded { return Err("source configuration identity was reused for different inputs".into()); }
    Ok(digest)
}
fn load(state: &DaemonState, digest: &str) -> Result<StoredSources> {
    let database = state.jobs.database();
    database.apply_migration("runtime.sources", 1, MIGRATION).map_err(message)?;
    let encoded: Option<String> = database.connection().map_err(message)?.query_row(
        "SELECT snapshot_json FROM source_generations WHERE digest=?1", [digest], |row| row.get(0),
    ).optional().map_err(message)?;
    let value: Value = serde_json::from_str(&encoded.ok_or("source snapshot is missing")?).map_err(message)?;
    if canonical_digest(&value).map_err(message)? != digest { return Err("source snapshot identity mismatch".into()); }
    let frozen: StoredSources = serde_json::from_value(value).map_err(message)?;
    match &frozen {
        StoredSources::Builtin(snapshot) if snapshot.schema_version == 2 => {},
        StoredSources::Legacy(snapshot) if snapshot.schema_version == 1 => {},
        _ => return Err("unsupported source snapshot schema".into()),
    }
    Ok(frozen)
}
fn build(state: &Arc<DaemonState>, digest: &str, configuration: &BTreeMap<String, SourceConfig>, games: &[GameDescriptor], remote_downloads_available: bool) -> Result<SourceGeneration> {
    let catalog = DownloadCatalog::default();
    let local_games = register_builtin_sources(&catalog, configuration, games, &state.network_broker, &state.interactions, digest).map_err(message)?;
    let local = LocalSource::new(state.artifacts.clone(), state.interactions.clone(), state.layout.runtime_dir.join("local-inputs"), LocalImportLimits::default()).map_err(message)?;
    let application = SourceApplication::new(state.library.clone(), state.library_authorization.clone(), state.jobs.clone(), catalog.clone(), local, local_games, digest.into(), state.layout.clone());
    let downloads = DownloadCoordinator::new(state.library.clone(), state.library_authorization.clone(), state.jobs.clone(), state.transfers.clone(), state.artifacts.clone(), catalog);
    Ok(SourceGeneration { environment_generation: state.generation_id.clone(), scope_confirmations: Arc::from([]), lock_digest: digest.into(), application, downloads, remote_downloads_available, configured_sources: configuration.keys().cloned().collect() })
}
pub(super) async fn publish(state: &Arc<DaemonState>, environment: EnvironmentSnapshot, games: Vec<GameDescriptor>, configuration: BTreeMap<String, SourceConfig>) -> Result<()> {
    let frozen = FrozenSources { schema_version: 2, application_digest: application_digest()?.into(), configuration, games };
    let digest = store(state, &frozen)?;
    let existing = state.source_generations.read().map_err(message)?.get(&digest).cloned();
    let mut generation = match existing { Some(generation) => generation, None => build(state, &digest, &frozen.configuration, &frozen.games, true)? };
    generation.environment_generation = environment.generation_id;
    generation.scope_confirmations = environment.scopes.values().map(|generation| generation.confirmed).collect();
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
        let digest = target.get("moduleLockDigest").and_then(Value::as_str).ok_or("source job lacks its immutable configuration")?;
        Ok((job, digest.into()))
    }).collect()
}
pub(super) async fn restore(state: &Arc<DaemonState>) -> Result<()> {
    let pending = pending(state)?;
    for digest in pending.iter().map(|(_, digest)| digest).collect::<BTreeSet<_>>() {
        if state.source_generations.read().map_err(message)?.contains_key(digest) { continue; }
        let result = load(state, digest).and_then(|snapshot| match snapshot {
            StoredSources::Builtin(snapshot) if snapshot.application_digest == application_digest()? => build(state, digest, &snapshot.configuration, &snapshot.games, true),
            StoredSources::Builtin(_) => Err("source operation belongs to another application executable; cancel and resubmit".into()),
            StoredSources::Legacy(snapshot) => {
                // Legacy source code is never replaced by current built-ins.
                // Local selection and owner-journal inspection need only the
                // original game binding; no network request is resumed here.
                let _ = (snapshot.environment, snapshot.configuration);
                build(state, digest, &BTreeMap::new(), &snapshot.games, false)
            },
        });
        match result {
            Ok(generation) => { state.source_generations.write().map_err(message)?.insert(digest.clone(), generation); },
            Err(reason) => for (job_id, _) in pending.iter().filter(|(_, lock)| lock == digest) { unavailable(state, job_id, &reason)?; },
        }
    }
    for (job_id, digest) in &pending {
        if state.source_generations.read().map_err(message)?.contains_key(digest) {
            if let Err(reason) = for_job(state, job_id) { unavailable(state, job_id, &reason)?; }
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
    let mut event = EventUpdate::state("source-configuration-unavailable", mintcat_sdk::JobState::RecoveryRequired);
    event.error = Some(mintcat_sdk::AppError { code: "RECOVERY_REQUIRED".into(), message_key: "source.frozen_configuration_unavailable".into(), details: json!({"reason":reason}), request_id: job_id.into(), job_id: Some(job_id.into()), retryable: false, recovery_actions: vec![] });
    state.jobs.append_event(job_id, event).map_err(message)?;
    Ok(())
}
/// Drop terminal operations' configuration snapshots, never their content.
pub(super) async fn collect_obsolete(state: &Arc<DaemonState>) -> Result<()> {
    let mut retained = pending(state)?.into_iter().map(|(_, digest)| digest).collect::<BTreeSet<_>>();
    if let Ok(current) = current_lock(state) { retained.insert(current); }
    let removed = {
        let mut generations = state.source_generations.write().map_err(message)?;
        let obsolete = generations.keys().filter(|digest| !retained.contains(*digest)).cloned().collect::<Vec<_>>();
        for digest in &obsolete { generations.remove(digest); }
        obsolete
    };
    for digest in removed {
        state.jobs.database().connection().map_err(message)?.execute("DELETE FROM source_generations WHERE digest=?1", [digest]).map_err(message)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mintcat_runtime_jobs::{request_hash, ConfirmationPolicy, RegisterJob};
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
    use mintcat_sdk::JobState;

    #[tokio::test]
    async fn interrupted_source_job_requires_its_exact_application_executable() {
        let sandbox = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        let daemon = crate::BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let frozen = FrozenSources {
            schema_version: 2,
            application_digest: format!("sha256:{}", "0".repeat(64)),
            configuration: BTreeMap::new(),
            games: Vec::new(),
        };
        let digest = store(&daemon.state, &frozen).unwrap();
        let target = json!({"moduleLockDigest": digest});
        let registration = daemon.state.jobs.register_job(RegisterJob {
            command: "library.download.apply".into(),
            owner_scope: LOCAL_OWNER_SCOPE.into(),
            idempotency_key: "interrupted-source-job".into(),
            request_hash: request_hash(&target).unwrap(),
            target,
            confirmation_policy: ConfirmationPolicy::None,
        }).unwrap();
        restore(&daemon.state).await.unwrap();
        let job = daemon.state.jobs.get_job(LOCAL_OWNER_SCOPE, &registration.job.job_id).unwrap();
        assert_eq!(job.state, JobState::RecoveryRequired);
        assert_eq!(job.error.unwrap().code, "RECOVERY_REQUIRED");
    }
}
