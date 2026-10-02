use std::{fs, path::Path};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_library_service::{GameDefinition, LibraryAuthorization, LibraryError, LibrarySnapshot, LibraryStore, ProfileRecord, METADATA_WRITE_PERMISSION, READ_PERMISSION, module_manifest};
use mintcat_runtime_artifacts::{ArtifactStore, TrustStore, module_signing_digest};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimeLayout, RuntimePaths};
use mintcat_sdk::{ModuleFile, ModuleSignature, ProfilesListParams, SettingsGetParams, SettingsScope, SettingsUpdateParams, SignatureAlgorithm};
use sha2::{Digest, Sha256};

struct Fixture { root:tempfile::TempDir, layout:RuntimeLayout, artifacts:ArtifactStore, key:SigningKey }
impl Fixture {
    fn new()->Self {
        let root=tempfile::tempdir().unwrap();let layout=RuntimePaths::new(root.path().join("data")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let key=SigningKey::from_bytes(&[74;32]);let mut trust=TrustStore::default();trust.trust_production_key("mintcat.official","fixture",key.verifying_key());
        let artifacts=ArtifactStore::from_layout(&layout,RuntimeEnvironment::Test,target_lexicon::HOST.to_string(),"1",trust).unwrap();
        Self {root,layout,artifacts,key}
    }
    fn package(&self,version:&str)->String {
        let source=self.root.path().join(version);fs::create_dir(&source).unwrap();
        let bytes=fs::read(env!("CARGO_BIN_EXE_mintcat-library-worker")).unwrap();fs::write(source.join("worker"),&bytes).unwrap();
        let mut manifest=module_manifest(&target_lexicon::HOST.to_string(),"1",ModuleFile {path:"worker".into(),size:bytes.len() as u64,sha256:format!("sha256:{:x}",Sha256::digest(&bytes)),executable:true});manifest.version=version.into();
        let digest=module_signing_digest(&source,&manifest).unwrap();
        let signature=ModuleSignature {schema_version:1,publisher:"mintcat.official".into(),key_id:"fixture".into(),algorithm:SignatureAlgorithm::Ed25519,signature:STANDARD.encode(self.key.sign(&digest).to_bytes())};
        self.artifacts.publish_module(&source,&manifest,&signature).unwrap().digest
    }
    fn connect(&self,digest:&str)->LibraryStore {LibraryStore::connect(self.artifacts.clone(),digest,&self.layout).unwrap()}
}
#[test]
fn signed_native_swap_preserves_data_closes_admission_and_excludes_second_writer() {
    let fixture=Fixture::new();let first=fixture.package("1.0.0");let second=fixture.package("2.0.0");
    let library=fixture.connect(&first);
    library.read_only_health().unwrap();assert!(!library.database_path().exists());
    assert!(matches!(library.get_global_setting("draft"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    assert!(matches!(library.set_global_setting("draft","blocked"),Err(LibraryError::ResourceBusy {..})));
    library.open_writes().unwrap();
    library.import_empty_snapshot(&LibrarySnapshot {
        games:vec![GameDefinition {game_id:"game:probation".into(),game_key:"probation".into(),display_name:"Probation Game".into(),icon:String::new(),active:true,created_at_ms:1,updated_at_ms:1}],
        profiles:vec![ProfileRecord {profile_id:"profile:retained".into(),game_id:"game:probation".into(),owner_scope:"human".into(),name:"Retained".into(),display_name:"Retained Profile".into(),active:true,description:"canonical data".into(),revision:3,content_revision:2,lease_epoch:0,incarnation_id:"retained-incarnation".into(),last_used_at_ms:1,created_at_ms:1,updated_at_ms:1,deleted_at_ms:None}],
        ..LibrarySnapshot::default()
    }).unwrap();
    library.set_global_setting("draft","retained").unwrap();
    let authorization=LibraryAuthorization::new("human",[READ_PERMISSION,METADATA_WRITE_PERMISSION]);
    let profiles_request=ProfilesListParams {game_id:"game:probation".into(),cursor:None,page_size:10};
    let settings_request=SettingsGetParams {scope:SettingsScope::Value(serde_json::json!({"kind":"global"})),section:"appearance".into()};
    let settings_update=SettingsUpdateParams {scope:settings_request.scope.clone(),section:settings_request.section.clone(),expected_revision:0,values:serde_json::json!({"theme":"dark"})};
    let settings=library.update_settings(&authorization,"appearance-first",&settings_update,None).unwrap();
    let profiles=library.list_profiles(&authorization,&profiles_request).unwrap();
    assert_eq!(profiles.items.iter().map(|profile|profile.profile_id.as_str()).collect::<Vec<_>>(),["profile:retained"]);
    let before=library.native_identity().unwrap();assert_eq!(before.module_version,"1.0.0");
    let other=fixture.connect(&first);other.read_only_health().unwrap();
    assert_eq!(other.get_global_setting("draft").unwrap().as_deref(),Some("retained"));
    assert!(matches!(other.open_writes(),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-writer"));
    assert!(matches!(other.get_global_setting("draft"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));drop(other);
    library.pause().unwrap();let snapshot=library.backup_snapshot().unwrap();let epoch=library.write_epoch().unwrap();
    let database=fs::read(library.database_path()).unwrap();
    assert!(matches!(library.get_global_setting("draft"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    library.read_only_health().unwrap();
    assert_eq!(library.get_global_setting("draft").unwrap().as_deref(),Some("retained"));
    assert_eq!(library.list_profiles(&authorization,&profiles_request).unwrap(),profiles);
    assert_eq!(library.get_settings(&authorization,&settings_request).unwrap(),settings);
    assert_eq!(fs::read(library.database_path()).unwrap(),database);assert_eq!(library.write_epoch().unwrap(),epoch);
    library.replace_native(fixture.artifacts.clone(),&second).unwrap();
    assert!(matches!(library.list_profiles(&authorization,&profiles_request),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    library.read_only_health().unwrap();
    let after=library.native_identity().unwrap();assert_eq!(after.module_version,"2.0.0");assert_eq!(after.module_digest,second);assert_ne!(after.process_id,before.process_id);assert_ne!(after.instance_id,before.instance_id);
    assert_eq!(library.get_global_setting("draft").unwrap().as_deref(),Some("retained"));
    assert_eq!(library.list_profiles(&authorization,&profiles_request).unwrap(),profiles);
    assert_eq!(library.get_settings(&authorization,&settings_request).unwrap(),settings);
    let denied=LibraryAuthorization::new("human",Vec::<String>::new());
    assert!(matches!(library.list_profiles(&denied,&profiles_request),Err(LibraryError::PermissionDenied(_))));
    assert_eq!(library.backup_snapshot().unwrap(),snapshot);
    assert!(matches!(library.set_global_setting("draft","probation"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    assert!(matches!(library.update_settings(&authorization,"appearance-probation",&settings_update,None),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    assert_eq!(library.write_epoch().unwrap(),epoch);assert_eq!(fs::read(library.database_path()).unwrap(),database);
    library.open_writes().unwrap();assert_eq!(library.get_global_setting("draft").unwrap().as_deref(),Some("retained"));
    library.set_global_setting("draft","new-generation").unwrap();assert!(library.write_epoch().unwrap()>epoch);
    library.pause().unwrap();library.replace_native(fixture.artifacts.clone(),&first).unwrap();library.read_only_health().unwrap();library.open_writes().unwrap();
    assert_eq!(library.get_global_setting("draft").unwrap().as_deref(),Some("new-generation"));
}
#[test]
fn giant_snapshot_is_transferred_as_verified_artifact_not_worker_frame() {
    let fixture=Fixture::new();let digest=fixture.package("1.0.0");let library=fixture.connect(&digest);library.open_writes().unwrap();
    let value="x".repeat(2*1024*1024);library.set_global_setting("large","x").unwrap();library.set_global_setting("large",&value).unwrap();
    let snapshot=library.snapshot().unwrap();assert_eq!(snapshot.settings.iter().find(|s|s.name=="large").unwrap().value,value);
    library.pause().unwrap();assert_eq!(library.backup_snapshot().unwrap(),snapshot);
}
#[test]
fn unsupported_schema_health_rejects_without_initialization_or_database_mutation() {
    let fixture=Fixture::new();let digest=fixture.package("1.0.0");let library=fixture.connect(&digest);library.open_writes().unwrap();library.set_global_setting("retained","value").unwrap();library.pause().unwrap();
    library.read_only_health().unwrap();assert_eq!(library.get_global_setting("retained").unwrap().as_deref(),Some("value"));
    let connection=rusqlite::Connection::open(library.database_path()).unwrap();connection.execute("INSERT INTO library_schema VALUES(6,0)",[]).unwrap();drop(connection);
    let bytes=fs::read(library.database_path()).unwrap();
    assert!(matches!(library.read_only_health(),Err(LibraryError::UnsupportedSchema(6))));
    assert_eq!(fs::read(library.database_path()).unwrap(),bytes);
    assert!(matches!(library.get_global_setting("retained"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    assert!(matches!(library.set_global_setting("retained","blocked"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
    assert!(matches!(library.open_writes(),Err(LibraryError::UnsupportedSchema(6))));assert_eq!(fs::read(library.database_path()).unwrap(),bytes);
}

#[test]
fn old_schema_and_current_schema_integrity_failures_close_read_admission() {
    for old_schema in [true,false] {
        let fixture=Fixture::new();let digest=fixture.package("1.0.0");let library=fixture.connect(&digest);
        library.open_writes().unwrap();library.set_global_setting("retained","value").unwrap();library.pause().unwrap();
        library.read_only_health().unwrap();assert_eq!(library.get_global_setting("retained").unwrap().as_deref(),Some("value"));
        if old_schema {
            let connection=rusqlite::Connection::open(library.database_path()).unwrap();
            connection.execute("DELETE FROM library_schema WHERE version=5",[]).unwrap();
        } else {
            // Destroy database structure while retaining the SQLite header.
            fs::OpenOptions::new().write(true).open(library.database_path()).unwrap().set_len(100).unwrap();
        }
        let bytes=fs::read(library.database_path()).unwrap();let epoch=library.write_epoch().unwrap();
        let health=library.read_only_health();
        if old_schema {assert!(matches!(health,Err(LibraryError::UnsupportedSchema(4))));}
        else {assert!(matches!(health,Err(LibraryError::InvalidRecord(_))));}
        assert!(matches!(library.get_global_setting("retained"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
        assert!(matches!(library.set_global_setting("retained","blocked"),Err(LibraryError::ResourceBusy {owner_operation_id}) if owner_operation_id=="library-paused"));
        let admission=library.open_writes();
        if old_schema {assert!(matches!(admission,Err(LibraryError::UnsupportedSchema(4))));}
        else {assert!(matches!(admission,Err(LibraryError::InvalidRecord(_))));}
        assert_eq!(fs::read(library.database_path()).unwrap(),bytes);assert_eq!(library.write_epoch().unwrap(),epoch);
    }
}
#[test]
fn absent_approved_owner_never_falls_back_to_sqlite() {
    let fixture=Fixture::new();let library=LibraryStore::unavailable(&fixture.layout);
    assert!(matches!(library.snapshot(),Err(LibraryError::OwnerUnavailable(_))));assert!(!library.database_path().exists());
    assert!(matches!(LibraryStore::connect(fixture.artifacts.clone(),&format!("sha256:{}","0".repeat(64)),&fixture.layout),Err(LibraryError::OwnerUnavailable(_))));assert!(!library.database_path().exists());
}

#[test]
fn deactivation_removes_shared_backend_without_initializing_or_erasing_data() {
    let fixture=Fixture::new();let digest=fixture.package("1.0.0");
    let library=fixture.connect(&digest);let retained_consumer=library.clone();
    library.read_only_health().unwrap();library.pause().unwrap();library.disconnect().unwrap();
    assert!(matches!(retained_consumer.open_writes(),Err(LibraryError::OwnerUnavailable(_))));
    assert!(matches!(retained_consumer.snapshot(),Err(LibraryError::OwnerUnavailable(_))));
    assert!(!library.database_path().exists());
    library.replace_native(fixture.artifacts.clone(),&digest).unwrap();library.open_writes().unwrap();
    library.set_global_setting("preserve","user-data").unwrap();library.pause().unwrap();
    let before=fs::read(library.database_path()).unwrap();
    library.disconnect().unwrap();assert_eq!(fs::read(library.database_path()).unwrap(),before);
    assert!(matches!(retained_consumer.snapshot(),Err(LibraryError::OwnerUnavailable(_))));
    let reactivated=fixture.connect(&digest);reactivated.read_only_health().unwrap();reactivated.open_writes().unwrap();
    assert_eq!(reactivated.get_global_setting("preserve").unwrap().as_deref(),Some("user-data"));
}
#[test]
fn readonly_legacy_snapshot_includes_committed_wal_and_preserves_source() {
    let root=tempfile::tempdir().unwrap();let source=root.path().join("legacy.sqlite");let target=root.path().join("copy.sqlite");
    let writer=rusqlite::Connection::open(&source).unwrap();writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE retained(value TEXT); INSERT INTO retained VALUES('from-wal');").unwrap();
    let before=fs::read(&source).unwrap();let wal_path=Path::new(&source).with_file_name("legacy.sqlite-wal");let wal=fs::read(&wal_path).unwrap();
    LibraryStore::snapshot_legacy_source(&source,&target).unwrap();
    let copy=rusqlite::Connection::open_with_flags(&target,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();let value:String=copy.query_row("SELECT value FROM retained",[],|r|r.get(0)).unwrap();assert_eq!(value,"from-wal");
    assert_eq!(fs::read(&source).unwrap(),before);assert_eq!(fs::read(wal_path).unwrap(),wal);
}

#[test]
fn credential_import_uses_genuine_approved_vault_and_atomic_outcome_survives_restart() {
    use mintcat_library_service::{CredentialVault, LegacyCredential};
    struct Denied;
    impl CredentialVault for Denied {
        fn store(&self,_:LegacyCredential<'_>)->Result<String,String> {Err("do not expose raw-test-secret".into())}
        fn delete(&self,_:&str)->Result<(),String> {Err("denied".into())}
    }
    #[derive(Default)]
    struct Vault(std::cell::RefCell<std::collections::HashMap<String,String>>);
    impl CredentialVault for Vault {
        fn store(&self,credential:LegacyCredential<'_>)->Result<String,String> {
            let reference=format!("keychain://fixture/{}",credential.provider);
            self.0.borrow_mut().insert(reference.clone(),credential.secret.into());Ok(reference)
        }
        fn delete(&self,reference:&str)->Result<(),String> {self.0.borrow_mut().remove(reference);Ok(())}
    }
    let fixture=Fixture::new();let digest=fixture.package("1.0.0");let library=fixture.connect(&digest);library.open_writes().unwrap();
    let source=fixture.root.path().join("approved.sqlite");
    fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/baseline/legacy/mintcat-v0.6.sqlite"),&source).unwrap();
    let approved_digest=LibraryStore::legacy_source_digest(&source).unwrap();
    let legacy=rusqlite::Connection::open(&source).unwrap();
    legacy.execute("UPDATE oauths SET oauth='raw-test-secret' WHERE platform='mod.io'",[]).unwrap();drop(legacy);
    let before=fs::read(&source).unwrap();
    assert!(matches!(library.import_legacy_checked(&source,&approved_digest,&Denied),Err(LibraryError::StaleOperation)));
    assert_eq!(library.write_epoch().unwrap(),0);
    let error=library.import_legacy(&source,&Denied).unwrap_err();assert!(matches!(error,LibraryError::CredentialVault {..}));assert!(!error.to_string().contains("raw-test-secret"));
    assert_eq!(library.snapshot().unwrap(),mintcat_library_service::LibrarySnapshot::default());
    let source_digest=LibraryStore::legacy_source_digest(&source).unwrap();
    let vault=Vault::default();let report=library.import_legacy_checked(&source,&source_digest,&vault).unwrap();
    assert_eq!(vault.0.borrow().get("keychain://fixture/modio").map(String::as_str),Some("raw-test-secret"));
    assert_eq!(fs::read(&source).unwrap(),before);
    let outcome=library.legacy_import_outcome(&source_digest).unwrap().unwrap();
    assert_eq!(outcome.result,report);assert_eq!(outcome.write_epoch,library.write_epoch().unwrap());
    let snapshot=library.snapshot().unwrap();
    assert_eq!(outcome.snapshot_digest,format!("sha256:{:x}",Sha256::digest(serde_json::to_vec(&snapshot).unwrap())));
    library.pause().unwrap();drop(library);
    let restarted=fixture.connect(&digest);assert_eq!(restarted.legacy_import_outcome(&source_digest).unwrap().unwrap(),outcome);
    restarted.open_writes().unwrap();
    assert!(restarted.import_legacy(&source,&vault).unwrap().already_imported);
    restarted.set_global_setting("after-import","newer-data").unwrap();
    assert!(matches!(restarted.legacy_import_outcome(&source_digest),Err(LibraryError::StaleOperation)));
    assert!(matches!(restarted.import_legacy(&source,&vault),Err(LibraryError::StaleOperation)));
    assert_eq!(restarted.get_global_setting("after-import").unwrap().as_deref(),Some("newer-data"));
}
