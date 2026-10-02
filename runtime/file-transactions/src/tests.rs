use std::{fs, io::Cursor, path::Path};

use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use tempfile::{tempdir, TempDir};

use super::*;

fn stopped(_: &Path) -> Result<(), String> {
    Ok(())
}

fn running(_: &Path) -> Result<(), String> {
    Err("game.exe (pid 42)".to_owned())
}

fn content_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_bytes(bytes))
}

struct Fixture {
    _sandbox: TempDir,
    database: KernelDatabase,
    artifacts: ArtifactStore,
    manager: FileTransactionManager,
    root: PathBuf,
    manager_state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("runtime"))
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();
        let database = KernelDatabase::open(&layout).unwrap();
        let artifacts = ArtifactStore::from_layout(
            &layout,
            RuntimeEnvironment::Test,
            "test-target",
            "1",
            TrustStore::default(),
        )
        .unwrap();
        let root = sandbox.path().join("game");
        fs::create_dir(&root).unwrap();
        let manager_state = sandbox.path().join("file-transactions");
        let manager = FileTransactionManager::open(database.clone(), &manager_state).unwrap();
        Self {
            _sandbox: sandbox,
            database,
            artifacts,
            manager,
            root,
            manager_state,
        }
    }

    fn grant(&self, bytes: &[u8]) -> ArtifactGrant {
        let artifact = self
            .artifacts
            .publish_blob_reader(&mut Cursor::new(bytes), None)
            .unwrap();
        self.artifacts.grant_blob(&artifact).unwrap()
    }

    fn reopen_manager(&self) -> FileTransactionManager {
        FileTransactionManager::open(self.database.clone(), &self.manager_state).unwrap()
    }

    fn request<'a>(&self, files: Vec<DeployFile<'a>>) -> DeployRequest<'a> {
        DeployRequest {
            installation_id: "install-1".to_owned(),
            owner_scope: "profile:profile-1".to_owned(),
            root: self.root.clone(),
            game_fingerprint: "game-fingerprint".to_owned(),
            profile_revision: 7,
            plan_hash: "sha256:plan".to_owned(),
            module_lock_digest: "sha256:module-lock".to_owned(),
            files,
        }
    }

    fn prepare_replacement_and_new(&self) -> PreparedTransaction {
        fs::create_dir_all(self.root.join("bin")).unwrap();
        fs::write(self.root.join("bin/game.dat"), b"old-game").unwrap();
        let replacement = self.grant(b"new-game");
        let addition = self.grant(b"new-config");
        self.manager
            .prepare(
                self.request(vec![
                    DeployFile {
                        relative_path: "bin/game.dat".to_owned(),
                        artifact: &replacement,
                        expected_target: ExpectedTarget::Digest(content_digest(b"old-game")),
                        owner: "module:loader".to_owned(),
                        install_mode: None,
                    },
                    DeployFile {
                        relative_path: "config/mintcat.ini".to_owned(),
                        artifact: &addition,
                        expected_target: ExpectedTarget::Absent,
                        owner: "profile:profile-1".to_owned(),
                        install_mode: None,
                    },
                ]),
                &stopped,
            )
            .unwrap()
    }

    fn install_files(&self, files: &[(&str, &[u8])]) -> InstallReceipt {
        let grants: Vec<_> = files.iter().map(|(_, bytes)| self.grant(bytes)).collect();
        let deployment = files.iter().zip(&grants).map(|((path, _), grant)| {
            let existing = self.root.join(path);
            DeployFile {
                relative_path: (*path).to_owned(),
                artifact: grant,
                expected_target: match fs::read(&existing) {
                    Ok(bytes) => ExpectedTarget::Digest(content_digest(&bytes)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => ExpectedTarget::Absent,
                    Err(error) => panic!("{error}"),
                },
                owner: "module:loader".to_owned(),
                install_mode: None,
            }
        }).collect();
        self.manager.commit(
            self.manager.prepare(self.request(deployment), &stopped).unwrap(),
            &FileCancellation::default(),
        ).unwrap().receipt.unwrap()
    }
}

#[test]
fn commit_persists_complete_receipt_and_uninstall_preserves_modified_and_foreign_files() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("foreign.txt"), b"foreign").unwrap();
    let prepared = fixture.prepare_replacement_and_new();

    assert_eq!(
        fs::read(fixture.root.join("bin/game.dat")).unwrap(),
        b"old-game"
    );
    assert!(!fixture.root.join("config/mintcat.ini").exists());
    assert!(fixture
        .root
        .join(TARGET_TRANSACTION_DIR)
        .join(prepared.transaction_id())
        .join("staged")
        .is_dir());

    let outcome = fixture
        .manager
        .commit(prepared, &FileCancellation::default())
        .unwrap();
    assert_eq!(outcome.disposition, CommitDisposition::Committed);
    assert!(!outcome.cancel_requested_during_commit);
    let receipt = outcome.receipt.unwrap();

    assert_eq!(receipt.installation_id, "install-1");
    assert_eq!(receipt.owner_scope, "profile:profile-1");
    assert_eq!(receipt.game_fingerprint, "game-fingerprint");
    assert_eq!(receipt.profile_revision, 7);
    assert_eq!(receipt.plan_hash, "sha256:plan");
    assert_eq!(receipt.module_lock_digest, "sha256:module-lock");
    assert_eq!(receipt.files.len(), 2);
    assert_eq!(
        fs::read(fixture.root.join("bin/game.dat")).unwrap(),
        b"new-game"
    );
    assert_eq!(
        fs::read(fixture.root.join("config/mintcat.ini")).unwrap(),
        b"new-config"
    );
    assert!(fixture
        .root
        .join(TARGET_TRANSACTION_DIR)
        .join(&receipt.transaction_id)
        .join("COMMITTED")
        .is_file());
    assert_eq!(
        fixture.manager.receipt(&receipt.receipt_id).unwrap(),
        receipt
    );

    fs::write(fixture.root.join("config/mintcat.ini"), b"user-edit").unwrap();
    let report = fixture
        .manager
        .uninstall_chain(&[receipt.receipt_id.clone()], &stopped, &FileCancellation::default())
        .unwrap();

    assert_eq!(
        report.disposition,
        UninstallDisposition::CompletedWithPreservedFiles
    );
    assert_eq!(report.restored, ["bin/game.dat"]);
    assert!(report.removed.is_empty());
    assert_eq!(report.preserved_modified, ["config/mintcat.ini"]);
    assert!(report.missing.is_empty());
    assert!(report.foreign_files_untouched);
    assert_eq!(
        fs::read(fixture.root.join("bin/game.dat")).unwrap(),
        b"old-game"
    );
    assert_eq!(
        fs::read(fixture.root.join("config/mintcat.ini")).unwrap(),
        b"user-edit"
    );
    assert_eq!(
        fs::read(fixture.root.join("foreign.txt")).unwrap(),
        b"foreign"
    );
    assert!(matches!(
        fixture.manager.receipt(&receipt.receipt_id),
        Err(FileTransactionError::ReceiptInactive(_))
    ));
}

#[test]
fn cancellation_is_truthful_before_and_during_commit() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("target.bin"), b"old").unwrap();
    let first = fixture.grant(b"new");
    let prepared = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "target.bin".to_owned(),
                artifact: &first,
                expected_target: ExpectedTarget::Digest(content_digest(b"old")),
                owner: "module:first".to_owned(),
                install_mode: None,
            }]),
            &stopped,
        )
        .unwrap();
    let cancellation = FileCancellation::default();
    cancellation.request();
    let outcome = fixture.manager.commit(prepared, &cancellation).unwrap();
    assert_eq!(outcome.disposition, CommitDisposition::Cancelled);
    assert!(outcome.receipt.is_none());
    assert_eq!(fs::read(fixture.root.join("target.bin")).unwrap(), b"old");

    let second = fixture.grant(b"new");
    let prepared = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "target.bin".to_owned(),
                artifact: &second,
                expected_target: ExpectedTarget::Digest(content_digest(b"old")),
                owner: "module:second".to_owned(),
                install_mode: None,
            }]),
            &stopped,
        )
        .unwrap();
    let cancellation = FileCancellation::default();
    let outcome = fixture
        .manager
        .commit_with_cancel_after_mutation(prepared, &cancellation, 1)
        .unwrap();
    assert_eq!(outcome.disposition, CommitDisposition::Committed);
    assert!(outcome.cancel_requested_during_commit);
    assert!(outcome.receipt.is_some());
    assert_eq!(fs::read(fixture.root.join("target.bin")).unwrap(), b"new");
}

#[test]
fn preparation_rejects_running_processes_bad_paths_mismatches_and_altered_grants() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("target.bin"), b"old").unwrap();
    let grant = fixture.grant(b"new");
    let error = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "target.bin".to_owned(),
                artifact: &grant,
                expected_target: ExpectedTarget::Digest(content_digest(b"old")),
                owner: "module:test".to_owned(),
                install_mode: None,
            }]),
            &running,
        )
        .unwrap_err();
    assert!(matches!(error, FileTransactionError::TargetRunning(_)));

    let error = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "../escape.bin".to_owned(),
                artifact: &grant,
                expected_target: ExpectedTarget::Absent,
                owner: "module:test".to_owned(),
                install_mode: None,
            }]),
            &stopped,
        )
        .unwrap_err();
    assert!(matches!(error, FileTransactionError::InvalidPath(_)));

    let error = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "target.bin".to_owned(),
                artifact: &grant,
                expected_target: ExpectedTarget::Digest(content_digest(b"not-old")),
                owner: "module:test".to_owned(),
                install_mode: None,
            }]),
            &stopped,
        )
        .unwrap_err();
    assert!(matches!(error, FileTransactionError::TargetMismatch { .. }));

    let mut altered = fixture.grant(b"newer");
    altered.descriptor.digest = sha256_bytes(b"forged");
    let error = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "other.bin".to_owned(),
                artifact: &altered,
                expected_target: ExpectedTarget::Absent,
                owner: "module:test".to_owned(),
                install_mode: None,
            }]),
            &stopped,
        )
        .unwrap_err();
    assert!(matches!(error, FileTransactionError::Artifact(_)));

    let transaction_parent = fixture.root.join(TARGET_TRANSACTION_DIR);
    if transaction_parent.exists() {
        assert_eq!(fs::read_dir(transaction_parent).unwrap().count(), 0);
    }
}

#[cfg(unix)]
#[test]
fn physical_aliases_share_identity_and_lock() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let alias = fixture._sandbox.path().join("game-alias");
    symlink(&fixture.root, &alias).unwrap();
    assert_eq!(
        physical_root_identity(&fixture.root).unwrap(),
        physical_root_identity(&alias).unwrap()
    );

    let first = fixture.grant(b"first");
    let prepared = fixture
        .manager
        .prepare(
            fixture.request(vec![DeployFile {
                relative_path: "first.bin".to_owned(),
                artifact: &first,
                expected_target: ExpectedTarget::Absent,
                owner: "module:first".to_owned(),
                install_mode: None,
            }]),
            &stopped,
        )
        .unwrap();

    let second = fixture.grant(b"second");
    let error = fixture
        .manager
        .prepare(
            DeployRequest {
                root: alias,
                files: vec![DeployFile {
                    relative_path: "second.bin".to_owned(),
                    artifact: &second,
                    expected_target: ExpectedTarget::Absent,
                    owner: "module:second".to_owned(),
                    install_mode: None,
                }],
                ..fixture.request(Vec::new())
            },
            &stopped,
        )
        .unwrap_err();
    assert!(matches!(error, FileTransactionError::RootBusy(_)));
    drop(prepared);
}

#[test]
fn every_filesystem_mutation_before_commit_marker_recovers_to_old_state() {
    for crash_after in 1..=3 {
        let fixture = Fixture::new();
        let prepared = fixture.prepare_replacement_and_new();
        let error = fixture
            .manager
            .commit_with_crash(prepared, crash_after)
            .unwrap_err();
        assert!(matches!(error, FileTransactionError::SimulatedCrash));

        let reports = fixture
            .reopen_manager()
            .recover_incomplete(&stopped)
            .unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].disposition, RecoveryDisposition::RolledBack);
        assert_eq!(
            fs::read(fixture.root.join("bin/game.dat")).unwrap(),
            b"old-game",
            "crash point {crash_after}"
        );
        assert!(
            !fixture.root.join("config/mintcat.ini").exists(),
            "crash point {crash_after}"
        );
    }
}

#[test]
fn committed_marker_recovers_receipt_after_database_commit_is_interrupted() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare_replacement_and_new();
    let transaction_id = prepared.transaction_id().to_owned();
    let error = fixture.manager.commit_with_crash(prepared, 5).unwrap_err();
    assert!(matches!(error, FileTransactionError::SimulatedCrash));

    let manager = fixture.reopen_manager();
    let reports = manager.recover_incomplete(&stopped).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].transaction_id, transaction_id);
    assert_eq!(
        reports[0].disposition,
        RecoveryDisposition::RecoveredCommitted
    );
    let receipt = reports[0].receipt.as_ref().unwrap();
    assert_eq!(manager.receipt(&receipt.receipt_id).unwrap(), *receipt);
    assert_eq!(
        fs::read(fixture.root.join("bin/game.dat")).unwrap(),
        b"new-game"
    );
    assert_eq!(
        fs::read(fixture.root.join("config/mintcat.ini")).unwrap(),
        b"new-config"
    );
}

#[test]
fn recovery_never_overwrites_an_external_change() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare_replacement_and_new();
    let error = fixture.manager.commit_with_crash(prepared, 2).unwrap_err();
    assert!(matches!(error, FileTransactionError::SimulatedCrash));
    fs::write(fixture.root.join("bin/game.dat"), b"external-change").unwrap();

    let reports = fixture
        .reopen_manager()
        .recover_incomplete(&stopped)
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].disposition,
        RecoveryDisposition::RecoveryRequired
    );
    assert!(reports[0].detail.contains("externally modified"));
    assert_eq!(
        fs::read(fixture.root.join("bin/game.dat")).unwrap(),
        b"external-change"
    );
}

#[test]
fn uninstall_recovers_every_rename_before_phase_update_and_the_commit_decision() {
    // replacement: quarantine + restore; addition: quarantine; decision: fourth crash point.
    for crash in 1..=4 {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("original"), b"original").unwrap();
        let receipt = fixture.install_files(&[("original", b"installed"), ("added", b"addition")]);
        let ids = vec![receipt.receipt_id.clone()];
        let error = fixture.manager.uninstall_chain_internal(
            &ids, &stopped, &FileCancellation::default(), Some(crash), None,
        ).unwrap_err();
        assert!(matches!(error, FileTransactionError::SimulatedCrash));
        let manager = fixture.reopen_manager();
        assert!(manager.has_unresolved().unwrap());
        let reports = manager.recover_incomplete(&stopped).unwrap();
        if crash == 4 {
            assert_eq!(reports[0].disposition, RecoveryDisposition::RecoveredCommitted);
            assert_eq!(fs::read(fixture.root.join("original")).unwrap(), b"original");
            assert!(!fixture.root.join("added").exists());
            assert!(matches!(manager.receipt(&receipt.receipt_id), Err(FileTransactionError::ReceiptInactive(_))));
        } else {
            assert_eq!(reports[0].disposition, RecoveryDisposition::RolledBack);
            assert_eq!(fs::read(fixture.root.join("original")).unwrap(), b"installed");
            assert_eq!(fs::read(fixture.root.join("added")).unwrap(), b"addition");
            assert_eq!(manager.receipt(&receipt.receipt_id).unwrap(), receipt);
        }
        assert!(!manager.has_unresolved().unwrap());
    }
}

#[test]
fn uninstall_validates_all_original_backups_before_any_target_write() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("a"), b"original-a").unwrap();
    fs::write(fixture.root.join("b"), b"original-b").unwrap();
    let receipt = fixture.install_files(&[("a", b"installed-a"), ("b", b"installed-b")]);
    let backup = fixture.root.join(TARGET_TRANSACTION_DIR).join(&receipt.transaction_id)
        .join(receipt.files[1].backup_relative_path.as_ref().unwrap());
    fs::write(backup, b"corruption").unwrap();
    assert!(matches!(
        fixture.manager.uninstall_chain(&[receipt.receipt_id.clone()], &stopped, &FileCancellation::default()),
        Err(FileTransactionError::RecoveryRequired { .. })
    ));
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"installed-a");
    assert_eq!(fs::read(fixture.root.join("b")).unwrap(), b"installed-b");
    assert_eq!(fixture.manager.receipt(&receipt.receipt_id).unwrap(), receipt);
}

#[test]
fn uninstall_chain_restores_oldest_original_once_and_rejects_partial_or_wrong_order() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("shared"), b"original").unwrap();
    let oldest = fixture.install_files(&[("shared", b"first"), ("first-only", b"one")]);
    let newest = fixture.install_files(&[("shared", b"second"), ("second-only", b"two")]);
    for invalid in [vec![newest.receipt_id.clone()], vec![oldest.receipt_id.clone(), newest.receipt_id.clone()]] {
        assert!(matches!(
            fixture.manager.uninstall_chain(&invalid, &stopped, &FileCancellation::default()),
            Err(FileTransactionError::InvalidRequest(_))
        ));
        assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"second");
    }
    let ids = vec![newest.receipt_id.clone(), oldest.receipt_id.clone()];
    let report = fixture.manager.uninstall_chain(&ids, &stopped, &FileCancellation::default()).unwrap();
    assert_eq!(report.processed_receipt_ids, ids);
    assert_eq!(report.restored, ["shared"]);
    assert_eq!(report.removed, ["second-only", "first-only"]);
    assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"original");
    assert!(!fixture.root.join("first-only").exists());
    assert!(!fixture.root.join("second-only").exists());
    assert!(fixture.manager.active_receipts_for_installation("install-1").unwrap().is_empty());
}

#[test]
fn uninstall_cancellation_before_mutation_keeps_receipts_and_during_commit_completes_chain() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("shared"), b"original").unwrap();
    let oldest = fixture.install_files(&[("shared", b"first")]);
    let newest = fixture.install_files(&[("shared", b"second"), ("new", b"new")]);
    let ids = vec![newest.receipt_id.clone(), oldest.receipt_id.clone()];
    let cancelled = FileCancellation::default();
    cancelled.request();
    let report = fixture.manager.uninstall_chain(&ids, &stopped, &cancelled).unwrap();
    assert_eq!(report.disposition, UninstallDisposition::Cancelled);
    assert!(report.processed_receipt_ids.is_empty());
    assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"second");
    assert_eq!(fixture.manager.active_receipts_for_installation("install-1").unwrap(), [newest, oldest]);
    let during = FileCancellation::default();
    let report = fixture.manager.uninstall_chain_internal(&ids, &stopped, &during, None, Some(1)).unwrap();
    assert_eq!(report.disposition, UninstallDisposition::Completed);
    assert!(report.cancel_requested_during_commit);
    assert_eq!(report.processed_receipt_ids, ids);
    assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"original");
    assert!(!fixture.root.join("new").exists());
    assert!(fixture.manager.active_receipts_for_installation("install-1").unwrap().is_empty());
}

#[test]
fn uninstall_recovery_fences_unknown_external_changes_and_blocks_new_installs() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("original"), b"original").unwrap();
    let receipt = fixture.install_files(&[("original", b"installed"), ("added", b"addition")]);
    let ids = vec![receipt.receipt_id.clone()];
    assert!(matches!(
        fixture.manager.uninstall_chain_internal(&ids, &stopped, &FileCancellation::default(), Some(2), None),
        Err(FileTransactionError::SimulatedCrash)
    ));
    fs::write(fixture.root.join("original"), b"user-change").unwrap();
    let manager = fixture.reopen_manager();
    let reports = manager.recover_incomplete(&stopped).unwrap();
    assert_eq!(reports[0].disposition, RecoveryDisposition::RecoveryRequired);
    assert_eq!(fs::read(fixture.root.join("original")).unwrap(), b"user-change");
    assert_eq!(fs::read(fixture.root.join("added")).unwrap(), b"addition");
    assert_eq!(manager.receipt(&receipt.receipt_id).unwrap(), receipt);
    let grant = fixture.grant(b"other");
    assert!(matches!(
        manager.prepare(fixture.request(vec![DeployFile {
            relative_path: "other".to_owned(), artifact: &grant, expected_target: ExpectedTarget::Absent,
            owner: "module:loader".to_owned(), install_mode: None,
        }]), &stopped),
        Err(FileTransactionError::RootBusy(_))
    ));
}

#[test]
fn uninstall_preserves_missing_and_modified_paths_across_entire_chain() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("foreign"), b"foreign").unwrap();
    let oldest = fixture.install_files(&[("modified", b"first"), ("missing", b"first")]);
    let newest = fixture.install_files(&[("modified", b"second"), ("missing", b"second")]);
    fs::write(fixture.root.join("modified"), b"user").unwrap();
    fs::remove_file(fixture.root.join("missing")).unwrap();
    let report = fixture.manager.uninstall_chain(
        &[newest.receipt_id, oldest.receipt_id], &stopped, &FileCancellation::default(),
    ).unwrap();
    assert_eq!(report.preserved_modified, ["modified"]);
    assert_eq!(report.missing, ["missing"]);
    assert_eq!(fs::read(fixture.root.join("modified")).unwrap(), b"user");
    assert!(!fixture.root.join("missing").exists());
    assert_eq!(fs::read(fixture.root.join("foreign")).unwrap(), b"foreign");
}

#[cfg(unix)]
#[test]
fn uninstall_physical_root_alias_excludes_conflicting_operation() {
    let fixture = Fixture::new();
    let receipt = fixture.install_files(&[("installed", b"installed")]);
    let alias = fixture.root.with_file_name("alias");
    std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
    let grant = fixture.grant(b"new");
    let mut request = fixture.request(vec![DeployFile {
        relative_path: "new".to_owned(), artifact: &grant, expected_target: ExpectedTarget::Absent,
        owner: "module:loader".to_owned(), install_mode: None,
    }]);
    request.root = alias;
    let held = fixture.manager.prepare(request, &stopped).unwrap();
    assert!(matches!(
        fixture.reopen_manager().uninstall_chain(&[receipt.receipt_id.clone()], &stopped, &FileCancellation::default()),
        Err(FileTransactionError::RootBusy(_))
    ));
    assert_eq!(fs::read(fixture.root.join("installed")).unwrap(), b"installed");
    drop(held);
    assert_eq!(fixture.manager.recover_incomplete(&stopped).unwrap()[0].disposition, RecoveryDisposition::RolledBack);
    let report = fixture.manager.uninstall_chain(&[receipt.receipt_id], &stopped, &FileCancellation::default()).unwrap();
    assert_eq!(report.removed, ["installed"]);
}

#[test]
fn interrupted_multi_receipt_uninstall_keeps_the_whole_newest_install_active() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("shared"), b"original").unwrap();
    let oldest = fixture.install_files(&[("shared", b"first"), ("old-added", b"old-added")]);
    let newest = fixture.install_files(&[("shared", b"second"), ("new-added", b"new-added")]);
    let ids = vec![newest.receipt_id.clone(), oldest.receipt_id.clone()];
    assert!(matches!(
        fixture.manager.uninstall_chain_internal(&ids, &stopped, &FileCancellation::default(), Some(3), None),
        Err(FileTransactionError::SimulatedCrash)
    ));
    let manager = fixture.reopen_manager();
    assert_eq!(manager.recover_incomplete(&stopped).unwrap()[0].disposition, RecoveryDisposition::RolledBack);
    assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"second");
    assert_eq!(fs::read(fixture.root.join("old-added")).unwrap(), b"old-added");
    assert_eq!(fs::read(fixture.root.join("new-added")).unwrap(), b"new-added");
    assert_eq!(manager.active_receipts_for_installation("install-1").unwrap(), [newest, oldest]);
    let report = manager.uninstall_chain(&ids, &stopped, &FileCancellation::default()).unwrap();
    assert_eq!(report.processed_receipt_ids, ids);
    assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"original");
    assert!(!fixture.root.join("old-added").exists());
    assert!(!fixture.root.join("new-added").exists());
}

#[test]
fn committed_uninstall_lookup_recovers_exact_chain_result_without_replaying_files() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("shared"), b"original").unwrap();
    let oldest = fixture.install_files(&[("shared", b"first")]);
    let newest = fixture.install_files(&[("shared", b"second"), ("added", b"addition")]);
    let ids = vec![newest.receipt_id.clone(), oldest.receipt_id.clone()];
    assert_eq!(fixture.manager.committed_uninstall_result(&newest.receipt_id).unwrap(), None);
    assert!(!fixture.manager.has_unresolved_for_installation("install-1").unwrap());
    let cancellation = FileCancellation::default();
    assert!(matches!(
        fixture.manager.uninstall_chain_internal(&ids, &stopped, &cancellation, Some(4), Some(1)),
        Err(FileTransactionError::SimulatedCrash)
    ));
    let manager = fixture.reopen_manager();
    assert!(manager.has_unresolved_for_installation("install-1").unwrap());
    assert!(!manager.has_unresolved_for_installation("other-installation").unwrap());
    assert_eq!(manager.committed_uninstall_result(&newest.receipt_id).unwrap(), None);
    assert_eq!(manager.recover_incomplete(&stopped).unwrap()[0].disposition, RecoveryDisposition::RecoveredCommitted);
    let expected = UninstallChainReport {
        processed_receipt_ids: ids, disposition: UninstallDisposition::Completed,
        restored: vec!["shared".to_owned()], removed: vec!["added".to_owned()],
        preserved_modified: Vec::new(), missing: Vec::new(), foreign_files_untouched: true,
        cancel_requested_during_commit: true,
    };
    assert_eq!(manager.committed_uninstall_result(&newest.receipt_id).unwrap(), Some(expected.clone()));
    assert_eq!(manager.committed_uninstall_result(&oldest.receipt_id).unwrap(), None);
    assert_eq!(manager.committed_uninstall_result("unknown-receipt").unwrap(), None);
    // A completed job replay must not perform a second uninstall or undo later user edits.
    fs::write(fixture.root.join("shared"), b"later-user-edit").unwrap();
    fs::write(fixture.root.join("added"), b"later-user-file").unwrap();
    assert_eq!(manager.committed_uninstall_result(&newest.receipt_id).unwrap(), Some(expected));
    assert_eq!(fs::read(fixture.root.join("shared")).unwrap(), b"later-user-edit");
    assert_eq!(fs::read(fixture.root.join("added")).unwrap(), b"later-user-file");
}
