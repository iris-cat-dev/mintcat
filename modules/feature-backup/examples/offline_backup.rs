use std::{collections::BTreeMap, path::Path};
use mintcat_feature_backup::{BackupBlockers, BackupError, BackupRuntimeState, BackupService, ExportBackupRequest, MaintenanceGate, ModuleVersion, RestoreBackupRequest};
use mintcat_library_service::{CredentialVault, LegacyCredential, LibraryAuthorization, LibraryStore, CONTENT_WRITE_PERMISSION};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};

struct FixtureVault;
impl CredentialVault for FixtureVault {
    fn store(&self, _: LegacyCredential<'_>) -> Result<String, String> { Err("offline fixture unexpectedly contains a token".into()) }
    fn delete(&self, _: &str) -> Result<(), String> { Ok(()) }
}
fn state() -> Result<BackupRuntimeState, BackupError> {
    Ok(BackupRuntimeState { blockers: BackupBlockers::default(), modules: BTreeMap::from([
        ("mintcat.library".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 5 }),
        ("mintcat.game-drg".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
        ("mintcat.game-rogue-core".into(), ModuleVersion { version: "0.1.0".into(), schema_version: 1 }),
    ]) })
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sandbox = tempfile::tempdir()?;
    let source_layout = RuntimePaths::new(sandbox.path().join("source"))?.initialize(RuntimeEnvironment::Test)?;
    let source = LibraryStore::open(&source_layout)?;
    source.import_legacy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/baseline/legacy/mintcat-v0.6.sqlite"), &FixtureVault)?;
    let before = source.snapshot()?;
    let backup = sandbox.path().join("offline-backup.json");
    BackupService::new(source_layout, source)?.export(ExportBackupRequest { target_path: backup.clone() }, state)?;
    let destination_layout = RuntimePaths::new(sandbox.path().join("restored"))?.initialize(RuntimeEnvironment::Test)?;
    let destination = LibraryStore::open(&destination_layout)?;
    let service = BackupService::new(destination_layout.clone(), destination.clone())?;
    service.restore(RestoreBackupRequest { source_path: backup.clone() }, state)?;
    let restored = destination.snapshot()?;
    assert_eq!(restored, before);
    for membership in &restored.profile_mods {
        println!("profile={} mod={} order={} enabled={} usedVersion={:?} selectedVersion={:?}", membership.profile_id, membership.mod_id, membership.sort_order, membership.enabled, membership.used_version, membership.selected_version_id);
    }
    let admission = MaintenanceGate::enter_mutation(&destination_layout)?;
    assert!(matches!(service.restore(RestoreBackupRequest { source_path: backup.clone() }, state), Err(BackupError::Busy(_))));
    drop(admission);
    let profile = &restored.profiles[0];
    destination.acquire_profile_content_guard(&LibraryAuthorization::new(&profile.owner_scope, [CONTENT_WRITE_PERMISSION]), "smoke-install", &profile.profile_id, profile.content_revision, &profile.incarnation_id, "smoke-generation")?;
    assert!(matches!(service.restore(RestoreBackupRequest { source_path: backup }, state), Err(BackupError::Library(mintcat_library_service::LibraryError::ResourceBusy { .. }))));
    println!("offline restore retained {} profiles and {} ordered memberships; mutation admission and active install guard both rejected restore", restored.profiles.len(), restored.profile_mods.len());
    Ok(())
}
