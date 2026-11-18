use std::{fs, path::Path};
use mintcat_library_service::{LibraryAuthorization, LibraryError, LibrarySnapshot, LibraryStore, READ_PERMISSION, METADATA_WRITE_PERMISSION};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimeLayout, RuntimePaths};
use mintcat_sdk::{SettingsGetParams, SettingsScope, SettingsUpdateParams};
use sha2::{Digest, Sha256};
struct Fixture { root: tempfile::TempDir, layout: RuntimeLayout }
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let layout = RuntimePaths::new(root.path().join("data")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        Self { root, layout }
    }
}
#[test]
fn builtin_owner_shares_one_writer_preserves_data_and_enforces_revisions() {
    let fixture = Fixture::new();
    let library = LibraryStore::open(&fixture.layout).unwrap();
    assert!(matches!(LibraryStore::open(&fixture.layout), Err(LibraryError::ResourceBusy { .. })));
    let retained = library.clone();
    let authorization = LibraryAuthorization::new("human", [READ_PERMISSION, METADATA_WRITE_PERMISSION]);
    let request = SettingsGetParams { scope: SettingsScope::Value(serde_json::json!({"kind":"global"})), section: "appearance".into() };
    let update = SettingsUpdateParams { scope: request.scope.clone(), section: request.section.clone(), expected_revision: 0, values: serde_json::json!({"theme":"dark"}) };
    let result = library.update_settings(&authorization, "settings-first", &update, None).unwrap();
    assert!(matches!(library.update_settings(&authorization, "settings-stale", &update, None), Err(LibraryError::RevisionConflict { .. })));
    assert!(matches!(library.get_settings(&LibraryAuthorization::new("human", Vec::<String>::new()), &request), Err(LibraryError::PermissionDenied(_))));
    library.set_global_setting("large", &"x".repeat(2 * 1024 * 1024)).unwrap();
    let snapshot = library.backup_snapshot().unwrap();
    let epoch = library.write_epoch().unwrap();
    drop(library);
    assert_eq!(retained.get_settings(&authorization, &request).unwrap(), result);
    assert!(LibraryStore::open(&fixture.layout).is_err());
    drop(retained);
    let reopened = LibraryStore::open(&fixture.layout).unwrap();
    assert_eq!(reopened.backup_snapshot().unwrap(), snapshot);
    assert_eq!(reopened.write_epoch().unwrap(), epoch);
}
#[test]
fn unsupported_schema_is_rejected_without_mutating_database() {
    let fixture = Fixture::new();
    let library = LibraryStore::open(&fixture.layout).unwrap();
    library.set_global_setting("retained", "value").unwrap();
    let path = library.database_path().to_owned();
    drop(library);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("INSERT INTO library_schema VALUES(6,0)", []).unwrap();
    drop(connection);
    let bytes = fs::read(&path).unwrap();
    assert!(matches!(LibraryStore::open(&fixture.layout), Err(LibraryError::UnsupportedSchema(6))));
    assert_eq!(fs::read(path).unwrap(), bytes);
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
    let fixture=Fixture::new();let library=LibraryStore::open(&fixture.layout).unwrap();
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
    drop(library);
    let restarted=LibraryStore::open(&fixture.layout).unwrap();assert_eq!(restarted.legacy_import_outcome(&source_digest).unwrap().unwrap(),outcome);
    assert!(restarted.import_legacy(&source,&vault).unwrap().already_imported);
    restarted.set_global_setting("after-import","newer-data").unwrap();
    assert!(matches!(restarted.legacy_import_outcome(&source_digest),Err(LibraryError::StaleOperation)));
    assert!(matches!(restarted.import_legacy(&source,&vault),Err(LibraryError::StaleOperation)));
    assert_eq!(restarted.get_global_setting("after-import").unwrap().as_deref(),Some("newer-data"));
}
