use super::*;
use mintcat_library_service::{CredentialVault, LegacyCredential, LibraryAuthorization, CONTENT_WRITE_PERMISSION};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use tempfile::tempdir;
use std::sync::{Arc, Mutex};

struct NoCredentials;
impl CredentialVault for NoCredentials {
    fn store(&self, _: LegacyCredential<'_>) -> Result<String, String> { Err("fixture must not contain credentials".into()) }
    fn delete(&self, _: &str) -> Result<(), String> { Ok(()) }
}
fn setup(root: &Path, fixture: bool) -> (BackupService, LibraryStore, RuntimeLayout) {
    let layout = RuntimePaths::new(root).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
    let library = LibraryStore::open(&layout).unwrap();
    if fixture {
        library.import_legacy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/baseline/legacy/mintcat-v0.6.sqlite"), &NoCredentials).unwrap();
    }
    (BackupService::new(layout.clone(), library.clone()).unwrap(), library, layout)
}
fn state() -> Result<BackupRuntimeState, BackupError> {
    Ok(BackupRuntimeState { blockers: BackupBlockers::default(), modules: BTreeMap::from([
        ("mintcat.library".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 5 }),
        ("mintcat.game-drg".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
        ("mintcat.game-rogue-core".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
    ]) })
}
fn rewrite_snapshot(bytes: &[u8], change: impl FnOnce(&mut LibrarySnapshot)) -> Vec<u8> {
    let mut archive: BackupArchive = serde_json::from_slice(bytes).unwrap();
    let mut snapshot: PortableSnapshot = serde_json::from_str(&archive.content).unwrap();
    change(&mut snapshot.library);
    archive.content = serde_json::to_string(&snapshot).unwrap();
    archive.manifest.content_digest = digest(archive.content.as_bytes());
    archive.manifest.content_size = archive.content.len() as u64;
    serde_json::to_vec(&archive).unwrap()
}

const SETTINGS_OWNER: &str = "mintcat.fixture.settings";
struct FixtureOwners(Mutex<(BTreeMap<String, OwnerSnapshot>, u64)>);
impl FixtureOwners {
    fn new(content: serde_json::Value) -> Self {
        Self(Mutex::new((BTreeMap::from([(SETTINGS_OWNER.into(), OwnerSnapshot {
            schema_version: 1,
            credential_policy: "excluded-reauthenticate-no-live-authority".into(),
            excluded_fields: vec!["credentials".into()],
            content,
        })]), 0)))
    }
}
impl PortableOwners for FixtureOwners {
    fn snapshot(&self) -> Result<BTreeMap<String, OwnerSnapshot>, BackupError> { Ok(self.0.lock().unwrap().0.clone()) }
    fn ensure_empty(&self) -> Result<(), BackupError> {
        if self.0.lock().unwrap().0.values().any(|owner| !owner.content.as_object().is_some_and(|values| values.is_empty())) {
            return Err(BackupError::Busy("fixture settings owner is not empty".into()));
        }
        Ok(())
    }
    fn validate(&self, owners: &BTreeMap<String, OwnerSnapshot>) -> Result<(), BackupError> {
        if owners.len() != 1 || !owners.get(SETTINGS_OWNER).is_some_and(|owner| owner.content.is_object()) {
            return Err(BackupError::Invalid("fixture settings scope or content mismatch".into()));
        }
        Ok(())
    }
    fn write_epochs(&self) -> Result<BTreeMap<String, u64>, BackupError> { Ok(BTreeMap::from([(SETTINGS_OWNER.into(), self.0.lock().unwrap().1)])) }
    fn import_empty(&self, owners: &BTreeMap<String, OwnerSnapshot>) -> Result<(), BackupError> {
        self.ensure_empty()?;
        self.validate(owners)?;
        let mut current = self.0.lock().unwrap();
        current.0 = owners.clone();
        current.1 += 1;
        Ok(())
    }
}
fn settings_state() -> Result<BackupRuntimeState, BackupError> {
    Ok(BackupRuntimeState { blockers: BackupBlockers::default(), modules: BTreeMap::from([
        ("mintcat.library".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 5 }),
        (SETTINGS_OWNER.into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
    ]) })
}
fn export_schema_four_fixture(root: &Path) -> (Vec<u8>, PortableSnapshot) {
    let (_, library, _) = setup(root, true);
    let imported = library.snapshot().unwrap();
    library.set_global_setting("viewMode", "compact").unwrap();
    library.set_game_setting(&imported.games[0].game_id, "viewMode", "expanded").unwrap();
    let connection = rusqlite::Connection::open(library.database_path()).unwrap();
    // Build the actual SQL 4 store, not a schema-5 export with relabelled metadata.
    // Migration 5 is exactly this guard table/index and its schema journal entry.
    connection.execute_batch(
        "DROP TABLE library_profile_content_guards;
         DELETE FROM library_schema WHERE version = 5;
         UPDATE library_import_runs SET imported_at_ms = 0, source_label = 'mintcat-v0.6.sqlite';
         UPDATE library_settings SET created_at_ms = 0, updated_at_ms = 0 WHERE name = 'viewMode';"
    ).unwrap();
    connection.execute("UPDATE library_credentials SET credential_ref = ?1 WHERE credential_id = ?2",
        rusqlite::params!["credential:123e4567-e89b-42d3-a456-426614174000", imported.credentials[0].credential_id]).unwrap();
    assert_eq!(connection.query_row("SELECT MAX(version) FROM library_schema", [], |row| row.get::<_, u32>(0)).unwrap(), 4);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'library_profile_content_guards'", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
    drop(connection);
    let owners = FixtureOwners::new(serde_json::json!({"theme":"dark","showDisabled":true,"sortOrder":["name","version"]}));
    let mut snapshot = PortableSnapshot { library: library.snapshot().unwrap(), owners: owners.snapshot().unwrap() };
    snapshot.exclude_execution_authority().unwrap();
    // Emit the historical format-2/schema-4 envelope directly from SQL 4 data.
    // The current codec must continue exporting schema 5 only.
    let content = serde_json::to_string(&snapshot).unwrap();
    let archive = BackupArchive { manifest: BackupManifest {
        format_version: 2,
        owner_id: "mintcat.backup.portable".into(),
        owner_schema_version: 2,
        module_versions: BTreeMap::from([
            ("mintcat.library".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 4 }),
            (SETTINGS_OWNER.into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
            ("mintcat.source.modio".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
            ("mintcat.ui.search".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
            ("mintcat.game-drg".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
        ]),
        credential_policy: "references-only-reauthenticate-on-import".into(),
        content_digest: digest(content.as_bytes()),
        content_size: content.len() as u64,
        consistency: "per-owner-logical-snapshots-under-common-maintenance-admission".into(),
        owner_schemas: BTreeMap::from([("mintcat.library".into(), 4), (SETTINGS_OWNER.into(), 1)]),
        excluded_authority: vec!["raw-secrets-and-vault-authority".into(), "kernel-jobs-actions-approvals".into(),
            "install-receipts-and-file-journals".into(), "proxy-ca-private-key-hosts-ownership-controller-and-service-leases".into()],
    }, content };
    (serde_json::to_vec(&archive).unwrap(), snapshot)
}
fn rewrite_archive(bytes: &[u8], change: impl FnOnce(&mut BackupArchive)) -> Vec<u8> {
    let mut archive: BackupArchive = serde_json::from_slice(bytes).unwrap();
    change(&mut archive);
    serde_json::to_vec(&archive).unwrap()
}
fn rewrite_owners(bytes: &[u8], change: impl FnOnce(&mut BTreeMap<String, OwnerSnapshot>)) -> Vec<u8> {
    let mut archive: BackupArchive = serde_json::from_slice(bytes).unwrap();
    let mut snapshot: PortableSnapshot = serde_json::from_str(&archive.content).unwrap();
    change(&mut snapshot.owners);
    archive.content = serde_json::to_string(&snapshot).unwrap();
    archive.manifest.content_digest = digest(archive.content.as_bytes());
    archive.manifest.content_size = archive.content.len() as u64;
    serde_json::to_vec(&archive).unwrap()
}

#[test]
fn schema_four_archive_restores_complete_legacy_data_and_owner_settings_into_schema_five() {
    let sandbox = tempdir().unwrap();
    let (bytes, mut expected) = export_schema_four_fixture(&sandbox.path().join("source"));
    let (same_bytes, same_snapshot) = export_schema_four_fixture(&sandbox.path().join("same-source"));
    assert_eq!(bytes, same_bytes);
    assert_eq!(expected, same_snapshot);
    assert!(expected.library.games.len() >= 2 && expected.library.profiles.len() >= 2);
    assert!(expected.library.groups.len() >= 2 && expected.library.mods.len() >= 3 && expected.library.versions.len() >= 3);
    assert!(!expected.library.legacy_mappings.is_empty() && !expected.library.legacy_install_hints.is_empty());
    assert!(!expected.library.import_runs.is_empty() && !expected.library.credentials.is_empty());
    assert!(expected.library.profile_mods.iter().any(|row| !row.enabled));
    assert!(expected.library.profile_mods.iter().any(|row| row.selected_version_id.is_some()));
    let (manifest, decoded) = BackupCodec.decode(&bytes, &settings_state().unwrap().modules).unwrap();
    assert_eq!(manifest.owner_schemas["mintcat.library"], 4);
    assert_eq!(decoded, expected);
    assert!(manifest.module_versions.contains_key("mintcat.source.modio"));
    assert!(!settings_state().unwrap().modules.contains_key("mintcat.source.modio"));
    assert!(!settings_state().unwrap().modules.contains_key("mintcat.ui.search"));
    assert!(BackupCodec.encode(&expected, &manifest.module_versions).is_err());
    let (target, library, layout) = setup(&sandbox.path().join("target"), false);
    let owners = Arc::new(FixtureOwners::new(serde_json::json!({})));
    let target = target.with_owners(owners.clone());
    let result = target.restore_bytes(&bytes, settings_state).unwrap();
    expected.clear_credentials();
    assert_eq!(result.manifest, manifest);
    assert_eq!(result.counts, expected.counts());
    assert_eq!(library.snapshot().unwrap(), expected.library);
    assert_eq!(owners.snapshot().unwrap(), expected.owners);
    assert_eq!(library.get_global_setting("viewMode").unwrap().as_deref(), Some("compact"));
    assert_eq!(library.get_game_setting(&expected.library.games[0].game_id, "viewMode").unwrap().as_deref(), Some("expanded"));
    let database = rusqlite::Connection::open(library.database_path()).unwrap();
    assert_eq!(database.query_row("SELECT MAX(version) FROM library_schema", [], |row| row.get::<_, u32>(0)).unwrap(), 5);
    assert_eq!(database.query_row("SELECT COUNT(*) FROM library_profile_content_guards", [], |row| row.get::<_, u32>(0)).unwrap(), 0);
    drop(database);
    assert!(MaintenanceGate::enter_mutation(&layout).is_ok());
    let current = target.export_bytes(settings_state).unwrap();
    let (current_manifest, current_snapshot) = BackupCodec.decode(&current, &settings_state().unwrap().modules).unwrap();
    assert_eq!(current_manifest.format_version, 2);
    assert_eq!(current_manifest.owner_schemas["mintcat.library"], 5);
    assert_eq!(current_manifest.module_versions["mintcat.library"].schema_version, 5);
    assert_eq!(current_snapshot, expected);
    assert!(target.restore_bytes(&bytes, settings_state).is_err());
    assert_eq!(library.snapshot().unwrap(), expected.library);
    assert_eq!(owners.snapshot().unwrap(), expected.owners);
}

#[test]
fn schema_four_compatibility_rejects_mismatches_credentials_and_execution_authority_without_writes() {
    let sandbox = tempdir().unwrap();
    let (bytes, _) = export_schema_four_fixture(&sandbox.path().join("source"));
    let (target, library, layout) = setup(&sandbox.path().join("target"), false);
    let owners = Arc::new(FixtureOwners::new(serde_json::json!({})));
    let target = target.with_owners(owners.clone());
    let empty_owners = owners.snapshot().unwrap();
    let mut rejected = vec![
        rewrite_archive(&bytes, |archive| { archive.manifest.owner_schemas.insert("mintcat.library".into(), 5); }),
        rewrite_archive(&bytes, |archive| archive.manifest.module_versions.get_mut("mintcat.library").unwrap().schema_version = 5),
        rewrite_archive(&bytes, |archive| archive.manifest.module_versions.get_mut("mintcat.library").unwrap().version.clear()),
        rewrite_archive(&bytes, |archive| archive.manifest.format_version = 1),
        rewrite_archive(&bytes, |archive| archive.manifest.owner_schema_version = 4),
        rewrite_archive(&bytes, |archive| archive.manifest.credential_policy = "include-secrets".into()),
        rewrite_archive(&bytes, |archive| archive.manifest.content_size += 1),
        rewrite_archive(&bytes, |archive| archive.content.push(' ')),
        rewrite_archive(&bytes, |archive| { archive.manifest.owner_schemas.insert("mintcat.unknown-owner".into(), 1); archive.manifest.module_versions.insert("mintcat.unknown-owner".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }); }),
        rewrite_owners(&bytes, |owners| owners.get_mut(SETTINGS_OWNER).unwrap().schema_version = 2),
        rewrite_owners(&bytes, |owners| owners.get_mut(SETTINGS_OWNER).unwrap().excluded_fields.clear()),
        rewrite_owners(&bytes, |owners| owners.get_mut(SETTINGS_OWNER).unwrap().content = serde_json::json!({"apiKey":"private-test-value"})),
        rewrite_snapshot(&bytes, |snapshot| snapshot.credentials[0].credential_ref = Some("private-test-value".into())),
        rewrite_snapshot(&bytes, |snapshot| snapshot.settings[0].value = "{\"accessToken\":\"private-test-value\"}".into()),
        rewrite_snapshot(&bytes, |snapshot| snapshot.outbox.push(mintcat_library_service::OutboxRecord {
            event_seq: 1, event_id: "forged-event".into(), operation_id: None, event_type: "fixture".into(), destination: "kernel".into(),
            payload_json: "{}".into(), delivery_state: "pending".into(), delivery_ref: None, created_at_ms: 0, delivered_at_ms: None,
        })),
    ];
    for schema in [0, 1, 2, 3, 6] {
        rejected.push(rewrite_archive(&bytes, |archive| {
            archive.manifest.owner_schemas.insert("mintcat.library".into(), schema);
            archive.manifest.module_versions.get_mut("mintcat.library").unwrap().schema_version = schema;
        }));
    }
    for invalid in rejected {
        assert!(target.restore_bytes(&invalid, settings_state).is_err());
        assert_eq!(library.snapshot().unwrap(), LibrarySnapshot::default());
        assert_eq!(owners.snapshot().unwrap(), empty_owners);
        assert!(MaintenanceGate::enter_mutation(&layout).is_ok());
    }
    for (id, schema) in [("mintcat.library", 4), ("mintcat.library", 6), (SETTINGS_OWNER, 2)] {
        assert!(target.restore_bytes(&bytes, || { let mut state = settings_state()?; state.modules.get_mut(id).unwrap().schema_version = schema; Ok(state) }).is_err());
    }
    for id in ["mintcat.library", SETTINGS_OWNER] {
        assert!(target.restore_bytes(&bytes, || { let mut state = settings_state()?; state.modules.remove(id); Ok(state) }).is_err());
    }
    assert_eq!(library.snapshot().unwrap(), LibrarySnapshot::default());
    assert_eq!(owners.snapshot().unwrap(), empty_owners);
    assert!(MaintenanceGate::enter_mutation(&layout).is_ok());
    let (nonempty, empty_library, nonempty_layout) = setup(&sandbox.path().join("nonempty-owner"), false);
    let nonempty_owners = Arc::new(FixtureOwners::new(serde_json::json!({"theme":"existing"})));
    let before = nonempty_owners.snapshot().unwrap();
    assert!(nonempty.with_owners(nonempty_owners.clone()).restore_bytes(&bytes, settings_state).is_err());
    assert_eq!(empty_library.snapshot().unwrap(), LibrarySnapshot::default());
    assert_eq!(nonempty_owners.snapshot().unwrap(), before);
    assert!(MaintenanceGate::enter_mutation(&nonempty_layout).is_ok());
}
#[test]
fn offline_fixture_round_trip_preserves_order_and_selected_versions_and_never_overwrites() {
    let sandbox = tempdir().unwrap();
    let (source, original, _) = setup(&sandbox.path().join("source"), true);
    let mut before = original.snapshot().unwrap();
    // Imported legacy receipts remain source-local authority, not restored user data.
    before.mutation_results.clear();
    before.outbox.clear();
    before.operation_leases.clear();
    let target = sandbox.path().join("backup.json");
    source.export(ExportBackupRequest { target_path: target.clone() }, state).unwrap();
    let bytes = fs::read(&target).unwrap();
    assert!(source.export(ExportBackupRequest { target_path: target.clone() }, state).is_err());
    assert_eq!(fs::read(&target).unwrap(), bytes);
    let (destination, restored, layout) = setup(&sandbox.path().join("restored"), false);
    destination.restore(RestoreBackupRequest { source_path: target }, state).unwrap();
    let after = restored.snapshot().unwrap();
    assert_eq!(after, before);
    let memberships: Vec<_> = after.profile_mods.iter().filter(|row| row.profile_id == "profile:legacy:1").map(|row| (row.sort_order, row.enabled, row.used_version.as_deref(), row.selected_version_id.as_deref())).collect();
    assert_eq!(memberships[0].0, 0);
    assert_eq!(memberships[0].2, Some("1.0.0"));
    assert_eq!(memberships[1].0, 1);
    assert!(!memberships[1].1);
    assert!(MaintenanceGate::enter_mutation(&layout).is_ok());
    restored.set_global_setting("releaseChannel", "stable").unwrap();
    let newer = restored.snapshot().unwrap();
    assert!(destination.restore_bytes(&bytes, state).is_err());
    assert_eq!(restored.snapshot().unwrap(), newer);
}
#[test]
fn digest_missing_owner_new_schema_and_credentials_reject_without_mutation() {
    let sandbox = tempdir().unwrap();
    let (source, _, _) = setup(&sandbox.path().join("source"), true);
    let bytes = source.export_bytes(state).unwrap();
    let (destination, library, layout) = setup(&sandbox.path().join("target"), false);
    let mut archive: BackupArchive = serde_json::from_slice(&bytes).unwrap();
    archive.content.push(' ');
    assert!(destination.restore_bytes(&serde_json::to_vec(&archive).unwrap(), state).is_err());
    let mut archive: BackupArchive = serde_json::from_slice(&bytes).unwrap();
    archive.manifest.owner_schema_version = 6;
    assert!(destination.restore_bytes(&serde_json::to_vec(&archive).unwrap(), state).is_err());
    assert!(destination.restore_bytes(&bytes, || { let mut state = state()?; state.modules.remove("mintcat.library"); Ok(state) }).is_err());
    for contaminated in [
        rewrite_snapshot(&bytes, |snapshot| snapshot.credentials[0].credential_ref = Some("private-token-value".into())),
        rewrite_snapshot(&bytes, |snapshot| snapshot.settings[0].value = "{\"accessToken\":\"private-token-value\"}".into()),
        rewrite_snapshot(&bytes, |snapshot| snapshot.settings[0].value = "-----BEGIN PRIVATE KEY----- secret".into()),
    ] {
        assert!(destination.restore_bytes(&contaminated, state).is_err());
        assert_eq!(library.snapshot().unwrap(), LibrarySnapshot::default());
        assert!(MaintenanceGate::enter_mutation(&layout).is_ok());
    }
}
#[test]
fn running_tasks_content_guards_and_mutation_admission_block_restore() {
    let sandbox = tempdir().unwrap();
    let (source, source_library, _) = setup(&sandbox.path().join("source"), true);
    let bytes = source.export_bytes(state).unwrap();
    let (destination, library, layout) = setup(&sandbox.path().join("target"), false);
    let admission = MaintenanceGate::enter_mutation(&layout).unwrap();
    assert!(matches!(destination.restore_bytes(&bytes, state), Err(BackupError::Busy(_))));
    drop(admission);
    assert!(matches!(destination.restore_bytes(&bytes, || { let mut state = state()?; state.blockers.unfinished_install_jobs.push("install-job".into()); Ok(state) }), Err(BackupError::Busy(_))));
    assert_eq!(library.snapshot().unwrap(), LibrarySnapshot::default());
    let snapshot = source_library.snapshot().unwrap();
    let profile = &snapshot.profiles[0];
    let auth = LibraryAuthorization::new(&profile.owner_scope, [CONTENT_WRITE_PERMISSION]);
    source_library.acquire_profile_content_guard(&auth, "install-guard", &profile.profile_id, profile.content_revision, &profile.incarnation_id, "generation").unwrap();
    let busy = source_library.snapshot().unwrap();
    assert!(matches!(source.restore_bytes(&bytes, state), Err(BackupError::Library(LibraryError::ResourceBusy { .. }))));
    assert_eq!(source_library.snapshot().unwrap(), busy);
}
#[test]
fn setting_only_newer_writes_and_failed_import_are_not_automatic_rollback() {
    let sandbox = tempdir().unwrap();
    let (source, _, _) = setup(&sandbox.path().join("source"), true);
    let bytes = source.export_bytes(state).unwrap();
    let (target, library, layout) = setup(&sandbox.path().join("target"), false);
    library.set_global_setting("releaseChannel", "stable").unwrap();
    let before = library.snapshot().unwrap();
    assert!(target.restore_bytes(&bytes, state).is_err());
    assert_eq!(library.snapshot().unwrap(), before);
    let (empty_target, empty_library, empty_layout) = setup(&sandbox.path().join("empty"), false);
    let invalid = rewrite_snapshot(&bytes, |snapshot| snapshot.profile_mods[0].profile_id = "missing-profile".into());
    assert!(empty_target.restore_bytes(&invalid, state).is_err());
    assert_eq!(empty_library.snapshot().unwrap(), LibrarySnapshot::default());
    assert!(matches!(MaintenanceGate::enter_mutation(&empty_layout), Err(BackupError::RecoveryRequired)));
    assert!(empty_target.recover_maintenance(state).unwrap());
    assert!(MaintenanceGate::enter_mutation(&empty_layout).is_ok());
    assert!(MaintenanceGate::enter_mutation(&layout).is_ok());
}
#[test]
fn recovery_refuses_changed_postcommit_owner() {
    let sandbox = tempdir().unwrap();
    let (service, library, layout) = setup(sandbox.path(), false);
    let expected = digest(&serde_json::to_vec(&LibrarySnapshot::default()).unwrap());
    let gate = MaintenanceGate::exclusive(&layout, false).unwrap();
    gate.mark(&MaintenanceMarker { format_version: 1, owner_id: "mintcat.library".into(), operation_id: Some("fixture-restore".into()), phase: MaintenancePhase::RestoreCommitted, snapshot_digest: expected }).unwrap();
    drop(gate);
    library.set_global_setting("releaseChannel", "newer").unwrap();
    assert!(matches!(service.recover_maintenance(state), Err(BackupError::RecoveryRequired)));
    assert!(matches!(MaintenanceGate::enter_mutation(&layout), Err(BackupError::RecoveryRequired)));
}

#[test]
fn portable_credential_references_require_reauthentication_and_inline_tokens_never_export() {
    let sandbox = tempdir().unwrap();
    let (source, source_library, _) = setup(&sandbox.path().join("source"), true);
    let bytes = source.export_bytes(state).unwrap();
    let referenced = rewrite_snapshot(&bytes, |snapshot| {
        snapshot.credentials[0].credential_ref = Some("credential:123e4567-e89b-42d3-a456-426614174000".into());
    });
    let (target, target_library, _) = setup(&sandbox.path().join("target"), false);
    target.restore_bytes(&referenced, state).unwrap();
    let restored = target_library.snapshot().unwrap();
    assert!(restored.credentials.iter().all(|credential| credential.credential_ref.is_none()));
    assert_eq!(restored.credentials[0].provider, source_library.snapshot().unwrap().credentials[0].provider);
    source_library.set_global_setting("mintcatAccessToken", "never-export-this-token").unwrap();
    let output = sandbox.path().join("unsafe.json");
    assert!(source.export(ExportBackupRequest { target_path: output.clone() }, state).is_err());
    assert!(!output.exists());
}

#[test]
fn durable_running_download_blocks_export_and_restore_without_changing_owner() {
    let sandbox = tempdir().unwrap();
    let (service, library, _) = setup(sandbox.path(), true);
    let bytes = service.export_bytes(state).unwrap();
    let snapshot = library.snapshot().unwrap();
    let profile = &snapshot.profiles[0];
    let connection = rusqlite::Connection::open(library.database_path()).unwrap();
    connection.execute(
        "INSERT INTO library_download_operations(
            operation_id, owner_scope, idempotency_scope, idempotency_key, inputs_hash,
            profile_id, mode, expected_content_revision, target_incarnation_id,
            owner_generation, fence_epoch, state, created_at_ms, updated_at_ms)
         VALUES ('running-download', ?1, 'fixture', 'download-key', 'fixture-hash',
                 ?2, 'cache-only', ?3, ?4, 'fixture-generation', 1, 'running', 0, 0)",
        rusqlite::params![profile.owner_scope, profile.profile_id, profile.content_revision, profile.incarnation_id],
    ).unwrap();
    drop(connection);
    let running = library.snapshot().unwrap();
    assert!(matches!(service.export_bytes(state), Err(BackupError::Library(LibraryError::ResourceBusy { .. }))));
    assert!(matches!(service.restore_bytes(&bytes, state), Err(BackupError::Library(LibraryError::ResourceBusy { .. }))));
    assert_eq!(library.snapshot().unwrap(), running);
}

#[test]
fn backup_recovery_never_clears_other_owner_or_migration_markers() {
    let sandbox = tempdir().unwrap();
    for (index, (owner, phase)) in [
        ("mintcat.library", MaintenancePhase::PreparingMigration),
        ("mintcat.library", MaintenancePhase::MigratedReadOnly),
        ("mintcat.library", MaintenancePhase::NewWrites),
        ("mintcat.other-owner", MaintenancePhase::RestoreCommitted),
    ].into_iter().enumerate() {
        let (service, library, layout) = setup(&sandbox.path().join(index.to_string()), false);
        let marker = MaintenanceMarker {
            format_version: 1,
            owner_id: owner.into(),
            operation_id: Some("fixture-migration".into()),
            phase,
            snapshot_digest: digest(&serde_json::to_vec(&LibrarySnapshot::default()).unwrap()),
        };
        let gate = MaintenanceGate::exclusive(&layout, false).unwrap();
        gate.mark(&marker).unwrap();
        drop(gate);
        assert!(matches!(service.recover_maintenance(state), Err(BackupError::RecoveryRequired)));
        assert_eq!(library.snapshot().unwrap(), LibrarySnapshot::default());
        assert!(matches!(MaintenanceGate::enter_mutation(&layout), Err(BackupError::RecoveryRequired)));
        let gate = MaintenanceGate::exclusive(&layout, true).unwrap();
        assert_eq!(gate.read().unwrap(), Some(marker));
    }
}
