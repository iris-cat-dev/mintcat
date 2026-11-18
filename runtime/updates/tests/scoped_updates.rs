mod common;
use common::Fixture;
use std::{collections::HashSet, fs};
use mintcat_runtime_updates::*;
use serde_json::json;

#[tokio::test]
async fn concurrent_scopes_cas_merge_latest_and_local_rollback_preserves_later_update() {
    let f = Fixture::new().await;
    for scope in ["game:drg", "game:rc"] { let (source, record) = f.package(scope, 1, scope.as_bytes(), None, false); f.publish(scope, 1, record, source); }
    let drg = f.stage("game:drg").await; let rc = f.stage("game:rc").await;
    let (a, b) = tokio::join!(f.updates.prepare(&drg.operation_id, 100), f.updates.prepare(&rc.operation_id, 100)); a.unwrap(); b.unwrap();
    f.updates.activate(&drg.operation_id, 100).unwrap(); f.updates.activate(&rc.operation_id, 100).unwrap();
    f.updates.confirm(&drg.operation_id, 100).await.unwrap(); f.updates.confirm(&rc.operation_id, 100).await.unwrap();
    let (source, record) = f.package("game:drg", 2, b"DRG second", None, false); f.publish("game:drg", 2, record, source);
    let drg2 = f.install("game:drg").await;
    let (source, record) = f.package("game:rc", 2, b"RC later", None, false); f.publish("game:rc", 2, record.clone(), source);
    let rc2 = f.install("game:rc").await;
    let error = f.updates.stage_local(&f.repository, "game:drg", "stable", 1, 100).await.unwrap_err(); assert!(matches!(error, UpdateError::Blocked(_)));
    f.updates.rollback(&drg2.operation_id, "consumer health failure").await.unwrap();
    let environment = f.updates.environment().unwrap();
    assert_eq!(environment.scopes["game:drg"].generation_id, drg.generation.generation_id);
    assert_eq!(environment.scopes["game:drg"].revision, 3);
    assert_eq!(environment.scopes["game:rc"].generation_id, rc2.generation.generation_id);
    assert_eq!(environment.scopes["game:rc"].targets.signed.packages[&record.digest], record);
}

#[tokio::test]
async fn parked_receipt_closure_survives_multiple_upgrades_then_reclaims_only_unleased_bytes() {
    let f = Fixture::new().await;
    let (source, oldest) = f.package("game:drg", 1, b"old task dependency", None, false); f.publish("game:drg", 1, oldest.clone(), source);
    f.install("game:drg").await;
    let lease = f.updates.retain_install_artifacts("saved-receipt", &[oldest.digest.clone()], false).unwrap();
    for sequence in 2..=4 { let (source, record) = f.package("game:drg", sequence, &[sequence as u8], None, false); f.publish("game:drg", sequence, record, source); f.install("game:drg").await; }
    for lease in f.updates.leases().unwrap().into_iter().filter(|l| l.kind == LeaseKind::Rollback) { f.updates.release_lease(&lease.lease_id).unwrap(); }
    assert!(f.updates.collect_garbage(999, &HashSet::new()).await.unwrap_err().to_string().contains("reconciliation"));
    f.updates.finish_lease_reconciliation().unwrap();
    let gc = f.updates.collect_garbage(999, &HashSet::new()).await.unwrap();
    assert!(!gc.removed.contains(&format!("module:{}", oldest.digest)));
    assert_eq!(f.artifacts.load_module(&oldest.digest).unwrap().manifest, oldest.manifest);
    f.updates.release_lease(&lease.lease_id).unwrap();
    let gc = f.updates.collect_garbage(999, &HashSet::new()).await.unwrap();
    assert!(gc.removed.contains(&format!("module:{}", oldest.digest)));
}

#[tokio::test]
async fn tampering_missing_payload_and_interrupted_probation_never_publish_an_incomplete_closure() {
    let f = Fixture::new().await;
    let (source, good) = f.package("game:drg", 1, b"confirmed", None, false); f.publish("game:drg", 1, good.clone(), source); let first = f.install("game:drg").await;
    let (source, bad) = f.package("game:drg", 2, b"new payload", None, false); f.publish("game:drg", 2, bad.clone(), source);
    let payload = f.repository.package_path(&bad.digest).unwrap().join("payload/data.bin");
    fs::write(&payload, b"tampered").unwrap();
    assert!(f.updates.stage_local(&f.repository, "game:drg", "stable", 1, 100).await.is_err());
    assert_eq!(f.updates.environment().unwrap().scopes["game:drg"].generation_id, first.generation.generation_id);
    let (source, missing) = f.package("game:drg", 3, b"missing", None, false); f.publish("game:drg", 3, missing.clone(), source);
    fs::remove_file(f.repository.package_path(&missing.digest).unwrap().join("payload/data.bin")).unwrap();
    assert!(f.updates.stage_local(&f.repository, "game:drg", "stable", 1, 100).await.is_err());
    let (source, ready) = f.package("game:drg", 4, b"fully staged", None, false); f.publish("game:drg", 4, ready, source);
    let op = f.stage("game:drg").await; f.updates.prepare(&op.operation_id, 100).await.unwrap(); f.updates.activate(&op.operation_id, 100).unwrap();
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    assert_eq!(restarted.operation(&op.operation_id).unwrap().phase, UpdatePhase::RolledBack);
    assert_eq!(restarted.environment().unwrap().scopes["game:drg"].generation_id, first.generation.generation_id);
    assert!(restarted.module_inventory().unwrap().modules.iter().any(|entry| entry.digest == good.digest));
}

#[tokio::test]
async fn repaired_artifact_io_failure_can_retry_the_same_signed_release() {
    let f = Fixture::new().await;
    let (source, record) = f.package("game:drg", 1, b"unchanged signed payload", None, false);
    f.publish("game:drg", 1, record.clone(), source);
    let modules = f.layout.artifacts_dir.join("modules");
    if modules.exists() { fs::remove_dir_all(&modules).unwrap(); }
    fs::write(&modules, b"blocked artifact directory").unwrap();
    let error = f.updates.stage_local(&f.repository, "game:drg", "stable", 0, 100).await.unwrap_err();
    assert!(matches!(error, UpdateError::Artifact(mintcat_runtime_artifacts::ArtifactError::Io(_))));
    let rejected = f.updates.operations().unwrap().pop().unwrap();
    assert_eq!(rejected.phase, UpdatePhase::Rejected);
    assert!(!rejected.quarantined);
    assert!(f.updates.environment().unwrap().scopes.is_empty());
    fs::remove_file(&modules).unwrap();
    fs::create_dir_all(modules.join("sha256")).unwrap();
    let retried = f.stage("game:drg").await;
    assert_eq!(retried.phase, UpdatePhase::Staged);
    assert_ne!(retried.operation_id, rejected.operation_id);
    assert_eq!(retried.generation.targets.signed.packages[&record.digest], record);
    assert_eq!(f.artifacts.load_module(&record.digest).unwrap().manifest, record.manifest);
}


#[tokio::test]
async fn real_failed_worker_ready_is_quarantined_before_publication() {
    let f = Fixture::new().await;
    let worker = std::path::Path::new(env!("CARGO_BIN_EXE_mintcat-update-readiness-failure"));
    let (source, record) = f.package("game:broken", 1, b"", Some(worker), false); f.publish("game:broken", 1, record, source);
    let op = f.stage("game:broken").await;
    assert!(f.updates.prepare(&op.operation_id, 100).await.is_err());
    assert_eq!(f.updates.operation(&op.operation_id).unwrap().phase, UpdatePhase::Rejected);
    assert!(!f.updates.environment().unwrap().scopes.contains_key("game:broken"));
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    assert!(restarted.stage_local(&f.repository, "game:broken", "stable", 0, 100).await.unwrap_err().to_string().contains("quarantined"));
}


#[tokio::test]
async fn signed_revocation_denies_new_admissions_but_preserves_required_running_closure() {
    use mintcat_runtime_release::{sign, MetadataHeader};
    let f = Fixture::new().await;
    let (source, record) = f.package("game:drg", 1, b"running closure", None, false); f.publish("game:drg", 1, record.clone(), source);
    let op = f.install("game:drg").await;
    let lease = f.updates.acquire_lease(Some("game:drg"), &op.generation.generation_id, "unfinished-commit", LeaseKind::Task, Some(1)).unwrap();
    let revoked = sign(RevocationMetadata { header: MetadataHeader { kind: "revocations".into(), schema_version: 1, version: 1, expires: 9000 }, digests: [record.digest.clone()].into(), reason: "signed security withdrawal".into() }, std::slice::from_ref(&f.key)).unwrap();
    let mut forged = revoked.clone(); forged.signed.reason = "tampered reason".into();
    assert!(f.updates.apply_revocations(&forged, 100).is_err());
    f.updates.apply_revocations(&revoked, 100).unwrap();
    assert!(f.updates.acquire_lease(Some("game:drg"), &op.generation.generation_id, "new-job", LeaseKind::Task, None).unwrap_err().to_string().contains("revoked"));
    assert_eq!(f.updates.leases().unwrap().into_iter().find(|l| l.lease_id == lease.lease_id).unwrap().digests, lease.digests);
    f.updates.finish_lease_reconciliation().unwrap();
    assert!(!f.updates.collect_garbage(999, &HashSet::new()).await.unwrap().removed.contains(&format!("module:{}", record.digest)));
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    assert!(restarted.acquire_lease(Some("game:drg"), &op.generation.generation_id, "restart-new-job", LeaseKind::Task, None).is_err());
}


#[tokio::test]
async fn authenticated_root_rotation_keeps_confirmed_historical_closures_verifiable_offline() {
    use std::collections::BTreeMap;
    use ed25519_dalek::SigningKey;
    use mintcat_runtime_release::*;
    let f = Fixture::new().await;
    let (source, old) = f.package("game:drg", 1, b"old publisher bytes", None, false); f.publish("game:drg", 1, old.clone(), source);
    let first = f.install("game:drg").await;
    let lease = f.updates.retain_install_artifacts("historical-receipt", &[old.digest.clone()], false).unwrap();
    let key = SigningKey::from_bytes(&[73; 32]);
    let key_id = key_id(&key.verifying_key()); let public = encode_public_key(&key.verifying_key());
    let next = sign(RootMetadata { header: MetadataHeader { kind: "root".into(), schema_version: 1, version: 2, expires: 10000 }, keys: BTreeMap::from([(key_id.clone(), public.clone())]), roles: ["root", "targets", "snapshot", "timestamp"].into_iter().map(|role| (role.into(), Role { key_ids: [key_id.clone()].into(), threshold: 1 })).collect(), publishers: BTreeMap::from([("fixture.publisher".into(), BTreeMap::from([("package".into(), public)]))]) }, &[f.key.clone(), key.clone()]).unwrap();
    let next_root = f.root.rotate(next.clone(), 100).unwrap();
    let input = f.sandbox.path().join("rotated-input"); fs::create_dir(&input).unwrap(); fs::write(input.join("data.bin"), b"new publisher bytes").unwrap();
    fs::copy(f.repository.package_path(&old.digest).unwrap().join("payload/worker"), input.join("worker")).unwrap();
    let mut manifest = old.manifest.clone(); manifest.version = "2.0.0".into(); manifest.contributes["games"][0]["adapterVersion"] = json!("2.0.0"); manifest.files[0].size = b"new publisher bytes".len() as u64; manifest.files[0].sha256 = sha256(b"new publisher bytes");
    let output = f.sandbox.path().join("rotated-package");
    let record = pack(&input, manifest, "package", &key, &f.policy, &output).unwrap();
    let previous = f.repository.current_bundle().unwrap().unwrap();
    let plan = ScopePlan { scope_id: "game:drg".into(), release_id: "drg-2".into(), sequence: 2, channel: "stable".into(), target: f.policy.target.clone(), min_host_api: "1".into(), roots: vec![record.digest.clone()], provider_bindings: vec![mintcat_sdk::ScopedProviderBinding { scope_id: "game:drg".into(), contract_id: mintcat_game_api::GAME_ADAPTER_CONTRACT_ID.into(), contract_version: mintcat_game_api::GAME_ADAPTER_CONTRACT_VERSION, provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone() }], singleton_requirements: vec![], resource_dependencies: BTreeMap::new(), cohort: CohortPolicy { salt: "fixture".into(), basis_points: 10000 } };
    let mut bundle = compose_bundle(Some(&previous), plan, vec![record.clone()], &f.policy, &next_root, std::slice::from_ref(&key), 2, 9000, 100).unwrap();
    bundle.root_rotations.push(next);
    f.repository.publish_bundle(&bundle, &[output], &f.root, &f.policy, 100).unwrap();
    let current = f.install("game:drg").await;
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    let old_store = restarted.artifact_store_for_generation("game:drg", &first.generation.generation_id).unwrap();
    assert_eq!(old_store.load_module(&old.digest).unwrap().manifest, old.manifest);
    let current_store = restarted.artifact_store_for_generation("game:drg", &current.generation.generation_id).unwrap();
    assert_eq!(current_store.load_module(&record.digest).unwrap().manifest, record.manifest);
    let mixed = restarted.artifact_store_for_digests(&[old.digest.clone(), record.digest.clone()]).unwrap();
    assert_eq!(mixed.load_module(&old.digest).unwrap().signature, old.signature);
    assert_eq!(mixed.load_module(&record.digest).unwrap().signature, record.signature);
    let mut unsigned_manifest = record.manifest.clone(); unsigned_manifest.version = "3.0.0".into();
    let unadmitted = f.sandbox.path().join("unadmitted-package");
    let unadmitted_record = pack(&input, unsigned_manifest, "package", &key, &f.policy, &unadmitted).unwrap();
    let publisher_store = f.artifacts.with_trust(next_root.artifact_trust_store().unwrap());
    publisher_store.publish_module(&unadmitted.join("payload"), &unadmitted_record.manifest, &unadmitted_record.signature).unwrap();
    assert!(matches!(mixed.load_module(&unadmitted_record.digest), Err(mintcat_runtime_artifacts::ArtifactError::SignatureVerification)));
    assert!(restarted.artifact_store_for_digests(&[unadmitted_record.digest]).is_err());
    assert_eq!(restarted.leases().unwrap().into_iter().find(|l| l.lease_id == lease.lease_id).unwrap().digests, lease.digests);
}

#[tokio::test]
async fn prepared_candidate_cannot_outlive_timestamp_and_refresh_does_not_quarantine_valid_package() {
    use mintcat_runtime_release::sign;
    let f = Fixture::new().await;
    let (source, first) = f.package("game:drg", 1, b"confirmed bytes", None, false); f.publish("game:drg", 1, first, source);
    let confirmed = f.install("game:drg").await;
    let (source, candidate) = f.package("game:drg", 2, b"new signed bytes", None, false); f.publish("game:drg", 2, candidate.clone(), source);
    let mut bundle = f.repository.current_bundle().unwrap().unwrap();
    bundle.timestamp.signed.header.version += 1;
    bundle.timestamp.signed.header.expires = 150;
    bundle.timestamp = sign(bundle.timestamp.signed, std::slice::from_ref(&f.key)).unwrap();
    f.repository.publish_bundle(&bundle, &[], &f.root, &f.policy, 100).unwrap();
    let operation = f.stage("game:drg").await;
    f.updates.prepare(&operation.operation_id, 149).await.unwrap();
    assert!(matches!(f.updates.activate(&operation.operation_id, 150), Err(UpdateError::MetadataExpired { .. })));
    assert_eq!(f.updates.environment().unwrap().scopes["game:drg"].generation_id, confirmed.generation.generation_id);
    bundle.timestamp.signed.header.version += 1;
    bundle.timestamp.signed.header.expires = 9000;
    bundle.timestamp = sign(bundle.timestamp.signed, std::slice::from_ref(&f.key)).unwrap();
    f.repository.publish_bundle(&bundle, &[], &f.root, &f.policy, 151).unwrap();
    let refreshed = f.updates.stage_local(&f.repository, "game:drg", "stable", 1, 151).await.unwrap();
    f.updates.prepare(&refreshed.operation_id, 151).await.unwrap();
    f.updates.activate(&refreshed.operation_id, 151).unwrap();
    f.updates.confirm(&refreshed.operation_id, 151).await.unwrap();
    let artifacts = f.updates.artifact_store_for_generation("game:drg", &refreshed.generation.generation_id).unwrap();
    assert_eq!(fs::read(artifacts.load_module(&candidate.digest).unwrap().root.join("data.bin")).unwrap(), b"new signed bytes");
}

#[tokio::test]
async fn running_game_work_cannot_enter_between_preparation_and_activation() {
    let f = Fixture::new().await;
    let (source, first) = f.package("game:drg", 1, b"confirmed", None, false);
    f.publish("game:drg", 1, first, source);
    let first = f.install("game:drg").await;
    let (source, candidate) = f.package("game:drg", 2, b"candidate", None, false);
    f.publish("game:drg", 2, candidate, source);
    let operation = f.stage("game:drg").await;
    f.updates.prepare(&operation.operation_id, 100).await.unwrap();
    let task = f.updates.acquire_lease(Some("game:drg"), &first.generation.generation_id, "late-game-task", LeaseKind::Task, None).unwrap();
    assert!(matches!(f.updates.activate(&operation.operation_id, 100), Err(UpdateError::Busy(_))));
    assert_eq!(f.updates.environment().unwrap().scopes["game:drg"].generation_id, first.generation.generation_id);
    f.updates.release_lease(&task.lease_id).unwrap();
    f.updates.activate(&operation.operation_id, 100).unwrap();
    f.updates.confirm(&operation.operation_id, 100).await.unwrap();
    assert_eq!(f.updates.environment().unwrap().scopes["game:drg"].generation_id, operation.generation.generation_id);
}

#[tokio::test]
async fn legacy_cutover_archives_exact_non_game_authority_and_removes_only_retired_process_leases() {
    use std::collections::BTreeMap;
    use mintcat_runtime_release::{sign, scope_metadata_key, MetadataHeader, TargetsMetadata};
    let f = Fixture::new().await;
    let (source, game) = f.package("game:drg", 1, b"game data", None, false);
    f.publish("game:drg", 1, game.clone(), source);
    let game = f.install("game:drg").await;
    let (path, record) = f.package("platform", 1, b"TEST ONLY historical platform bytes", None, false);
    f.artifacts.publish_module(&path.join("payload"), &record.manifest, &record.signature).unwrap();
    let release = mintcat_sdk::ScopeReleaseSet {
        schema_version: 1, scope_id: "platform".into(), release_id: "historical-platform".into(), sequence: 1,
        channel: "stable".into(), target: f.policy.target.clone(), min_host_api: "1".into(),
        packages: vec![mintcat_sdk::ReleasePackage { module_id: record.manifest.id.clone(), version: record.manifest.version.clone(), target: record.manifest.target.clone(), digest: record.digest.clone(), schema_version: 1 }],
        provider_bindings: vec![mintcat_sdk::ScopedProviderBinding { scope_id: "platform".into(), contract_id: mintcat_game_api::GAME_ADAPTER_CONTRACT_ID.into(), contract_version: mintcat_game_api::GAME_ADAPTER_CONTRACT_VERSION, provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone() }],
        singleton_requirements: vec![], signature_metadata_ref: scope_metadata_key("platform", "stable", &f.policy.target).unwrap(),
    };
    let targets = TargetsMetadata { header: MetadataHeader { kind: "targets".into(), schema_version: 1, version: 1, expires: 9000 }, release,
        cohort: mintcat_runtime_release::CohortPolicy { salt: "fixture".into(), basis_points: 10000 }, roots: vec![record.digest.clone()], resource_dependencies: BTreeMap::new(), packages: BTreeMap::from([(record.digest.clone(), record.clone())]) };
    let platform = ScopeGeneration { generation_id: "historical-platform-generation".into(), revision: 1,
        targets: sign(targets, std::slice::from_ref(&f.key)).unwrap(), verified_root: f.root.envelope().clone(), confirmed: true, new_writes: false };
    let connection = f.database.connection().unwrap();
    let encoded: String = connection.query_row("SELECT state_json FROM update_state WHERE id=1", [], |row| row.get(0)).unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    state.as_object_mut().unwrap().remove("schema_version");
    state["environment"]["scopes"]["platform"] = serde_json::to_value(&platform).unwrap();
    let environment = serde_json::to_value(EnvironmentSnapshot { generation_id: game.generation.generation_id.clone(), scopes: BTreeMap::from([("game:drg".into(), game.generation.clone())]) }).unwrap();
    for (id, owner, kind) in [("ui", "old-webview", "ui"), ("source-service", "source-generation:old", "service"), ("owner-service", "native-owner:library", "service"), ("source-task", "source-job:unfinished", "task"), ("receipt", "saved-install", "task"), ("rollback", "saved-rollback", "rollback"), ("game-service", "game-worker", "service")] {
        state["leases"][id] = json!({"leaseId":id,"ownerId":owner,"kind":kind,"environment":environment,"digests":game.generation.digests(),"expiresAt":null,"running":true});
    }
    connection.execute("UPDATE update_state SET state_json=?1 WHERE id=1", [serde_json::to_string(&state).unwrap()]).unwrap();
    drop(connection);
    let restarted = f.restarted();
    restarted.recover_startup().await.unwrap();
    assert!(!restarted.environment().unwrap().scopes.contains_key("platform"));
    let leases = restarted.leases().unwrap();
    for id in ["source-task", "receipt", "rollback", "game-service"] { assert!(leases.iter().any(|lease| lease.lease_id == id)); }
    for id in ["ui", "source-service", "owner-service"] { assert!(!leases.iter().any(|lease| lease.lease_id == id)); }
    let store = restarted.artifact_store_for_generation("platform", &platform.generation_id).unwrap();
    assert_eq!(store.load_module(&record.digest).unwrap().manifest, record.manifest);
    assert_eq!(restarted.module_inventory().unwrap().modules.len(), 1);
    assert!(restarted.supervisor_for_generation("platform", &platform.generation_id).is_err());
}

#[tokio::test]
async fn unfinished_historical_owner_migration_remains_fenced_with_its_original_snapshot() {
    let f = Fixture::new().await;
    let (source, record) = f.package("game:drg", 1, b"pending historical owner", None, false);
    f.publish("game:drg", 1, record, source);
    let operation = f.stage("game:drg").await;
    let connection = f.database.connection().unwrap();
    let encoded: String = connection.query_row("SELECT state_json FROM update_state WHERE id=1", [], |row| row.get(0)).unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    state.as_object_mut().unwrap().remove("schema_version");
    let snapshot = json!({"artifact":"TEST ONLY original owner snapshot","schemaVersion":1});
    state["operations"][&operation.operation_id]["backups"] = json!({"library":snapshot});
    state["operations"][&operation.operation_id]["drained"] = json!(["library"]);
    connection.execute("UPDATE update_state SET state_json=?1 WHERE id=1", [serde_json::to_string(&state).unwrap()]).unwrap();
    drop(connection);
    let restarted = f.restarted();
    assert!(restarted.recover_startup().await.unwrap_err().to_string().contains("original-application recovery"));
    let recovered = restarted.operation(&operation.operation_id).unwrap();
    assert_eq!(recovered.phase, UpdatePhase::RecoveryRequired);
    assert_eq!(recovered.backups["library"], snapshot);
    assert!(recovered.drained.contains("library"));
    assert!(restarted.environment().unwrap().scopes.is_empty());
    let (reason, blockers) = restarted.rescue_status().unwrap();
    assert!(reason.unwrap().contains(&operation.operation_id));
    assert!(blockers.is_empty());
}
