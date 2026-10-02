mod common;
use common::Fixture;
use std::{collections::HashSet, fs, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use mintcat_runtime_updates::*;
use mintcat_sdk::ModuleManifest;
use serde_json::json;

#[tokio::test]
async fn concurrent_scopes_cas_merge_latest_and_local_rollback_preserves_later_update() {
    let f = Fixture::new().await;
    for scope in ["drg", "rc"] { let (source, record) = f.package(scope, 1, scope.as_bytes(), None, false); f.publish(scope, 1, record, source); }
    let drg = f.stage("drg").await; let rc = f.stage("rc").await;
    let (a, b) = tokio::join!(f.updates.prepare(&drg.operation_id, 100), f.updates.prepare(&rc.operation_id, 100)); a.unwrap(); b.unwrap();
    f.updates.activate(&drg.operation_id, 100).unwrap(); f.updates.activate(&rc.operation_id, 100).unwrap();
    f.updates.confirm(&drg.operation_id, 100).await.unwrap(); f.updates.confirm(&rc.operation_id, 100).await.unwrap();
    let (source, record) = f.package("drg", 2, b"DRG second", None, false); f.publish("drg", 2, record, source);
    let drg2 = f.install("drg").await;
    let (source, record) = f.package("rc", 2, b"RC later", None, false); f.publish("rc", 2, record.clone(), source);
    let rc2 = f.install("rc").await;
    let error = f.updates.stage_local(&f.repository, "drg", "stable", 1, 100).await.unwrap_err(); assert!(error.to_string().contains("Revision conflict"));
    f.updates.rollback(&drg2.operation_id, "consumer health failure").await.unwrap();
    let environment = f.updates.environment().unwrap();
    assert_eq!(environment.scopes["drg"].generation_id, drg.generation.generation_id);
    assert_eq!(environment.scopes["drg"].revision, 3);
    assert_eq!(environment.scopes["rc"].generation_id, rc2.generation.generation_id);
    assert_eq!(environment.scopes["rc"].targets.signed.packages[&record.digest], record);
}

#[tokio::test]
async fn running_old_closure_survives_expiry_and_multiple_upgrades_then_reclaims_only_unleased_bytes() {
    let f = Fixture::new().await;
    let (source, oldest) = f.package("drg", 1, b"old task dependency", None, false); f.publish("drg", 1, oldest.clone(), source);
    let first = f.install("drg").await;
    let lease = f.updates.acquire_lease(Some("drg"), &first.generation.generation_id, "old-install-job", LeaseKind::Task, Some(1)).unwrap();
    for sequence in 2..=4 { let (source, record) = f.package("drg", sequence, &[sequence as u8], None, false); f.publish("drg", sequence, record, source); f.install("drg").await; }
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
    let (source, good) = f.package("drg", 1, b"confirmed", None, false); f.publish("drg", 1, good.clone(), source); let first = f.install("drg").await;
    let (source, bad) = f.package("drg", 2, b"new payload", None, false); f.publish("drg", 2, bad.clone(), source);
    let payload = f.repository.package_path(&bad.digest).unwrap().join("payload/data.bin");
    fs::write(&payload, b"tampered").unwrap();
    assert!(f.updates.stage_local(&f.repository, "drg", "stable", 1, 100).await.is_err());
    assert_eq!(f.updates.environment().unwrap().scopes["drg"].generation_id, first.generation.generation_id);
    let (source, missing) = f.package("drg", 3, b"missing", None, false); f.publish("drg", 3, missing.clone(), source);
    fs::remove_file(f.repository.package_path(&missing.digest).unwrap().join("payload/data.bin")).unwrap();
    assert!(f.updates.stage_local(&f.repository, "drg", "stable", 1, 100).await.is_err());
    let (source, ready) = f.package("drg", 4, b"fully staged", None, false); f.publish("drg", 4, ready, source);
    let op = f.stage("drg").await; f.updates.prepare(&op.operation_id, 100).await.unwrap(); f.updates.activate(&op.operation_id, 100).unwrap();
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    assert_eq!(restarted.operation(&op.operation_id).unwrap().phase, UpdatePhase::RolledBack);
    assert_eq!(restarted.environment().unwrap().scopes["drg"].generation_id, first.generation.generation_id);
    assert!(restarted.module_inventory().unwrap().modules.iter().any(|entry| entry.digest == good.digest));
}

#[tokio::test]
async fn repaired_artifact_io_failure_can_retry_the_same_signed_release() {
    let f = Fixture::new().await;
    let (source, record) = f.package("drg", 1, b"unchanged signed payload", None, false);
    f.publish("drg", 1, record.clone(), source);
    let modules = f.layout.artifacts_dir.join("modules");
    if modules.exists() { fs::remove_dir_all(&modules).unwrap(); }
    fs::write(&modules, b"blocked artifact directory").unwrap();
    let error = f.updates.stage_local(&f.repository, "drg", "stable", 0, 100).await.unwrap_err();
    assert!(matches!(error, UpdateError::Artifact(mintcat_runtime_artifacts::ArtifactError::Io(_))));
    let rejected = f.updates.operations().unwrap().pop().unwrap();
    assert_eq!(rejected.phase, UpdatePhase::Rejected);
    assert!(!rejected.quarantined);
    assert!(f.updates.environment().unwrap().scopes.is_empty());
    fs::remove_file(&modules).unwrap();
    fs::create_dir_all(modules.join("sha256")).unwrap();
    let retried = f.stage("drg").await;
    assert_eq!(retried.phase, UpdatePhase::Staged);
    assert_ne!(retried.operation_id, rejected.operation_id);
    assert_eq!(retried.generation.targets.signed.packages[&record.digest], record);
    assert_eq!(f.artifacts.load_module(&record.digest).unwrap().manifest, record.manifest);
}


#[tokio::test]
async fn real_failed_worker_ready_is_quarantined_before_publication() {
    let f = Fixture::new().await;
    let worker = std::path::Path::new(env!("CARGO_BIN_EXE_mintcat-update-readiness-failure"));
    let (source, record) = f.package("broken", 1, b"", Some(worker), false); f.publish("broken", 1, record, source);
    let op = f.stage("broken").await;
    assert!(f.updates.prepare(&op.operation_id, 100).await.is_err());
    assert_eq!(f.updates.operation(&op.operation_id).unwrap().phase, UpdatePhase::Rejected);
    assert!(!f.updates.environment().unwrap().scopes.contains_key("broken"));
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    assert!(restarted.stage_local(&f.repository, "broken", "stable", 0, 100).await.unwrap_err().to_string().contains("quarantined"));
}

#[tokio::test]
async fn ui_barrier_requires_dispose_then_probation_ready_and_retains_old_generation_until_session_release() {
    let f = Fixture::new().await;
    let (source, old) = f.package("drg", 1, b"old static chunks", None, false); f.publish("drg", 1, old.clone(), source); f.install("drg").await;
    let old_environment = f.updates.environment().unwrap(); f.updates.register_ui_session("webview", &old_environment.generation_id).unwrap();
    f.updates.heartbeat_ui_session("webview", &old_environment.generation_id, 100).unwrap();
    f.updates.register_ui_session("second-webview", &old_environment.generation_id).unwrap();
    f.updates.heartbeat_ui_session("second-webview", &old_environment.generation_id, 100).unwrap();
    let (source, record) = f.package("drg", 2, b"new chunks", None, false); f.publish("drg", 2, record, source);
    let op = f.stage("drg").await; let op = f.updates.prepare(&op.operation_id, 100).await.unwrap(); let barrier = op.barrier.unwrap();
    assert!(f.updates.activate(&op.operation_id, 100).is_err());
    f.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id.clone(), session_id: "webview".into(), stage: UiStage::Deferred, state: Some(json!({"draft":"not lost"})), error: None, rebuilt: false }).await.unwrap();
    assert!(f.updates.activate(&op.operation_id, 100).is_err());
    f.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id.clone(), session_id: "webview".into(), stage: UiStage::Disposed, state: Some(json!({"draft":"retained"})), error: None, rebuilt: false }).await.unwrap();
    assert!(f.updates.activate(&op.operation_id, 100).is_err());
    f.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id.clone(), session_id: "second-webview".into(), stage: UiStage::Disposed, state: Some(json!({"draft":"second retained"})), error: None, rebuilt: false }).await.unwrap();
    f.updates.activate(&op.operation_id, 100).unwrap(); assert!(f.updates.confirm(&op.operation_id, 100).await.is_err());
    f.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id.clone(), session_id: "webview".into(), stage: UiStage::Ready, state: None, error: None, rebuilt: false }).await.unwrap();
    assert!(f.updates.ui_barriers("webview").unwrap().is_empty(), "ready client must not be asked to dispose/remount again");
    assert_eq!(f.updates.ui_barriers("second-webview").unwrap()[0].barrier_id, barrier.barrier_id);
    assert!(f.updates.confirm(&op.operation_id, 100).await.is_err());
    f.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id, session_id: "second-webview".into(), stage: UiStage::Ready, state: None, error: None, rebuilt: false }).await.unwrap();
    assert!(f.updates.ui_barriers("second-webview").unwrap().is_empty());
    assert!(f.restarted().ui_barriers("webview").unwrap().is_empty(), "terminal acknowledgement must survive a coordinator restart");
    f.updates.confirm(&op.operation_id, 100).await.unwrap();
    f.updates.release_lease(&format!("rollback:{}", op.operation_id)).unwrap(); f.updates.finish_lease_reconciliation().unwrap();
    assert!(f.updates.authorize_ui_session("webview", &old_environment.generation_id, 191).is_err());
    assert!(!f.updates.collect_garbage(999, &HashSet::new()).await.unwrap().removed.contains(&format!("module:{}", old.digest)));
    f.updates.close_ui_session("webview").unwrap();
    f.updates.close_ui_session("second-webview").unwrap();
    assert!(f.updates.collect_garbage(999, &HashSet::new()).await.unwrap().removed.contains(&format!("module:{}", old.digest)));
}

struct FileOwner { root: std::path::PathBuf, drained: AtomicBool }
impl StateOwner for FileOwner {
    fn owner_id(&self) -> &str { "fixture.state" }
    fn supports(&self, _: Option<&ModuleManifest>, _: &ModuleManifest) -> Result<()> { Ok(()) }
    fn drain(&self) -> OwnerFuture<'_, ()> { Box::pin(async { self.drained.store(true, Ordering::Release); Ok(()) }) }
    fn backup(&self, old_schema: u64, new_schema: u64) -> OwnerFuture<'_, OwnerBackup> { Box::pin(async move { assert!(self.drained.load(Ordering::Acquire)); fs::copy(self.root.join("state.json"), self.root.join("backup.json"))?; fs::File::open(self.root.join("backup.json"))?.sync_all()?; Ok(OwnerBackup { owner_id: self.owner_id().into(), old_schema, new_schema, reference: json!({"path":"backup.json"}) }) }) }
    fn migrate<'a>(&'a self, _: &'a OwnerBackup) -> OwnerFuture<'a, ()> { Box::pin(async { assert!(self.drained.load(Ordering::Acquire)); let mut value: serde_json::Value = serde_json::from_slice(&fs::read(self.root.join("state.json"))?)?; value["schema"] = json!(2); fs::write(self.root.join("state.json"), serde_json::to_vec(&value)?)?; Ok(()) }) }
    fn read_only_health(&self) -> OwnerFuture<'_, ()> { Box::pin(async { let value: serde_json::Value = serde_json::from_slice(&fs::read(self.root.join("state.json"))?)?; assert!(value["count"].as_u64().is_some()); Ok(()) }) }
    fn restore<'a>(&'a self, _: &'a OwnerBackup) -> OwnerFuture<'a, ()> { Box::pin(async { let value: serde_json::Value = serde_json::from_slice(&fs::read(self.root.join("state.json"))?)?; if value["count"] != json!(1) { return Err(UpdateError::Blocked("owner first-write fence".into())); } fs::copy(self.root.join("backup.json"), self.root.join("state.json"))?; Ok(()) }) }
    fn open_writes(&self) -> OwnerFuture<'_, ()> { Box::pin(async { self.drained.store(false, Ordering::Release); Ok(()) }) }
}
#[tokio::test]
async fn stateful_owner_new_write_boundary_refuses_old_database_restore() {
    let f = Fixture::new().await;
    fs::write(f.sandbox.path().join("state.json"), br#"{"schema":1,"count":1}"#).unwrap();
    let owner = Arc::new(FileOwner { root: f.sandbox.path().into(), drained: AtomicBool::new(false) }); f.updates.register_state_owner(owner).unwrap();
    let (source, record) = f.owned_resource("state", 1, b"old owner version", 1); f.publish("state", 1, record, source); f.install("state").await;
    let (source, record) = f.owned_resource("state", 2, b"schema owner", 2); f.publish("state", 2, record, source);
    let op = f.install("state").await;
    let _admission = f.updates.admit_mutation("state", &op.generation.generation_id).unwrap();
    fs::write(f.sandbox.path().join("state.json"), br#"{"schema":2,"count":99}"#).unwrap();
    drop(_admission);
    let error = f.updates.rollback(&op.operation_id, "UI first mount failed after a new write").await.unwrap_err();
    assert!(error.to_string().contains("first-new-write refused"));
    let state: serde_json::Value = serde_json::from_slice(&fs::read(f.sandbox.path().join("state.json")).unwrap()).unwrap(); assert_eq!(state, json!({"schema":2,"count":99}));
    assert_eq!(f.updates.environment().unwrap().scopes["state"].generation_id, op.generation.generation_id);
}

#[tokio::test]
async fn signed_revocation_denies_new_admissions_but_preserves_required_running_closure() {
    use mintcat_runtime_release::{sign, MetadataHeader};
    let f = Fixture::new().await;
    let (source, record) = f.package("drg", 1, b"running closure", None, false); f.publish("drg", 1, record.clone(), source);
    let op = f.install("drg").await;
    let lease = f.updates.acquire_lease(Some("drg"), &op.generation.generation_id, "unfinished-commit", LeaseKind::Task, Some(1)).unwrap();
    let revoked = sign(RevocationMetadata { header: MetadataHeader { kind: "revocations".into(), schema_version: 1, version: 1, expires: 9000 }, digests: [record.digest.clone()].into(), reason: "signed security withdrawal".into() }, std::slice::from_ref(&f.key)).unwrap();
    let mut forged = revoked.clone(); forged.signed.reason = "tampered reason".into();
    assert!(f.updates.apply_revocations(&forged, 100).is_err());
    f.updates.apply_revocations(&revoked, 100).unwrap();
    assert!(f.updates.acquire_lease(Some("drg"), &op.generation.generation_id, "new-job", LeaseKind::Task, None).unwrap_err().to_string().contains("revoked"));
    assert_eq!(f.updates.leases().unwrap().into_iter().find(|l| l.lease_id == lease.lease_id).unwrap().digests, lease.digests);
    f.updates.finish_lease_reconciliation().unwrap();
    assert!(!f.updates.collect_garbage(999, &HashSet::new()).await.unwrap().removed.contains(&format!("module:{}", record.digest)));
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    assert!(restarted.acquire_lease(Some("drg"), &op.generation.generation_id, "restart-new-job", LeaseKind::Task, None).is_err());
}

#[tokio::test]
async fn ui_enable_cas_changes_only_new_ui_view_and_keeps_old_task_and_pinned_view_immutable() {
    let f = Fixture::new().await;
    let (source, record) = f.package("drg", 1, b"authoritative signed package", None, false); f.publish("drg", 1, record.clone(), source);
    let op = f.install("drg").await;
    let old_view = f.updates.environment().unwrap();
    f.updates.register_ui_session("old-view", &old_view.generation_id).unwrap();
    let task = f.updates.acquire_lease(Some("drg"), &op.generation.generation_id, "existing-job", LeaseKind::Task, None).unwrap();
    let disabled = f.updates.set_ui_enabled("fixture.drg", false, &old_view.generation_id).unwrap();
    assert_eq!(disabled.modules.iter().find(|m| m.module_id == "fixture.drg").unwrap().ui_enabled, false);
    let pinned = f.updates.module_inventory_for_generation(&old_view.generation_id).unwrap();
    assert_eq!(pinned.modules.iter().find(|m| m.module_id == "fixture.drg").unwrap().ui_enabled, true);
    assert_eq!(f.updates.environment().unwrap().scopes["drg"].generation_id, op.generation.generation_id);
    assert_eq!(f.updates.leases().unwrap().into_iter().find(|l| l.lease_id == task.lease_id).unwrap().digests, task.digests);
    assert!(f.updates.set_ui_enabled("fixture.drg", true, &old_view.generation_id).unwrap_err().to_string().contains("expectedGenerationId"));
}

#[tokio::test]
async fn authenticated_root_rotation_keeps_confirmed_historical_closures_verifiable_offline() {
    use std::collections::BTreeMap;
    use ed25519_dalek::SigningKey;
    use mintcat_runtime_release::*;
    let f = Fixture::new().await;
    let (source, old) = f.package("drg", 1, b"old publisher bytes", None, false); f.publish("drg", 1, old.clone(), source);
    let first = f.install("drg").await;
    let lease = f.updates.acquire_lease(Some("drg"), &first.generation.generation_id, "historical-task", LeaseKind::Task, None).unwrap();
    let key = SigningKey::from_bytes(&[73; 32]);
    let key_id = key_id(&key.verifying_key()); let public = encode_public_key(&key.verifying_key());
    let next = sign(RootMetadata { header: MetadataHeader { kind: "root".into(), schema_version: 1, version: 2, expires: 10000 }, keys: BTreeMap::from([(key_id.clone(), public.clone())]), roles: ["root", "targets", "snapshot", "timestamp"].into_iter().map(|role| (role.into(), Role { key_ids: [key_id.clone()].into(), threshold: 1 })).collect(), publishers: BTreeMap::from([("fixture.publisher".into(), BTreeMap::from([("package".into(), public)]))]) }, &[f.key.clone(), key.clone()]).unwrap();
    let next_root = f.root.rotate(next.clone(), 100).unwrap();
    let input = f.sandbox.path().join("rotated-input"); fs::create_dir(&input).unwrap(); fs::write(input.join("data.bin"), b"new publisher bytes").unwrap();
    let mut manifest = old.manifest.clone(); manifest.version = "2.0.0".into(); manifest.files[0].size = b"new publisher bytes".len() as u64; manifest.files[0].sha256 = sha256(b"new publisher bytes");
    let output = f.sandbox.path().join("rotated-package");
    let record = pack(&input, manifest, "package", &key, &f.policy, &output).unwrap();
    let previous = f.repository.current_bundle().unwrap().unwrap();
    let plan = ScopePlan { scope_id: "drg".into(), release_id: "drg-2".into(), sequence: 2, channel: "stable".into(), target: f.policy.target.clone(), min_host_api: "1".into(), roots: vec![record.digest.clone()], provider_bindings: vec![], singleton_requirements: vec![], resource_dependencies: BTreeMap::new(), cohort: CohortPolicy { salt: "fixture".into(), basis_points: 10000 } };
    let mut bundle = compose_bundle(Some(&previous), plan, vec![record.clone()], &f.policy, &next_root, std::slice::from_ref(&key), 2, 9000, 100).unwrap();
    bundle.root_rotations.push(next);
    f.repository.publish_bundle(&bundle, &[output], &f.root, &f.policy, 100).unwrap();
    let current = f.install("drg").await;
    let restarted = f.restarted(); restarted.recover_startup().await.unwrap();
    let old_store = restarted.artifact_store_for_generation("drg", &first.generation.generation_id).unwrap();
    assert_eq!(old_store.load_module(&old.digest).unwrap().manifest, old.manifest);
    let current_store = restarted.artifact_store_for_generation("drg", &current.generation.generation_id).unwrap();
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
async fn dirty_barrier_cannot_outlive_timestamp_and_refresh_does_not_quarantine_valid_package() {
    use mintcat_runtime_release::sign;
    let f = Fixture::new().await;
    let (source, first) = f.package("drg", 1, b"confirmed bytes", None, false); f.publish("drg", 1, first, source);
    let confirmed = f.install("drg").await;
    let (source, candidate) = f.package("drg", 2, b"new signed bytes", None, false); f.publish("drg", 2, candidate.clone(), source);
    let mut bundle = f.repository.current_bundle().unwrap().unwrap();
    bundle.timestamp.signed.header.version += 1;
    bundle.timestamp.signed.header.expires = 150;
    bundle.timestamp = sign(bundle.timestamp.signed, std::slice::from_ref(&f.key)).unwrap();
    f.repository.publish_bundle(&bundle, &[], &f.root, &f.policy, 100).unwrap();
    let operation = f.stage("drg").await;
    f.updates.prepare(&operation.operation_id, 149).await.unwrap();
    assert!(matches!(f.updates.activate(&operation.operation_id, 150), Err(UpdateError::MetadataExpired { .. })));
    assert_eq!(f.updates.environment().unwrap().scopes["drg"].generation_id, confirmed.generation.generation_id);
    bundle.timestamp.signed.header.version += 1;
    bundle.timestamp.signed.header.expires = 9000;
    bundle.timestamp = sign(bundle.timestamp.signed, std::slice::from_ref(&f.key)).unwrap();
    f.repository.publish_bundle(&bundle, &[], &f.root, &f.policy, 151).unwrap();
    let refreshed = f.updates.stage_local(&f.repository, "drg", "stable", 1, 151).await.unwrap();
    f.updates.prepare(&refreshed.operation_id, 151).await.unwrap();
    f.updates.activate(&refreshed.operation_id, 151).unwrap();
    f.updates.confirm(&refreshed.operation_id, 151).await.unwrap();
    let artifacts = f.updates.artifact_store_for_generation("drg", &refreshed.generation.generation_id).unwrap();
    assert_eq!(fs::read(artifacts.load_module(&candidate.digest).unwrap().root.join("data.bin")).unwrap(), b"new signed bytes");
}

#[tokio::test]
async fn stateful_task_safe_point_waits_and_same_schema_code_rollback_preserves_new_data() {
    let f = Fixture::new().await;
    fs::write(f.sandbox.path().join("state.json"), br#"{"schema":2,"count":1}"#).unwrap();
    f.updates.register_state_owner(Arc::new(FileOwner { root: f.sandbox.path().into(), drained: AtomicBool::new(false) })).unwrap();
    let (source, old) = f.owned_resource("state", 1, b"old resource implementation", 2); f.publish("state", 1, old, source);
    let first = f.install("state").await;
    let task = f.updates.acquire_lease(Some("state"), &first.generation.generation_id, "unfinished-owner-task", LeaseKind::Task, Some(1)).unwrap();
    let (source, new) = f.owned_resource("state", 2, b"replacement implementation", 2); f.publish("state", 2, new, source);
    let op = f.stage("state").await;
    assert!(matches!(f.updates.prepare(&op.operation_id, 100).await, Err(UpdateError::Busy(_))));
    assert_eq!(f.updates.operation(&op.operation_id).unwrap().phase, UpdatePhase::WaitingForSafePoint);
    assert_eq!(f.updates.environment().unwrap().scopes["state"].generation_id, first.generation.generation_id);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&fs::read(f.sandbox.path().join("state.json")).unwrap()).unwrap(), json!({"schema":2,"count":1}));
    f.updates.release_lease(&task.lease_id).unwrap();
    f.updates.prepare(&op.operation_id, 100).await.unwrap(); f.updates.activate(&op.operation_id, 100).unwrap(); f.updates.confirm(&op.operation_id, 100).await.unwrap();
    let admission = f.updates.admit_mutation("state", &op.generation.generation_id).unwrap();
    fs::write(f.sandbox.path().join("state.json"), br#"{"schema":2,"count":99}"#).unwrap();
    drop(admission);
    f.updates.rollback(&op.operation_id, "compatible old implementation after UI failure").await.unwrap();
    assert_eq!(f.updates.environment().unwrap().scopes["state"].generation_id, first.generation.generation_id);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&fs::read(f.sandbox.path().join("state.json")).unwrap()).unwrap(), json!({"schema":2,"count":99}));
}

#[tokio::test]
async fn ui_dispatch_fence_denies_stale_bindings_without_recursively_locking_backup() {
    let f = Fixture::new().await;
    let (source, record) = f.ui_package("drg", 1); f.publish("drg", 1, record.clone(), source);
    f.install("drg").await;
    let generation = f.updates.environment().unwrap().generation_id;
    let session = f.updates.register_ui_session("atomic-dispatch", &generation).unwrap();
    let admission = f.updates.admit_ui_call("atomic-dispatch", &generation, &record.manifest.id, &record.digest, 100, false).await.unwrap();
    assert!(matches!(f.updates.set_ui_enabled(&record.manifest.id, false, &generation), Err(UpdateError::Busy(_))));
    let backup = mintcat_feature_backup::MaintenanceGate::exclusive(&f.layout, false).unwrap();
    drop(backup);
    drop(admission);
    f.updates.renew_lease(&session.lease_id, 100).unwrap();
    assert!(matches!(f.updates.admit_ui_call("atomic-dispatch", &generation, &record.manifest.id, &record.digest, 100, false).await, Err(UpdateError::StaleUiSession(_))));
    f.updates.close_ui_session("atomic-dispatch").unwrap();
    f.updates.register_ui_session("control-dispatch", &generation).unwrap();
    let writer = f.updates.admit_ui_call("control-dispatch", &generation, &record.manifest.id, &record.digest, 100, true).await.unwrap();
    let changed = f.updates.set_ui_enabled_with_ui_admission(&writer, &record.manifest.id, false, &generation).unwrap();
    assert_ne!(changed.generation_id, generation);
    drop(writer);
    assert!(matches!(f.updates.admit_ui_call("control-dispatch", &generation, &record.manifest.id, &record.digest, 100, false).await, Err(UpdateError::StaleUiSession(_))));
    assert!(f.updates.ui_artifact_authority(&changed.generation_id).unwrap().modules.is_empty());
}
