use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};

use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{
    DownloadMode, GamesListParams, GroupsCreateParams, GroupsListParams, GroupsMoveParams,
    GroupsUpdateParams, ModsCleanupMissingParams, ModsListParams, ModsMoveParams, ModsRemoveParams,
    ModsRenameParams, ModsSetEnabledBatchParams, ModsSetVersionParams, ProfilesActivateParams,
    ProfilesCloneParams, ProfilesCreateParams, ProfilesDeleteParams, ProfilesListParams,
    ProfilesUpdateParams, VersionSelection, VersionSelectionKind,
};
use rusqlite::Connection;
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

use crate::{
    AppliedArtifact, CancelOperationResult, CredentialVault, DeleteProfileMod, LegacyCredential,
    LibraryAuthorization, LibraryError, LibraryMod, OwnerStore, ModContentVersion,
    ProfileModRecord, ProfileRecord, SetProfileModEnabled, StartDownloadOperation,
    StartOperationItem, CONTENT_WRITE_PERMISSION, GLOBAL_SETTING_SCOPE, METADATA_WRITE_PERMISSION,
    PROFILE_WRITE_PERMISSION, READ_PERMISSION,
};

#[derive(Default)]
struct RecordingVault {
    stored: RefCell<Vec<(String, String, String)>>,
    deleted: RefCell<Vec<String>>,
}

impl CredentialVault for RecordingVault {
    fn store(&self, credential: LegacyCredential<'_>) -> Result<String, String> {
        let mut stored = self.stored.borrow_mut();
        let reference = format!(
            "keychain://test/{}/{}",
            credential.provider,
            stored.len() + 1
        );
        stored.push((
            credential.provider.to_owned(),
            credential.secret.to_owned(),
            reference.clone(),
        ));
        Ok(reference)
    }

    fn delete(&self, credential_ref: &str) -> Result<(), String> {
        self.deleted.borrow_mut().push(credential_ref.to_owned());
        Ok(())
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/baseline/legacy/mintcat-v0.6.sqlite")
}

fn open_store(root: &Path) -> OwnerStore {
    let layout = RuntimePaths::new(root.join("data"))
        .unwrap()
        .initialize(RuntimeEnvironment::Test)
        .unwrap();
    OwnerStore::open(&layout).unwrap()
}

fn copy_fixture(root: &Path, name: &str) -> PathBuf {
    let destination = root.join(name);
    fs::copy(fixture(), &destination).unwrap();
    destination
}

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

#[test]
fn imports_real_legacy_copy_with_wal_order_identity_and_unverified_hints() {
    let sandbox = tempdir().unwrap();
    let legacy_path = copy_fixture(sandbox.path(), "legacy.sqlite");
    let writer = Connection::open(&legacy_path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             BEGIN IMMEDIATE;
             INSERT INTO settings(name, value) VALUES ('wal-visible', 'yes');
             INSERT INTO settings(name, value) VALUES ('game_1_shared-setting', 'drg-value');
             INSERT INTO settings(name, value) VALUES ('game_2_shared-setting', 'rc-value');
             COMMIT;",
        )
        .unwrap();
    let source_digest_before = digest(&legacy_path);

    let store = open_store(sandbox.path());
    let vault = RecordingVault::default();
    let report = store.import_legacy(&legacy_path, &vault).unwrap();
    assert!(!report.already_imported);
    assert_eq!(report.counts.games, 2);
    assert_eq!(report.counts.installations, 2);
    assert_eq!(report.counts.profiles, 2);
    assert_eq!(report.counts.groups, 3);
    assert_eq!(report.counts.mods, 3);
    assert_eq!(report.counts.versions, 4);
    assert_eq!(report.counts.profile_mods, 3);
    assert_eq!(report.counts.settings, 5);
    assert_eq!(report.counts.legacy_install_hints, 2);
    assert_eq!(report.imported_credentials, 0);
    assert!(vault.stored.borrow().is_empty());

    let snapshot = store.snapshot().unwrap();
    assert_eq!(
        snapshot
            .games
            .iter()
            .map(|game| (game.game_key.as_str(), game.active))
            .collect::<Vec<_>>(),
        vec![("drg", true), ("rogue-core", false)]
    );
    assert_eq!(
        snapshot
            .installations
            .iter()
            .map(|installation| installation.state.as_str())
            .collect::<Vec<_>>(),
        vec!["unresolved", "unresolved"]
    );
    assert_eq!(snapshot.games[1].game_id, "game:rogue-core");
    assert_eq!(snapshot.profiles.iter().find(|profile| profile.profile_id == "profile:legacy:2").unwrap().game_id, "game:rogue-core");
    assert_eq!(snapshot.mods.iter().find(|item| item.mod_id == "mod:legacy:3").unwrap().game_id, "game:rogue-core");
    assert_eq!(snapshot.installations.iter().find(|installation| installation.installation_id == "installation:legacy:2").unwrap().game_id, "game:rogue-core");

    let drg_memberships = snapshot
        .profile_mods
        .iter()
        .filter(|record| record.profile_id == "profile:legacy:1")
        .collect::<Vec<_>>();
    assert_eq!(drg_memberships.len(), 2);
    assert_eq!(drg_memberships[0].sort_order, 0);
    assert!(drg_memberships[0].enabled);
    assert_eq!(drg_memberships[0].used_version.as_deref(), Some("1.0.0"));
    assert_eq!(drg_memberships[1].sort_order, 1);
    assert!(!drg_memberships[1].enabled);
    assert_eq!(drg_memberships[1].used_version, None);
    assert_ne!(
        drg_memberships[0].incarnation_id,
        drg_memberships[1].incarnation_id
    );

    let nested = snapshot
        .groups
        .iter()
        .find(|group| group.group_id == "group:legacy:2")
        .unwrap();
    assert_eq!(nested.parent_group_id.as_deref(), Some("group:legacy:1"));
    let online = snapshot
        .mods
        .iter()
        .find(|record| record.mod_id == "mod:legacy:1")
        .unwrap();
    assert_eq!(online.source_id, "modio");
    assert_eq!(online.remote_game_key, "drg");
    assert_eq!(online.remote_mod_id, "1001");
    let local = snapshot
        .mods
        .iter()
        .find(|record| record.mod_id == "mod:legacy:2")
        .unwrap();
    assert_eq!(local.source_id, "local");
    assert_eq!(local.remote_mod_id, "legacy-local:2");

    let cached_versions = snapshot
        .versions
        .iter()
        .filter(|version| version.legacy_location.is_some())
        .collect::<Vec<_>>();
    assert_eq!(cached_versions.len(), 3);
    assert!(cached_versions
        .iter()
        .all(|version| version.artifact_digest.is_none()
            && version.availability == "legacy-unverified"));
    assert!(snapshot
        .legacy_install_hints
        .iter()
        .all(|hint| hint.claim_status == "unverified"));
    assert!(snapshot
        .legacy_mappings
        .iter()
        .any(|mapping| mapping.entity_kind == "profile-mod"
            && mapping.legacy_id == "2"
            && mapping.new_id == "profile-mod:legacy:2"));
    assert_eq!(
        store.get_global_setting("wal-visible").unwrap().as_deref(),
        Some("yes")
    );
    assert_eq!(
        store
            .get_game_setting("game:drg", "shared-setting")
            .unwrap()
            .as_deref(),
        Some("drg-value")
    );
    assert_eq!(
        store
            .get_game_setting("game:rogue-core", "shared-setting")
            .unwrap()
            .as_deref(),
        Some("rc-value")
    );
    assert!(!snapshot
        .settings
        .iter()
        .any(|setting| setting.name.ends_with("installHash")));
    assert_eq!(digest(&legacy_path), source_digest_before);

    store
        .set_game_setting("game:drg", "same-key", "drg")
        .unwrap();
    store.set_game_setting("game:rogue-core", "same-key", "rc").unwrap();
    assert_eq!(
        store
            .get_game_setting("game:drg", "same-key")
            .unwrap()
            .as_deref(),
        Some("drg")
    );
    assert_eq!(
        store
            .get_game_setting("game:rogue-core", "same-key")
            .unwrap()
            .as_deref(),
        Some("rc")
    );
    assert!(matches!(
        store.set_game_setting("game:missing", "key", "value"),
        Err(LibraryError::InvalidRecord(_))
    ));

    assert!(matches!(store.import_legacy(&legacy_path, &vault), Err(LibraryError::StaleOperation)));
    drop(writer);
}

#[test]
fn credentials_become_references_and_logical_backup_restores_exact_snapshot() {
    let sandbox = tempdir().unwrap();
    let legacy_path = copy_fixture(sandbox.path(), "legacy-with-token.sqlite");
    let legacy = Connection::open(&legacy_path).unwrap();
    legacy
        .execute(
            "UPDATE oauths SET oauth = 'super-secret-token' WHERE platform = 'mod.io'",
            [],
        )
        .unwrap();
    drop(legacy);

    let store = open_store(&sandbox.path().join("source"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(store.database_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.database_path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let vault = RecordingVault::default();
    let report = store.import_legacy(&legacy_path, &vault).unwrap();
    assert_eq!(report.imported_credentials, 1);
    let stored = vault.stored.borrow();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].0, "modio");
    assert_eq!(stored[0].1, "super-secret-token");
    drop(stored);

    let snapshot = store.snapshot().unwrap();
    let modio = snapshot
        .credentials
        .iter()
        .find(|record| record.provider == "modio")
        .unwrap();
    assert_eq!(
        modio.credential_ref.as_deref(),
        Some("keychain://test/modio/1")
    );
    assert!(snapshot
        .credentials
        .iter()
        .all(|record| record.credential_ref.as_deref() != Some("super-secret-token")));
    assert_eq!(
        snapshot
            .settings
            .iter()
            .filter(|setting| setting.scope_id == GLOBAL_SETTING_SCOPE)
            .count(),
        2
    );

    let database_path = store.database_path();
    for path in [
        database_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", database_path.display())),
    ] {
        if path.exists() {
            assert!(!fs::read(path)
                .unwrap()
                .windows(b"super-secret-token".len())
                .any(|window| window == b"super-secret-token"));
        }
    }

    let backup = serde_json::to_vec(&snapshot).unwrap();
    assert!(!backup
        .windows(b"super-secret-token".len())
        .any(|window| window == b"super-secret-token"));
    let restored_snapshot = serde_json::from_slice(&backup).unwrap();
    let restored_store = open_store(&sandbox.path().join("restored"));
    restored_store.import_empty_snapshot(&restored_snapshot).unwrap();
    assert_eq!(restored_store.snapshot().unwrap(), snapshot);
    assert!(matches!(
        restored_store.import_empty_snapshot(&restored_snapshot),
        Err(LibraryError::LibraryNotEmpty)
    ));
}

#[derive(Clone)]
struct GateFixture {
    store: OwnerStore,
    authorization: LibraryAuthorization,
    profile: ProfileRecord,
    profile_mod: ProfileModRecord,
    library_mod: LibraryMod,
    version: ModContentVersion,
}

impl GateFixture {
    fn request(
        &self,
        idempotency_key: &str,
        owner_generation: &str,
        expected_digest: Option<String>,
    ) -> StartDownloadOperation {
        StartDownloadOperation {
            idempotency_scope: "library.download".to_owned(),
            idempotency_key: idempotency_key.to_owned(),
            profile_id: self.profile.profile_id.clone(),
            expected_content_revision: self.profile.content_revision,
            target_incarnation_id: self.profile.incarnation_id.clone(),
            owner_generation: owner_generation.to_owned(),
            module_lock_digest: format!("sha256:{}", "a".repeat(64)),
            mode: DownloadMode::PrepareAndApply,
            items: vec![StartOperationItem {
                item_id: "primary".to_owned(),
                profile_mod_id: self.profile_mod.profile_mod_id.clone(),
                profile_mod_incarnation_id: self.profile_mod.incarnation_id.clone(),
                mod_id: self.library_mod.mod_id.clone(),
                source_id: self.library_mod.source_id.clone(),
                source_mod_id: self.library_mod.remote_mod_id.clone(),
                remote_game_key: self.library_mod.remote_game_key.clone(),
                source_file_id: self.version.remote_file_id.clone(),
                resolved_version: self.version.version_id.clone(),
                expected_digest,
                validator: Some("fixture-validator".to_owned()),
                selection: None,
            }],
        }
    }
}

fn gate_fixture(root: &Path) -> GateFixture {
    fs::create_dir_all(root).unwrap();
    let legacy_path = copy_fixture(root, "gate-legacy.sqlite");
    let store = open_store(root);
    store
        .import_legacy(&legacy_path, &RecordingVault::default())
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let profile = snapshot
        .profiles
        .iter()
        .find(|record| record.profile_id == "profile:legacy:1")
        .unwrap()
        .clone();
    let profile_mod = snapshot
        .profile_mods
        .iter()
        .find(|record| record.profile_id == profile.profile_id && record.enabled)
        .unwrap()
        .clone();
    let library_mod = snapshot
        .mods
        .iter()
        .find(|record| record.mod_id == profile_mod.mod_id)
        .unwrap()
        .clone();
    let version = snapshot
        .versions
        .iter()
        .find(|record| {
            record.mod_id == library_mod.mod_id
                && Some(record.version_id.as_str()) == profile_mod.used_version.as_deref()
        })
        .unwrap()
        .clone();
    let authorization = LibraryAuthorization::new(
        profile.owner_scope.clone(),
        [CONTENT_WRITE_PERMISSION.to_owned()],
    );
    GateFixture {
        store,
        authorization,
        profile,
        profile_mod,
        library_mod,
        version,
    }
}

fn artifact_store(root: &Path) -> ArtifactStore {
    let layout = RuntimePaths::new(root.join("data"))
        .unwrap()
        .initialize(RuntimeEnvironment::Test)
        .unwrap();
    ArtifactStore::from_layout(
        &layout,
        RuntimeEnvironment::Test,
        "test-target",
        "1",
        TrustStore::default(),
    )
    .unwrap()
}

fn publish_fixture_artifact(root: &Path, artifacts: &ArtifactStore) -> mintcat_sdk::ArtifactRef {
    let payload = root.join("downloaded.mod");
    fs::write(&payload, b"verified fixture artifact").unwrap();
    artifacts
        .publish_blob_file(&payload, Some("application/octet-stream".to_owned()))
        .unwrap()
}

#[test]
fn operation_gate_linearizes_start_edit_and_delete_and_deduplicates_two_clients() {
    let sandbox = tempdir().unwrap();
    let fixture = gate_fixture(&sandbox.path().join("edit"));
    let request = fixture.request("start-edit", "generation-1", None);
    let edit = SetProfileModEnabled {
        idempotency_key: "edit-during-start".to_owned(),
        profile_id: fixture.profile.profile_id.clone(),
        profile_mod_id: fixture.profile_mod.profile_mod_id.clone(),
        expected_content_revision: fixture.profile.content_revision,
        enabled: !fixture.profile_mod.enabled,
    };
    let barrier = Arc::new(Barrier::new(3));
    let (start_result, edit_result) = std::thread::scope(|scope| {
        let start_store = fixture.store.clone();
        let start_authorization = fixture.authorization.clone();
        let start_barrier = Arc::clone(&barrier);
        let start = scope.spawn(move || {
            start_barrier.wait();
            start_store.start_download_operation(&start_authorization, &request)
        });
        let edit_store = fixture.store.clone();
        let edit_authorization = fixture.authorization.clone();
        let edit_barrier = Arc::clone(&barrier);
        let edit = scope.spawn(move || {
            edit_barrier.wait();
            edit_store.set_profile_mod_enabled(&edit_authorization, &edit)
        });
        barrier.wait();
        (start.join().unwrap(), edit.join().unwrap())
    });
    assert_ne!(start_result.is_ok(), edit_result.is_ok());
    if let Ok(started) = start_result {
        assert!(matches!(
            edit_result,
            Err(LibraryError::ResourceBusy { .. })
        ));
        let replay = fixture
            .store
            .start_download_operation(
                &fixture.authorization,
                &fixture.request("start-edit", "generation-1", None),
            )
            .unwrap();
        assert!(!replay.created);
        assert_eq!(
            replay.operation.operation_id,
            started.operation.operation_id
        );
        let snapshot = fixture.store.snapshot().unwrap();
        assert_eq!(
            snapshot
                .profiles
                .iter()
                .find(|record| record.profile_id == fixture.profile.profile_id)
                .unwrap()
                .content_revision,
            fixture.profile.content_revision
        );
    } else {
        assert!(matches!(
            start_result,
            Err(LibraryError::RevisionConflict { .. })
        ));
        let edited = edit_result.unwrap();
        assert_eq!(
            edited.content_revision,
            fixture.profile.content_revision + 1
        );
    }

    let delete_fixture = gate_fixture(&sandbox.path().join("delete"));
    let start_request = delete_fixture.request("start-delete", "generation-1", None);
    let delete_request = DeleteProfileMod {
        idempotency_key: "delete-during-start".to_owned(),
        profile_id: delete_fixture.profile.profile_id.clone(),
        profile_mod_id: delete_fixture.profile_mod.profile_mod_id.clone(),
        expected_content_revision: delete_fixture.profile.content_revision,
    };
    let barrier = Arc::new(Barrier::new(3));
    let (start_result, delete_result) = std::thread::scope(|scope| {
        let start_store = delete_fixture.store.clone();
        let start_authorization = delete_fixture.authorization.clone();
        let start_barrier = Arc::clone(&barrier);
        let start = scope.spawn(move || {
            start_barrier.wait();
            start_store.start_download_operation(&start_authorization, &start_request)
        });
        let delete_store = delete_fixture.store.clone();
        let delete_authorization = delete_fixture.authorization.clone();
        let delete_barrier = Arc::clone(&barrier);
        let delete = scope.spawn(move || {
            delete_barrier.wait();
            delete_store.delete_profile_mod(&delete_authorization, &delete_request)
        });
        barrier.wait();
        (start.join().unwrap(), delete.join().unwrap())
    });
    assert_ne!(start_result.is_ok(), delete_result.is_ok());
    let membership_exists = delete_fixture
        .store
        .snapshot()
        .unwrap()
        .profile_mods
        .iter()
        .any(|record| record.profile_mod_id == delete_fixture.profile_mod.profile_mod_id);
    if start_result.is_ok() {
        assert!(matches!(
            delete_result,
            Err(LibraryError::ResourceBusy { .. })
        ));
        assert!(membership_exists);
    } else {
        assert!(matches!(
            start_result,
            Err(LibraryError::RevisionConflict { .. })
        ));
        assert!(delete_result.is_ok());
        assert!(!membership_exists);
    }

    let clients_fixture = gate_fixture(&sandbox.path().join("clients"));
    let first_request = clients_fixture.request("client-a", "generation-1", None);
    let second_request = clients_fixture.request("client-b", "generation-1", None);
    let barrier = Arc::new(Barrier::new(3));
    let (first, second) = std::thread::scope(|scope| {
        let first_store = clients_fixture.store.clone();
        let first_authorization = clients_fixture.authorization.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = scope.spawn(move || {
            first_barrier.wait();
            first_store.start_download_operation(&first_authorization, &first_request)
        });
        let second_store = clients_fixture.store.clone();
        let second_authorization = clients_fixture.authorization.clone();
        let second_barrier = Arc::clone(&barrier);
        let second = scope.spawn(move || {
            second_barrier.wait();
            second_store.start_download_operation(&second_authorization, &second_request)
        });
        barrier.wait();
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_ne!(first.is_ok(), second.is_ok());
    let winner = first
        .as_ref()
        .ok()
        .or_else(|| second.as_ref().ok())
        .unwrap();
    let loser = if first.is_err() { &first } else { &second };
    assert!(matches!(loser, Err(LibraryError::ResourceBusy { .. })));
    assert_eq!(
        clients_fixture
            .store
            .pending_outbox("kernel-job", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        clients_fixture
            .store
            .download_operation(
                &clients_fixture.authorization,
                &winner.operation.operation_id
            )
            .unwrap()
            .state,
        crate::DownloadOperationState::Queued
    );
    let winning_key = if first.is_ok() {
        "client-a"
    } else {
        "client-b"
    };
    let replay = clients_fixture
        .store
        .start_download_operation(
            &clients_fixture.authorization,
            &clients_fixture.request(winning_key, "generation-1", None),
        )
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.operation.operation_id, winner.operation.operation_id);
    assert!(matches!(
        clients_fixture.store.start_download_operation(
            &clients_fixture.authorization,
            &clients_fixture.request(
                winning_key,
                "generation-1",
                Some(format!("sha256:{}", "0".repeat(64))),
            ),
        ),
        Err(LibraryError::IdempotencyConflict)
    ));
    let denied = LibraryAuthorization::new(
        clients_fixture.profile.owner_scope.clone(),
        Vec::<String>::new(),
    );
    assert!(matches!(
        clients_fixture.store.set_profile_mod_enabled(
            &denied,
            &SetProfileModEnabled {
                idempotency_key: "denied-edit".to_owned(),
                profile_id: clients_fixture.profile.profile_id.clone(),
                profile_mod_id: clients_fixture.profile_mod.profile_mod_id.clone(),
                expected_content_revision: clients_fixture.profile.content_revision,
                enabled: !clients_fixture.profile_mod.enabled,
            },
        ),
        Err(LibraryError::PermissionDenied(_))
    ));
}

#[test]
fn cancel_and_apply_have_one_durable_winner_and_cancel_revokes_late_callbacks() {
    let sandbox = tempdir().unwrap();
    let fixture = gate_fixture(&sandbox.path().join("race"));
    let artifacts = artifact_store(&sandbox.path().join("race"));
    let artifact = publish_fixture_artifact(sandbox.path(), &artifacts);
    let grant = artifacts.grant_blob(&artifact).unwrap();
    let started = fixture
        .store
        .start_download_operation(
            &fixture.authorization,
            &fixture.request("cancel-race", "generation-1", Some(artifact.digest.clone())),
        )
        .unwrap();
    let operation_id = started.operation.operation_id.clone();
    let fence_epoch = started.operation.fence.fence_epoch;
    let barrier = Arc::new(Barrier::new(3));
    let (apply_result, cancel_result) = std::thread::scope(|scope| {
        let apply_store = fixture.store.clone();
        let apply_authorization = fixture.authorization.clone();
        let apply_operation_id = operation_id.clone();
        let apply_barrier = Arc::clone(&barrier);
        let apply = scope.spawn(move || {
            apply_barrier.wait();
            apply_store.apply_download_operation(
                &apply_authorization,
                &apply_operation_id,
                fence_epoch,
                "generation-1",
                &[AppliedArtifact {
                    item_id: "primary",
                    grant: &grant,
                }],
            )
        });
        let cancel_store = fixture.store.clone();
        let cancel_authorization = fixture.authorization.clone();
        let cancel_operation_id = operation_id.clone();
        let cancel_barrier = Arc::clone(&barrier);
        let cancel = scope.spawn(move || {
            cancel_barrier.wait();
            cancel_store.cancel_download_operation(&cancel_authorization, &cancel_operation_id)
        });
        barrier.wait();
        (apply.join().unwrap(), cancel.join().unwrap())
    });
    assert_ne!(
        apply_result.is_ok(),
        matches!(cancel_result, Ok(CancelOperationResult::Cancelled))
    );
    let operation = fixture
        .store
        .download_operation(&fixture.authorization, &operation_id)
        .unwrap();
    match operation.state {
        crate::DownloadOperationState::Succeeded => {
            assert!(apply_result.is_ok());
            assert!(matches!(
                cancel_result,
                Ok(CancelOperationResult::AlreadyCompleted(_))
            ));
            assert!(fixture
                .store
                .profile_content_binding(&fixture.profile_mod.profile_mod_id)
                .unwrap()
                .is_some());
        }
        crate::DownloadOperationState::Cancelled => {
            assert!(apply_result.is_err());
            assert!(matches!(
                cancel_result,
                Ok(CancelOperationResult::Cancelled)
            ));
            assert!(fixture
                .store
                .profile_content_binding(&fixture.profile_mod.profile_mod_id)
                .unwrap()
                .is_none());
        }
        state => panic!("unexpected terminal state {state:?}"),
    }

    let late_fixture = gate_fixture(&sandbox.path().join("late"));
    let late_artifacts = artifact_store(&sandbox.path().join("late"));
    let late_artifact = publish_fixture_artifact(&sandbox.path().join("late"), &late_artifacts);
    let late_grant = late_artifacts.grant_blob(&late_artifact).unwrap();
    let late_started = late_fixture
        .store
        .start_download_operation(
            &late_fixture.authorization,
            &late_fixture.request(
                "late-callback",
                "generation-1",
                Some(late_artifact.digest.clone()),
            ),
        )
        .unwrap();
    late_fixture
        .store
        .cancel_download_operation(
            &late_fixture.authorization,
            &late_started.operation.operation_id,
        )
        .unwrap();
    assert!(late_fixture
        .store
        .apply_download_operation(
            &late_fixture.authorization,
            &late_started.operation.operation_id,
            late_started.operation.fence.fence_epoch,
            "generation-1",
            &[AppliedArtifact {
                item_id: "primary",
                grant: &late_grant,
            }],
        )
        .is_err());
    assert!(late_fixture
        .store
        .profile_content_binding(&late_fixture.profile_mod.profile_mod_id)
        .unwrap()
        .is_none());
}

#[test]
fn owner_restart_requires_explicit_resume_and_rejects_the_old_fence() {
    let sandbox = tempdir().unwrap();
    let fixture = gate_fixture(sandbox.path());
    let artifacts = artifact_store(sandbox.path());
    let artifact = publish_fixture_artifact(sandbox.path(), &artifacts);
    let grant = artifacts.grant_blob(&artifact).unwrap();
    let started = fixture
        .store
        .start_download_operation(
            &fixture.authorization,
            &fixture.request("restart", "generation-1", Some(artifact.digest.clone())),
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .recover_download_operations("generation-2")
            .unwrap(),
        1
    );
    let capability = fixture
        .store
        .profile_content_capability(&fixture.authorization, &fixture.profile.profile_id)
        .unwrap();
    assert!(!capability.writable);
    assert!(capability.recovery_required);
    assert!(matches!(
        fixture.store.set_profile_mod_enabled(
            &fixture.authorization,
            &SetProfileModEnabled {
                idempotency_key: "blocked-edit".to_owned(),
                profile_id: fixture.profile.profile_id.clone(),
                profile_mod_id: fixture.profile_mod.profile_mod_id.clone(),
                expected_content_revision: fixture.profile.content_revision,
                enabled: !fixture.profile_mod.enabled,
            },
        ),
        Err(LibraryError::ResourceBusy { .. })
    ));
    let resumed = fixture
        .store
        .resume_download_operation(
            &fixture.authorization,
            &started.operation.operation_id,
            fixture.profile.content_revision,
            "generation-2",
        )
        .unwrap();
    assert!(resumed.fence.fence_epoch > started.operation.fence.fence_epoch);
    assert!(matches!(
        fixture.store.apply_download_operation(
            &fixture.authorization,
            &resumed.operation_id,
            started.operation.fence.fence_epoch,
            "generation-1",
            &[AppliedArtifact {
                item_id: "primary",
                grant: &grant,
            }],
        ),
        Err(LibraryError::StaleFence { .. })
    ));
    let applied = fixture
        .store
        .apply_download_operation(
            &fixture.authorization,
            &resumed.operation_id,
            resumed.fence.fence_epoch,
            "generation-2",
            &[AppliedArtifact {
                item_id: "primary",
                grant: &grant,
            }],
        )
        .unwrap();
    assert!(applied.applied);
    assert_eq!(
        fixture
            .store
            .profile_content_binding(&fixture.profile_mod.profile_mod_id)
            .unwrap()
            .unwrap()
            .0,
        fixture.version.version_id
    );
    let complete_snapshot = fixture.store.snapshot().unwrap();
    assert_eq!(complete_snapshot.download_operations.len(), 1);
    assert_eq!(complete_snapshot.content_refs.len(), 1);
    assert_eq!(complete_snapshot.profile_content_bindings.len(), 1);
    let restored = open_store(&sandbox.path().join("restored"));
    restored.import_empty_snapshot(&complete_snapshot).unwrap();
    assert_eq!(restored.snapshot().unwrap(), complete_snapshot);
}

fn full_authorization(owner_scope: &str) -> LibraryAuthorization {
    LibraryAuthorization::new(
        owner_scope.to_owned(),
        [
            READ_PERMISSION.to_owned(),
            PROFILE_WRITE_PERMISSION.to_owned(),
            CONTENT_WRITE_PERMISSION.to_owned(),
            METADATA_WRITE_PERMISSION.to_owned(),
        ],
    )
}

#[test]
fn library_queries_are_side_effect_free_bound_and_idempotent() {
    let sandbox = tempdir().unwrap();
    let fixture = gate_fixture(sandbox.path());
    let authorization = full_authorization(&fixture.profile.owner_scope);
    let before_reads = fixture.store.snapshot().unwrap();

    let first_games = fixture
        .store
        .list_games(
            &authorization,
            &GamesListParams {
                game_id: None,
                cursor: None,
                page_size: 1,
            },
        )
        .unwrap();
    assert_eq!(first_games.items.len(), 1);
    let second_games = fixture
        .store
        .list_games(
            &authorization,
            &GamesListParams {
                game_id: None,
                cursor: first_games.next_cursor.clone(),
                page_size: 1,
            },
        )
        .unwrap();
    assert_eq!(second_games.items.len(), 1);
    assert_ne!(first_games.items[0].game_id, second_games.items[0].game_id);
    assert!(fixture
        .store
        .list_games(
            &authorization,
            &GamesListParams {
                game_id: Some(first_games.items[0].game_id.clone()),
                cursor: first_games.next_cursor,
                page_size: 1,
            },
        )
        .is_err());
    fixture
        .store
        .list_profiles(
            &authorization,
            &ProfilesListParams {
                game_id: fixture.profile.game_id.clone(),
                cursor: None,
                page_size: 100,
            },
        )
        .unwrap();
    assert_eq!(fixture.store.snapshot().unwrap(), before_reads);

    let alpha_request = ProfilesCreateParams {
        game_id: fixture.profile.game_id.clone(),
        name: "Alpha".to_owned(),
        display_name: None,
        description: Some("first".to_owned()),
        active: Some(false),
    };
    let alpha = fixture
        .store
        .create_profile(&authorization, "create-alpha", &alpha_request)
        .unwrap();
    let alpha_replay = fixture
        .store
        .create_profile(&authorization, "create-alpha", &alpha_request)
        .unwrap();
    assert_eq!(alpha_replay, alpha);
    let mut conflicting_create = alpha_request.clone();
    conflicting_create.name = "Different".to_owned();
    assert!(matches!(
        fixture
            .store
            .create_profile(&authorization, "create-alpha", &conflicting_create,),
        Err(LibraryError::IdempotencyConflict)
    ));
    fixture
        .store
        .create_profile(
            &authorization,
            "create-beta",
            &ProfilesCreateParams {
                game_id: fixture.profile.game_id.clone(),
                name: "Beta".to_owned(),
                display_name: None,
                description: None,
                active: Some(false),
            },
        )
        .unwrap();
    let first_profiles = fixture
        .store
        .list_profiles(
            &authorization,
            &ProfilesListParams {
                game_id: fixture.profile.game_id.clone(),
                cursor: None,
                page_size: 1,
            },
        )
        .unwrap();
    let stale_cursor = first_profiles.next_cursor.unwrap();
    fixture
        .store
        .update_profile(
            &authorization,
            "update-alpha",
            &ProfilesUpdateParams {
                profile_id: alpha.profile_id.clone(),
                expected_revision: alpha.revision,
                name: Some("Alpha renamed".to_owned()),
                display_name: None,
                description: None,
            },
        )
        .unwrap();
    assert!(fixture
        .store
        .list_profiles(
            &authorization,
            &ProfilesListParams {
                game_id: fixture.profile.game_id.clone(),
                cursor: Some(stale_cursor),
                page_size: 1,
            },
        )
        .is_err());
    let profiles = fixture
        .store
        .list_profiles(
            &authorization,
            &ProfilesListParams {
                game_id: fixture.profile.game_id.clone(),
                cursor: None,
                page_size: 100,
            },
        )
        .unwrap();
    assert_eq!(
        profiles
            .items
            .iter()
            .filter(|profile| profile.active)
            .count(),
        1
    );
    assert!(
        profiles
            .items
            .iter()
            .find(|profile| profile.profile_id == fixture.profile.profile_id)
            .unwrap()
            .active
    );
}

#[test]
fn profile_group_and_mod_commands_preserve_explicit_targets_and_shared_metadata() {
    let sandbox = tempdir().unwrap();
    let fixture = gate_fixture(sandbox.path());
    let authorization = full_authorization(&fixture.profile.owner_scope);
    let initial_groups = fixture
        .store
        .list_groups(
            &authorization,
            &GroupsListParams {
                profile_id: fixture.profile.profile_id.clone(),
            },
        )
        .unwrap();
    let original_group = initial_groups.items[0].clone();
    let created_group = fixture
        .store
        .create_group(
            &authorization,
            "group-create",
            &GroupsCreateParams {
                profile_id: fixture.profile.profile_id.clone(),
                expected_content_revision: initial_groups.content_revision,
                parent_group_id: None,
                name: "Pinned".to_owned(),
                group_type: "custom".to_owned(),
                position: 0,
                expanded: Some(true),
            },
        )
        .unwrap();
    let after_create = fixture
        .store
        .list_groups(
            &authorization,
            &GroupsListParams {
                profile_id: fixture.profile.profile_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        after_create
            .items
            .iter()
            .filter(|group| group.parent_group_id.is_none())
            .map(|group| group.sort_order)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    fixture
        .store
        .update_group(
            &authorization,
            "group-update",
            &GroupsUpdateParams {
                profile_id: fixture.profile.profile_id.clone(),
                group_id: created_group.group_id.clone(),
                expected_content_revision: after_create.content_revision,
                name: Some("Pinned versions".to_owned()),
                expanded: Some(false),
            },
        )
        .unwrap();
    let after_update = fixture
        .store
        .list_groups(
            &authorization,
            &GroupsListParams {
                profile_id: fixture.profile.profile_id.clone(),
            },
        )
        .unwrap();
    fixture
        .store
        .move_group(
            &authorization,
            "group-move",
            &GroupsMoveParams {
                profile_id: fixture.profile.profile_id.clone(),
                group_id: created_group.group_id.clone(),
                expected_content_revision: after_update.content_revision,
                parent_group_id: Some(original_group.group_id.clone()),
                position: 0,
            },
        )
        .unwrap();
    let after_group_move = fixture
        .store
        .list_groups(
            &authorization,
            &GroupsListParams {
                profile_id: fixture.profile.profile_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        after_group_move
            .items
            .iter()
            .find(|group| group.group_id == created_group.group_id)
            .unwrap()
            .parent_group_id
            .as_deref(),
        Some(original_group.group_id.as_str())
    );

    let cloned = fixture
        .store
        .clone_profile(
            &authorization,
            "clone-profile",
            &ProfilesCloneParams {
                source_profile_id: fixture.profile.profile_id.clone(),
                expected_content_revision: after_group_move.content_revision,
                name: "Clone".to_owned(),
                display_name: None,
                description: None,
                active: Some(false),
            },
        )
        .unwrap();
    let cloned_groups = fixture
        .store
        .list_groups(
            &authorization,
            &GroupsListParams {
                profile_id: cloned.profile_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(cloned_groups.items.len(), after_group_move.items.len());

    let other_profile = fixture
        .store
        .list_profiles(
            &authorization,
            &ProfilesListParams {
                game_id: "game:rogue-core".to_owned(),
                cursor: None,
                page_size: 10,
            },
        )
        .unwrap()
        .items
        .remove(0);
    fixture
        .store
        .activate_profile(
            &authorization,
            "activate-other-game",
            &ProfilesActivateParams {
                game_id: other_profile.game_id.clone(),
                profile_id: other_profile.profile_id,
                expected_revision: other_profile.revision,
            },
        )
        .unwrap();

    let original_mods = fixture
        .store
        .list_mods(
            &authorization,
            &ModsListParams {
                profile_id: fixture.profile.profile_id.clone(),
                cursor: None,
                page_size: 100,
                query: None,
                source_id: None,
                enabled: None,
                group_id: None,
            },
        )
        .unwrap();
    let profile_mod_ids = original_mods
        .items
        .iter()
        .map(|item| item.profile_mod_id.clone())
        .collect::<Vec<_>>();
    let disabled = fixture
        .store
        .set_mods_enabled_batch(
            &authorization,
            "disable-original-profile",
            &ModsSetEnabledBatchParams {
                profile_id: fixture.profile.profile_id.clone(),
                profile_mod_ids,
                expected_content_revision: after_group_move.content_revision,
                enabled: false,
            },
        )
        .unwrap();
    assert!(fixture
        .store
        .list_mods(
            &authorization,
            &ModsListParams {
                profile_id: fixture.profile.profile_id.clone(),
                cursor: None,
                page_size: 100,
                query: None,
                source_id: None,
                enabled: Some(true),
                group_id: None,
            },
        )
        .unwrap()
        .items
        .is_empty());

    let selected = &original_mods.items[0];
    let renamed = fixture
        .store
        .rename_mod(
            &authorization,
            "rename-shared-mod",
            &ModsRenameParams {
                mod_id: selected.mod_id.clone(),
                display_name: "Shared display name".to_owned(),
                expected_entity_revision: selected.entity_revision,
                scope: "shared".to_owned(),
            },
        )
        .unwrap();
    assert!(renamed.changed);
    let cloned_mods = fixture
        .store
        .list_mods(
            &authorization,
            &ModsListParams {
                profile_id: cloned.profile_id.clone(),
                cursor: None,
                page_size: 100,
                query: Some("Shared display".to_owned()),
                source_id: None,
                enabled: None,
                group_id: None,
            },
        )
        .unwrap();
    assert_eq!(cloned_mods.items.len(), 1);

    let moved = fixture
        .store
        .move_mod(
            &authorization,
            "move-mod",
            &ModsMoveParams {
                profile_id: fixture.profile.profile_id.clone(),
                profile_mod_id: selected.profile_mod_id.clone(),
                group_id: Some(created_group.group_id.clone()),
                position: 0,
                expected_content_revision: disabled.content_revision,
            },
        )
        .unwrap();
    let chosen_version = fixture
        .store
        .snapshot()
        .unwrap()
        .versions
        .into_iter()
        .find(|version| version.mod_id == selected.mod_id)
        .unwrap();
    let version_download = fixture
        .store
        .start_version_download(
            &authorization,
            "set-version-download",
            "generation-version",
            &format!("sha256:{}", "a".repeat(64)),
            &ModsSetVersionParams {
                profile_id: fixture.profile.profile_id.clone(),
                profile_mod_id: selected.profile_mod_id.clone(),
                incarnation_id: selected.incarnation_id.clone(),
                expected_content_revision: moved.content_revision,
                selection: VersionSelection {
                    kind: VersionSelectionKind::Pinned,
                    version_id: Some(chosen_version.version_id),
                },
                mode: DownloadMode::PrepareAndApply,
            },
        )
        .unwrap();
    assert!(version_download.created);
    assert_eq!(
        fixture
            .store
            .pending_outbox("kernel-job", 10)
            .unwrap()
            .len(),
        1
    );
    fixture
        .store
        .cancel_download_operation(&authorization, &version_download.operation.operation_id)
        .unwrap();
    {
        let connection = fixture.store.connection().unwrap();
        connection
            .execute(
                "UPDATE library_mods SET local_missing = 1 WHERE mod_id = ?1",
                [&selected.mod_id],
            )
            .unwrap();
    }
    assert!(matches!(
        fixture.store.cleanup_missing_mods(
            &authorization,
            "cleanup-wrong-game",
            &ModsCleanupMissingParams {
                game_id: "game:other".to_owned(),
                profile_id: fixture.profile.profile_id.clone(),
                expected_content_revision: moved.content_revision,
            },
        ),
        Err(LibraryError::InvalidRecord(_))
    ));
    let cleaned = fixture
        .store
        .cleanup_missing_mods(
            &authorization,
            "cleanup-missing",
            &ModsCleanupMissingParams {
                game_id: fixture.profile.game_id.clone(),
                profile_id: fixture.profile.profile_id.clone(),
                expected_content_revision: moved.content_revision,
            },
        )
        .unwrap();
    assert!(cleaned.changed);
    let after_cleanup = fixture.store.snapshot().unwrap();
    assert!(after_cleanup
        .mods
        .iter()
        .any(|record| record.mod_id == selected.mod_id));
    assert!(after_cleanup
        .profile_mods
        .iter()
        .any(|record| record.profile_id == cloned.profile_id && record.mod_id == selected.mod_id));
    let cloned_association = cloned_mods.items.first().unwrap();
    fixture
        .store
        .remove_mod(
            &authorization,
            "remove-cloned-association",
            &ModsRemoveParams {
                profile_id: cloned.profile_id.clone(),
                profile_mod_id: cloned_association.profile_mod_id.clone(),
                expected_content_revision: cloned_mods.content_revision,
            },
        )
        .unwrap();
    assert!(fixture
        .store
        .snapshot()
        .unwrap()
        .mods
        .iter()
        .any(|record| record.mod_id == selected.mod_id));

    let transient = fixture
        .store
        .create_profile(
            &authorization,
            "create-active-transient",
            &ProfilesCreateParams {
                game_id: fixture.profile.game_id.clone(),
                name: "Transient".to_owned(),
                display_name: None,
                description: None,
                active: Some(true),
            },
        )
        .unwrap();
    fixture
        .store
        .delete_profile_command(
            &authorization,
            "delete-active-transient",
            &ProfilesDeleteParams {
                profile_id: transient.profile_id,
                expected_revision: transient.revision,
            },
        )
        .unwrap();
    assert!(fixture
        .store
        .list_profiles(
            &authorization,
            &ProfilesListParams {
                game_id: fixture.profile.game_id.clone(),
                cursor: None,
                page_size: 100,
            },
        )
        .unwrap()
        .items
        .iter()
        .all(|profile| !profile.active));
}

#[test]
fn every_profile_content_command_obeys_the_backend_gate_while_metadata_remains_editable() {
    let sandbox = tempdir().unwrap();
    let fixture = gate_fixture(sandbox.path());
    let authorization = full_authorization(&fixture.profile.owner_scope);
    let started = fixture
        .store
        .start_download_operation(
            &authorization,
            &fixture.request("gate-all-content", "generation-1", None),
        )
        .unwrap();
    let profile_mods = fixture
        .store
        .snapshot()
        .unwrap()
        .profile_mods
        .into_iter()
        .filter(|record| record.profile_id == fixture.profile.profile_id)
        .collect::<Vec<_>>();
    let first = &profile_mods[0];
    let version = fixture
        .store
        .snapshot()
        .unwrap()
        .versions
        .into_iter()
        .find(|record| record.mod_id == first.mod_id)
        .unwrap();
    let blocked = [
        fixture
            .store
            .create_group(
                &authorization,
                "blocked-group",
                &GroupsCreateParams {
                    profile_id: fixture.profile.profile_id.clone(),
                    expected_content_revision: fixture.profile.content_revision,
                    parent_group_id: None,
                    name: "Blocked".to_owned(),
                    group_type: "custom".to_owned(),
                    position: 0,
                    expanded: None,
                },
            )
            .map(|_| ()),
        fixture
            .store
            .set_mods_enabled_batch(
                &authorization,
                "blocked-enable",
                &ModsSetEnabledBatchParams {
                    profile_id: fixture.profile.profile_id.clone(),
                    profile_mod_ids: vec![first.profile_mod_id.clone()],
                    expected_content_revision: fixture.profile.content_revision,
                    enabled: !first.enabled,
                },
            )
            .map(|_| ()),
        fixture
            .store
            .move_mod(
                &authorization,
                "blocked-move",
                &ModsMoveParams {
                    profile_id: fixture.profile.profile_id.clone(),
                    profile_mod_id: first.profile_mod_id.clone(),
                    group_id: first.group_id.clone(),
                    position: 0,
                    expected_content_revision: fixture.profile.content_revision,
                },
            )
            .map(|_| ()),
        fixture
            .store
            .remove_mod(
                &authorization,
                "blocked-remove",
                &ModsRemoveParams {
                    profile_id: fixture.profile.profile_id.clone(),
                    profile_mod_id: first.profile_mod_id.clone(),
                    expected_content_revision: fixture.profile.content_revision,
                },
            )
            .map(|_| ()),
        fixture
            .store
            .start_version_download(
                &authorization,
                "blocked-version",
                "generation-blocked",
                &format!("sha256:{}", "a".repeat(64)),
                &ModsSetVersionParams {
                    profile_id: fixture.profile.profile_id.clone(),
                    profile_mod_id: first.profile_mod_id.clone(),
                    incarnation_id: first.incarnation_id.clone(),
                    expected_content_revision: fixture.profile.content_revision,
                    selection: VersionSelection {
                        kind: VersionSelectionKind::Pinned,
                        version_id: Some(version.version_id),
                    },
                    mode: DownloadMode::PrepareAndApply,
                },
            )
            .map(|_| ()),
        fixture
            .store
            .delete_profile_command(
                &authorization,
                "blocked-profile-delete",
                &ProfilesDeleteParams {
                    profile_id: fixture.profile.profile_id.clone(),
                    expected_revision: fixture.profile.revision,
                },
            )
            .map(|_| ()),
    ];
    assert!(blocked.iter().all(|result| {
        matches!(
            result,
            Err(LibraryError::ResourceBusy {
                owner_operation_id
            }) if owner_operation_id == &started.operation.operation_id
        )
    }));

    let updated = fixture
        .store
        .update_profile(
            &authorization,
            "allowed-profile-name",
            &ProfilesUpdateParams {
                profile_id: fixture.profile.profile_id.clone(),
                expected_revision: fixture.profile.revision,
                name: Some("Renamed while downloading".to_owned()),
                display_name: None,
                description: None,
            },
        )
        .unwrap();
    assert_eq!(updated.name, "Renamed while downloading");
    let library_mod = fixture
        .store
        .snapshot()
        .unwrap()
        .mods
        .into_iter()
        .find(|record| record.mod_id == first.mod_id)
        .unwrap();
    assert!(
        fixture
            .store
            .rename_mod(
                &authorization,
                "allowed-mod-name",
                &ModsRenameParams {
                    mod_id: library_mod.mod_id,
                    display_name: "Renamed metadata".to_owned(),
                    expected_entity_revision: library_mod.revision,
                    scope: "shared".to_owned(),
                },
            )
            .unwrap()
            .changed
    );
}
