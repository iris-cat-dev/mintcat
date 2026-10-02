use super::*;
use mintcat_library_service::{CredentialVault, LegacyCredential, LibraryAuthorization, CONTENT_WRITE_PERMISSION};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use tempfile::tempdir;

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
fn digest_missing_module_new_schema_and_credentials_reject_without_mutation() {
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
    assert!(destination.restore_bytes(&bytes, || { let mut state = state()?; state.modules.remove("mintcat.game-drg"); Ok(state) }).is_err());
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
