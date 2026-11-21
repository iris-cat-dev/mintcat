use std::{collections::{BTreeMap, BTreeSet}, io::Read, sync::{Arc, LazyLock}};
use mintcat_game_api::{canonical_digest, GameDescriptor};
use mintcat_runtime_broker::{CredentialRef, SecretValue};
use mintcat_runtime_downloads::{DownloadCatalog, DownloadCoordinator};
use mintcat_runtime_jobs::EventUpdate;
use mintcat_runtime_kernel::KernelDatabase;
use mintcat_runtime_updates::EnvironmentSnapshot;
use mintcat_source_api::SourceConfig;
use mintcat_source_local::{LocalSource, LocalImportLimits};
use mintcat_source_service::{register_builtin_sources, SourceApplication};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use super::{ConfiguredSourceSummary, DaemonState, SourceGeneration, LOCAL_OWNER_SCOPE};

type Result<T, E = String> = std::result::Result<T, E>;
fn message(error: impl std::fmt::Display) -> String { error.to_string() }

pub(super) enum CredentialMutationError {
    Empty,
    InvalidFormat,
    NotConfigured(String),
    VaultUnavailable,
    Unavailable,
}

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
const MIGRATION_1: &str = "CREATE TABLE source_generations (digest TEXT PRIMARY KEY, snapshot_json TEXT NOT NULL);";
const MIGRATION_2: &str = "CREATE TABLE source_credentials (source_id TEXT PRIMARY KEY, credential_ref TEXT NOT NULL, updated_at_ms INTEGER NOT NULL);";

fn ensure_migrations(database: &KernelDatabase) -> Result<()> {
    database.apply_migration("runtime.sources", 1, MIGRATION_1).map_err(message)?;
    database.apply_migration("runtime.sources", 2, MIGRATION_2).map_err(message)?;
    Ok(())
}

fn parse_credential_ref(raw: String) -> Result<CredentialRef> {
    serde_json::from_value(Value::String(raw)).map_err(message)
}

pub(super) fn parse_secret_input(raw: &str) -> std::result::Result<SecretValue, CredentialMutationError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CredentialMutationError::Empty);
    }
    if !trimmed.bytes().all(|byte| (0x21..=0x7E).contains(&byte)) {
        return Err(CredentialMutationError::InvalidFormat);
    }
    SecretValue::new(trimmed.as_bytes().to_vec()).map_err(|_| CredentialMutationError::Empty)
}

fn load_credential_overrides(state: &DaemonState) -> Result<BTreeMap<String, CredentialRef>> {
    let database = state.jobs.database();
    ensure_migrations(database)?;
    let connection = database.connection().map_err(message)?;
    let mut statement = connection.prepare("SELECT source_id, credential_ref FROM source_credentials").map_err(message)?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(message)?;
    let mut overrides = BTreeMap::new();
    for row in rows {
        let (source_id, raw_ref) = row.map_err(message)?;
        overrides.insert(source_id, parse_credential_ref(raw_ref)?);
    }
    Ok(overrides)
}

pub(super) fn merge_credential_overrides(state: &DaemonState, mut configuration: BTreeMap<String, SourceConfig>) -> Result<BTreeMap<String, SourceConfig>> {
    for (source_id, credential_ref) in load_credential_overrides(state)? {
        if let Some(config) = configuration.get_mut(&source_id) {
            config.credential_ref = Some(credential_ref);
        }
    }
    Ok(configuration)
}

fn upsert_credential_override(state: &DaemonState, source_id: &str, reference: &CredentialRef, updated_at_ms: i64) -> Result<Option<(CredentialRef, i64)>> {
    let database = state.jobs.database();
    ensure_migrations(database)?;
    let mut connection = database.connection().map_err(message)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(message)?;
    let previous = transaction.query_row(
        "SELECT credential_ref, updated_at_ms FROM source_credentials WHERE source_id = ?1",
        [source_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    ).optional().map_err(message)?;
    let previous = previous.map(|(raw, ts)| parse_credential_ref(raw).map(|r| (r, ts))).transpose()?;
    transaction.execute(
        "INSERT INTO source_credentials(source_id, credential_ref, updated_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(source_id) DO UPDATE SET credential_ref = excluded.credential_ref, updated_at_ms = excluded.updated_at_ms",
        params![source_id, reference.as_str(), updated_at_ms],
    ).map_err(message)?;
    transaction.commit().map_err(message)?;
    Ok(previous)
}

fn delete_credential_override(state: &DaemonState, source_id: &str) -> Result<Option<(CredentialRef, i64)>> {
    let database = state.jobs.database();
    ensure_migrations(database)?;
    let mut connection = database.connection().map_err(message)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(message)?;
    let previous = transaction.query_row(
        "SELECT credential_ref, updated_at_ms FROM source_credentials WHERE source_id = ?1",
        [source_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    ).optional().map_err(message)?;
    let previous = previous.map(|(raw, ts)| parse_credential_ref(raw).map(|r| (r, ts))).transpose()?;
    transaction.execute("DELETE FROM source_credentials WHERE source_id = ?1", [source_id]).map_err(message)?;
    transaction.commit().map_err(message)?;
    Ok(previous)
}

fn restore_credential_override(state: &DaemonState, source_id: &str, previous: Option<&(CredentialRef, i64)>) -> Result<()> {
    let database = state.jobs.database();
    ensure_migrations(database)?;
    let mut connection = database.connection().map_err(message)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(message)?;
    match previous {
        Some((reference, updated_at_ms)) => {
            transaction.execute(
                "INSERT INTO source_credentials(source_id, credential_ref, updated_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_id) DO UPDATE SET credential_ref = excluded.credential_ref, updated_at_ms = excluded.updated_at_ms",
                params![source_id, reference.as_str(), updated_at_ms],
            ).map_err(message)?;
        }
        None => {
            transaction.execute("DELETE FROM source_credentials WHERE source_id = ?1", [source_id]).map_err(message)?;
        }
    }
    transaction.commit().map_err(message)?;
    Ok(())
}

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
    Ok(json!({
        "sources": generation.configured_sources.iter().map(|source| json!({
            "sourceId": source.source_id,
            "credentialConfigured": source.credential_configured,
        })).collect::<Vec<_>>()
    }))
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
    ensure_migrations(database)?;
    let connection = database.connection().map_err(message)?;
    connection.execute("INSERT INTO source_generations(digest,snapshot_json) VALUES (?1,?2) ON CONFLICT(digest) DO NOTHING", params![digest, encoded]).map_err(message)?;
    let stored: String = connection.query_row("SELECT snapshot_json FROM source_generations WHERE digest=?1", [&digest], |row| row.get(0)).map_err(message)?;
    if stored != encoded { return Err("source configuration identity was reused for different inputs".into()); }
    Ok(digest)
}
fn load(state: &DaemonState, digest: &str) -> Result<StoredSources> {
    let database = state.jobs.database();
    ensure_migrations(database)?;
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
    let configured_sources = configuration.iter().map(|(source_id, config)| ConfiguredSourceSummary {
        source_id: source_id.clone(),
        credential_configured: config.credential_ref.is_some(),
    }).collect();
    Ok(SourceGeneration {
        environment_generation: state.generation_id.clone(),
        scope_confirmations: Arc::from([]),
        games: Arc::from(games.to_vec()),
        lock_digest: digest.into(),
        application,
        downloads,
        remote_downloads_available,
        configured_sources,
    })
}
fn publish_with_metadata(
    state: &Arc<DaemonState>,
    environment_generation: String,
    scope_confirmations: Arc<[bool]>,
    games: Vec<GameDescriptor>,
    configuration: BTreeMap<String, SourceConfig>,
) -> Result<()> {
    let frozen = FrozenSources { schema_version: 2, application_digest: application_digest()?.into(), configuration, games };
    let digest = store(state, &frozen)?;
    let existing = state.source_generations.read().map_err(message)?.get(&digest).cloned();
    let mut generation = match existing { Some(generation) => generation, None => build(state, &digest, &frozen.configuration, &frozen.games, true)? };
    generation.environment_generation = environment_generation;
    generation.scope_confirmations = scope_confirmations;
    state.source_generations.write().map_err(message)?.insert(digest, generation.clone());
    *state.sources.write().map_err(message)? = Some(generation);
    Ok(())
}
pub(super) async fn publish(state: &Arc<DaemonState>, environment: EnvironmentSnapshot, games: Vec<GameDescriptor>, configuration: BTreeMap<String, SourceConfig>) -> Result<()> {
    let scope_confirmations = environment.scopes.values().map(|generation| generation.confirmed).collect();
    publish_with_metadata(state, environment.generation_id, scope_confirmations, games, configuration)
}

pub(super) fn set_credential(
    state: &Arc<DaemonState>,
    source_id: &str,
    secret: SecretValue,
) -> std::result::Result<mintcat_sdk::SourceCredentialResult, CredentialMutationError> {
    let base_configuration = state.configuration.as_ref()
        .map(|configuration| configuration.configured_sources().clone())
        .unwrap_or_default();
    if !base_configuration.contains_key(source_id) {
        return Err(CredentialMutationError::NotConfigured(source_id.to_owned()));
    }
    let (environment_generation, scope_confirmations, games) = {
        let current = state.sources.read().map_err(|_| CredentialMutationError::Unavailable)?;
        let generation = current.as_ref().ok_or(CredentialMutationError::Unavailable)?;
        (generation.environment_generation.clone(), generation.scope_confirmations.clone(), generation.games.to_vec())
    };
    let new_ref = state.credential_vault.store(secret)
        .map_err(|_| CredentialMutationError::VaultUnavailable)?;
    let now = super::now_ms().map_err(|_| CredentialMutationError::Unavailable)?;
    let previous = match upsert_credential_override(state, source_id, &new_ref, now) {
        Ok(previous) => previous,
        Err(_) => {
            let _ = state.credential_vault.delete(&new_ref);
            return Err(CredentialMutationError::Unavailable);
        }
    };
    let merged = match merge_credential_overrides(state, base_configuration) {
        Ok(merged) => merged,
        Err(_) => {
            let _ = restore_credential_override(state, source_id, previous.as_ref());
            let _ = state.credential_vault.delete(&new_ref);
            return Err(CredentialMutationError::Unavailable);
        }
    };
    let credential_configured = merged.get(source_id).and_then(|config| config.credential_ref.as_ref()).is_some();
    if publish_with_metadata(state, environment_generation, scope_confirmations, games, merged).is_err() {
        let _ = restore_credential_override(state, source_id, previous.as_ref());
        let _ = state.credential_vault.delete(&new_ref);
        return Err(CredentialMutationError::Unavailable);
    }
    if let Some((previous_ref, _)) = previous {
        if previous_ref != new_ref {
            let _ = state.credential_vault.delete(&previous_ref);
        }
    }
    let _ = collect_obsolete_sync(state);
    Ok(mintcat_sdk::SourceCredentialResult {
        source_id: source_id.to_owned(),
        credential_configured,
    })
}

pub(super) fn clear_credential(
    state: &Arc<DaemonState>,
    source_id: &str,
) -> std::result::Result<mintcat_sdk::SourceCredentialResult, CredentialMutationError> {
    let base_configuration = state.configuration.as_ref()
        .map(|configuration| configuration.configured_sources().clone())
        .unwrap_or_default();
    if !base_configuration.contains_key(source_id) {
        return Err(CredentialMutationError::NotConfigured(source_id.to_owned()));
    }
    let (environment_generation, scope_confirmations, games) = {
        let current = state.sources.read().map_err(|_| CredentialMutationError::Unavailable)?;
        let generation = current.as_ref().ok_or(CredentialMutationError::Unavailable)?;
        (generation.environment_generation.clone(), generation.scope_confirmations.clone(), generation.games.to_vec())
    };
    let previous = delete_credential_override(state, source_id)
        .map_err(|_| CredentialMutationError::Unavailable)?;
    let merged = match merge_credential_overrides(state, base_configuration) {
        Ok(merged) => merged,
        Err(_) => {
            let _ = restore_credential_override(state, source_id, previous.as_ref());
            return Err(CredentialMutationError::Unavailable);
        }
    };
    let credential_configured = merged.get(source_id).and_then(|config| config.credential_ref.as_ref()).is_some();
    if publish_with_metadata(state, environment_generation, scope_confirmations, games, merged).is_err() {
        let _ = restore_credential_override(state, source_id, previous.as_ref());
        return Err(CredentialMutationError::Unavailable);
    }
    if let Some((previous_ref, _)) = previous {
        let _ = state.credential_vault.delete(&previous_ref);
    }
    let _ = collect_obsolete_sync(state);
    Ok(mintcat_sdk::SourceCredentialResult {
        source_id: source_id.to_owned(),
        credential_configured,
    })
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
pub(super) fn collect_obsolete_sync(state: &DaemonState) -> Result<()> {
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

/// Drop terminal operations' configuration snapshots, never their content.
pub(super) async fn collect_obsolete(state: &Arc<DaemonState>) -> Result<()> {
    collect_obsolete_sync(state.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{application, Principal};
    use mintcat_game_api::{GameInputFormat, GameSourceBinding, GameVariantDescriptor};
    use mintcat_runtime_jobs::{request_hash, ConfirmationPolicy, RegisterJob};
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
    use mintcat_sdk::{decode_request_value, JobState};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

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

    fn validated(method: &str, idempotency_key: Option<&str>, params: Value) -> mintcat_sdk::ValidatedRequest {
        let mut envelope = json!({
            "jsonrpc": "2.0",
            "id": format!("req-{method}"),
            "method": method,
            "contractVersion": 1,
            "sessionGeneration": "test-generation",
            "deadlineMs": 5000,
            "params": params,
        });
        if let Some(key) = idempotency_key {
            envelope["idempotencyKey"] = Value::String(key.to_owned());
        }
        decode_request_value(envelope).unwrap()
    }

    fn test_game_descriptor() -> GameDescriptor {
        GameDescriptor {
            schema_version: 1,
            game_id: "game:drg".into(),
            display_name: "Deep Rock Galactic".into(),
            adapter_version: "1.0.0".into(),
            settings_schema_version: 1,
            settings_schema: json!({"type": "object", "properties": {}}),
            variants: vec![GameVariantDescriptor {
                variant_id: "steam".into(),
                display_name: "Steam".into(),
                store: "steam".into(),
                store_app_id: Some("548430".into()),
                target: "windows-x86_64".into(),
                main_pak_relative_path: "FSD/Content/Paks/FSD-WindowsNoEditor.pak".into(),
                paks_relative_path: "FSD/Content/Paks".into(),
                binaries_relative_path: "FSD/Binaries/Win64".into(),
                mod_pak_relative_path: "FSD/Content/Paks/Mods".into(),
                process_names: vec!["FSD-Win64-Shipping.exe".into()],
            }],
            input_formats: vec![GameInputFormat::Pak, GameInputFormat::Zip],
            source_bindings: vec![GameSourceBinding {
                source_id: "modio".into(),
                remote_game_key: "drg".into(),
                provider_game_id: "2475".into(),
            }],
            resources: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    async fn spawn_auth_modio_server(expected_tokens: Arc<std::sync::Mutex<Vec<String>>>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let expected = Arc::clone(&expected_tokens);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let size = stream.read(&mut chunk).await.unwrap_or(0);
                        if size == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&chunk[..size]);
                        if bytes.windows(4).any(|window| window == b"\r\n\r\n") || bytes.len() > 65536 {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    let auth = request.lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("authorization") { Some(value.trim().to_owned()) } else { None }
                        });
                    let accepted = expected.lock().unwrap().iter()
                        .any(|token| auth.as_deref() == Some(&format!("Bearer {token}")));
                    let (status, body) = if accepted {
                        ("200 OK", json!({
                            "data": [{
                                "id": 101,
                                "game_id": 2475,
                                "status": 1,
                                "visible": 1,
                                "date_added": 1700000000_i64,
                                "date_updated": 1700000000_i64,
                                "name": "Vault Authenticated Mod",
                                "name_id": "vault-mod",
                                "summary": "Loaded via OS credential vault",
                                "profile_url": "https://mod.io/g/drg/m/vault-mod",
                                "submitted_by": { "username": "Author" },
                                "tags": [{"name": "Approved"}]
                            }],
                            "result_total": 1
                        }).to_string())
                    } else {
                        ("401 Unauthorized", json!({"error": {"code": 401, "message": "Unauthorized"}}).to_string())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}/v1/"), handle)
    }

    #[tokio::test]
    async fn source_credential_set_replace_clear_reloads_live_provider_without_persisting_plaintext() {
        let active_tokens = Arc::new(std::sync::Mutex::new(vec!["token-alpha-secret-value".to_owned()]));
        let (api_base, server) = spawn_auth_modio_server(Arc::clone(&active_tokens)).await;

        let sandbox = tempfile::tempdir().unwrap();
        let data_dir = sandbox.path().join("data");
        std::fs::create_dir_all(data_dir.join("state/trust")).unwrap();
        std::fs::write(
            data_dir.join("state/trust/runtime.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 2,
                "scopes": {},
                "sources": {
                    "modio": {
                        "apiBase": api_base,
                        "downloadOrigins": []
                    }
                }
            })).unwrap(),
        ).unwrap();

        let paths = RuntimePaths::new(&data_dir).unwrap();
        let daemon = crate::BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let base_sources = daemon.state.configuration.as_ref().unwrap().configured_sources().clone();
        let merged = merge_credential_overrides(&daemon.state, base_sources).unwrap();
        publish(
            &daemon.state,
            EnvironmentSnapshot { generation_id: "builtin".into(), scopes: BTreeMap::new() },
            vec![test_game_descriptor()],
            merged,
        ).await.unwrap();

        // Initially unconfigured: sources.list reports false and search fails with SOURCE_UNAUTHORIZED.
        let initial_list = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.list", None, json!({})),
            Principal::Human,
        ).await.unwrap();
        assert_eq!(initial_list, json!({"sources": [{"sourceId": "modio", "credentialConfigured": false}]}));

        let unauth = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.search", None, json!({"sourceId": "modio", "gameId": "game:drg", "remoteGameKey": "drg", "query": ""})),
            Principal::Human,
        ).await.unwrap_err();
        assert_eq!(unauth.code, "SOURCE_UNAUTHORIZED");

        // Agent principal is denied credential mutations.
        let agent_denied = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("agent-set"), json!({"sourceId": "modio", "secret": "token-alpha-secret-value"})),
            Principal::Agent,
        ).await.unwrap_err();
        assert_eq!(agent_denied.code, "PERMISSION_DENIED");
        let agent_clear_denied = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.clear", Some("agent-clear"), json!({"sourceId": "modio"})),
            Principal::Agent,
        ).await.unwrap_err();
        assert_eq!(agent_clear_denied.code, "PERMISSION_DENIED");

        // Invalid whitespace / control characters and unconfigured sources are rejected safely.
        let whitespace_err = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("empty-secret"), json!({"sourceId": "modio", "secret": "   \n  "})),
            Principal::Human,
        ).await.unwrap_err();
        assert_eq!(whitespace_err.code, "INVALID_ARGUMENT");
        assert_eq!(whitespace_err.details["reason"], "credential_empty");

        let bad_format = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("bad-secret"), json!({"sourceId": "modio", "secret": "Bearer leaked-secret-text"})),
            Principal::Human,
        ).await.unwrap_err();
        assert_eq!(bad_format.code, "INVALID_ARGUMENT");
        assert_eq!(bad_format.details["reason"], "credential_invalid_format");
        assert!(!serde_json::to_string(&bad_format).unwrap().contains("leaked-secret-text"));

        let missing_source = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("missing-source"), json!({"sourceId": "nexusmods", "secret": "token-alpha-secret-value"})),
            Principal::Human,
        ).await.unwrap_err();
        assert_eq!(missing_source.code, "NOT_FOUND");

        // Human saves credential (with surrounding whitespace trimmed) -> live reload succeeds immediately.
        let saved = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("set-alpha"), json!({"sourceId": "modio", "secret": "  token-alpha-secret-value \n"})),
            Principal::Human,
        ).await.unwrap();
        mintcat_sdk::validate_command_result("sources.credentials.set", 1, &saved).unwrap();
        assert_eq!(saved, json!({"sourceId": "modio", "credentialConfigured": true}));

        let list_after_set = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.list", None, json!({})),
            Principal::Human,
        ).await.unwrap();
        mintcat_sdk::validate_command_result("sources.list", 1, &list_after_set).unwrap();
        assert_eq!(list_after_set, json!({"sources": [{"sourceId": "modio", "credentialConfigured": true}]}));

        let search_ok = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.search", None, json!({"sourceId": "modio", "gameId": "game:drg", "remoteGameKey": "drg", "query": ""})),
            Principal::Human,
        ).await.unwrap();
        assert_eq!(search_ok["items"][0]["displayName"], "Vault Authenticated Mod");

        // Replace credential with a >4096-byte personal access token and verify live search and zero plaintext leakage.
        let long_pat = format!("eyJ{}.{}.{}", "A".repeat(2500), "B".repeat(2500), "C".repeat(1000));
        assert!(long_pat.len() > 4096);
        *active_tokens.lock().unwrap() = vec![long_pat.clone()];
        let stale_err = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.search", None, json!({"sourceId": "modio", "gameId": "game:drg", "remoteGameKey": "drg", "query": ""})),
            Principal::Human,
        ).await.unwrap_err();
        assert_eq!(stale_err.code, "SOURCE_UNAUTHORIZED");

        let replaced = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("set-long-pat"), json!({"sourceId": "modio", "secret": long_pat})),
            Principal::Human,
        ).await.unwrap();
        assert_eq!(replaced, json!({"sourceId": "modio", "credentialConfigured": true}));

        let search_after_replace = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.search", None, json!({"sourceId": "modio", "gameId": "game:drg", "remoteGameKey": "drg", "query": ""})),
            Principal::Human,
        ).await.unwrap();
        assert_eq!(search_after_replace["items"][0]["displayName"], "Vault Authenticated Mod");

        // Payloads exceeding the 32768-byte contract limit are rejected at schema validation.
        assert!(decode_request_value(json!({
            "jsonrpc": "2.0",
            "id": "req-oversized-secret",
            "method": "sources.credentials.set",
            "contractVersion": 1,
            "sessionGeneration": "test-generation",
            "deadlineMs": 5000,
            "idempotencyKey": "oversized-secret",
            "params": {"sourceId": "modio", "secret": "X".repeat(32769)},
        })).is_err());

        // Plaintext secrets must NEVER appear anywhere on disk inside data_dir.
        assert!(data_dir.join("kernel.sqlite3").is_file(), "expected kernel.sqlite3 to exist");
        let mut stack = vec![data_dir.clone()];
        let mut scanned_files = 0usize;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    scanned_files += 1;
                    let bytes = std::fs::read(&path).unwrap();
                    let text = String::from_utf8_lossy(&bytes);
                    assert!(!text.contains("token-alpha-secret-value"), "plaintext alpha leaked into {}", path.display());
                    assert!(!text.contains(&long_pat), "plaintext long PAT leaked into {}", path.display());
                }
            }
        }
        assert!(scanned_files >= 2, "expected multiple persisted files to be audited");

        // Clear credential -> list reports false and search reverts to SOURCE_UNAUTHORIZED.
        let cleared = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.clear", Some("clear-modio"), json!({"sourceId": "modio"})),
            Principal::Human,
        ).await.unwrap();
        mintcat_sdk::validate_command_result("sources.credentials.clear", 1, &cleared).unwrap();
        assert_eq!(cleared, json!({"sourceId": "modio", "credentialConfigured": false}));

        let list_after_clear = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.list", None, json!({})),
            Principal::Human,
        ).await.unwrap();
        assert_eq!(list_after_clear, json!({"sources": [{"sourceId": "modio", "credentialConfigured": false}]}));

        let unauth_again = application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.search", None, json!({"sourceId": "modio", "gameId": "game:drg", "remoteGameKey": "drg", "query": ""})),
            Principal::Human,
        ).await.unwrap_err();
        assert_eq!(unauth_again.code, "SOURCE_UNAUTHORIZED");

        server.abort();
    }

    #[tokio::test]
    async fn concurrent_sources_list_does_not_block_while_search_is_in_flight() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (search_started_tx, search_started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let size = stream.read(&mut chunk).await.unwrap_or(0);
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..size]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") || bytes.len() > 65536 {
                    break;
                }
            }
            let _ = search_started_tx.send(());
            let _ = release_rx.await;
            let body = json!({
                "data": [{
                    "id": 202,
                    "game_id": 2475,
                    "status": 1,
                    "visible": 1,
                    "date_added": 1700000000_i64,
                    "date_updated": 1700000000_i64,
                    "name": "Slow Search Mod",
                    "name_id": "slow-search-mod",
                    "summary": "Delayed response",
                    "profile_url": "https://mod.io/g/drg/m/slow-search-mod",
                    "submitted_by": { "username": "Author" },
                    "tags": []
                }],
                "result_total": 1
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let sandbox = tempfile::tempdir().unwrap();
        let data_dir = sandbox.path().join("data");
        std::fs::create_dir_all(data_dir.join("state/trust")).unwrap();
        std::fs::write(
            data_dir.join("state/trust/runtime.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 2,
                "scopes": {},
                "sources": {
                    "modio": {
                        "apiBase": format!("http://{address}/v1/"),
                        "downloadOrigins": []
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let paths = RuntimePaths::new(&data_dir).unwrap();
        let daemon = crate::BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let base_sources = daemon.state.configuration.as_ref().unwrap().configured_sources().clone();
        let merged = merge_credential_overrides(&daemon.state, base_sources).unwrap();
        publish(
            &daemon.state,
            EnvironmentSnapshot { generation_id: "builtin".into(), scopes: BTreeMap::new() },
            vec![test_game_descriptor()],
            merged,
        )
        .await
        .unwrap();

        application::handle_request(
            Arc::clone(&daemon.state),
            validated("sources.credentials.set", Some("set-concurrency-token"), json!({"sourceId": "modio", "secret": "concurrency-test-token"})),
            Principal::Human,
        )
        .await
        .unwrap();

        let search_state = Arc::clone(&daemon.state);
        let search_task = tokio::spawn(async move {
            application::handle_request(
                search_state,
                validated("sources.search", None, json!({"sourceId": "modio", "gameId": "game:drg", "remoteGameKey": "drg", "query": ""})),
                Principal::Human,
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), search_started_rx)
            .await
            .expect("search request should reach mock server")
            .unwrap();

        // While sources.search is awaiting the network response, sources.list must not block.
        let list_result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            application::handle_request(
                Arc::clone(&daemon.state),
                validated("sources.list", None, json!({})),
                Principal::Human,
            ),
        )
        .await
        .expect("sources.list must not wait on in-flight sources.search")
        .unwrap();
        assert_eq!(
            list_result,
            json!({"sources": [{"sourceId": "modio", "credentialConfigured": true}]})
        );

        let _ = release_tx.send(());
        let search_result = search_task.await.unwrap().unwrap();
        assert_eq!(search_result["items"][0]["displayName"], "Slow Search Mod");
        server.await.unwrap();
    }
}
