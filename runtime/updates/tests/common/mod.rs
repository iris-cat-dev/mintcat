#![allow(dead_code)]
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}};
use ed25519_dalek::SigningKey;
use mintcat_runtime_artifacts::ArtifactStore;
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimeLayout, RuntimePaths};
use mintcat_runtime_release::*;
use mintcat_runtime_updates::*;
use mintcat_sdk::{ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState, ProvidedContract};
use serde_json::json;
#[path = "../fixtures/game_worker.rs"] mod game_worker;

pub struct Fixture {
    pub sandbox: tempfile::TempDir,
    pub layout: RuntimeLayout,
    pub database: KernelDatabase,
    pub artifacts: ArtifactStore,
    pub updates: UpdatesCoordinator,
    pub repository: LocalRepository,
    pub root: TrustedRoot,
    pub key: SigningKey,
    pub policy: VerificationPolicy,
}
impl Fixture {
    pub async fn new() -> Self {
        let sandbox = tempfile::tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("runtime")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let key = SigningKey::from_bytes(&[39; 32]);
        let key_id = key_id(&key.verifying_key());
        let public = encode_public_key(&key.verifying_key());
        let root = trust_pinned_root(sign(RootMetadata { header: MetadataHeader { kind: "root".into(), schema_version: 1, version: 1, expires: 10000 }, keys: BTreeMap::from([(key_id.clone(), public.clone())]), roles: ["root", "targets", "snapshot", "timestamp"].into_iter().map(|r| (r.into(), Role { key_ids: [key_id.clone()].into(), threshold: 1 })).collect(), publishers: BTreeMap::from([("fixture.publisher".into(), BTreeMap::from([("package".into(), public)]))]) }, std::slice::from_ref(&key)).unwrap(), 100).unwrap();
        let policy = VerificationPolicy { target: target_lexicon::HOST.to_string(), host_api: "1".into(), max_data_schema: 2, permissions: vec![], contracts: BTreeMap::from([(mintcat_game_api::GAME_ADAPTER_CONTRACT_ID.into(), [mintcat_game_api::GAME_ADAPTER_CONTRACT_VERSION].into())]) };
        let artifacts = ArtifactStore::from_layout(&layout, RuntimeEnvironment::Test, &policy.target, "1", root.artifact_trust_store().unwrap()).unwrap();
        let database = KernelDatabase::open(&layout).unwrap();
        let updates = UpdatesCoordinator::open(layout.clone(), database.clone(), artifacts.clone(), RepositoryVerifier::new(layout.state_dir.join("trust-state.json"), root.clone()), policy.clone(), "fixture-installation".into()).unwrap();
        updates.recover_startup().await.unwrap();
        let repository = LocalRepository::open(&sandbox.path().join("repository")).unwrap();
        Self { sandbox, layout, database, artifacts, updates, repository, root, key, policy }
    }
    pub fn package(&self, scope: &str, sequence: u64, bytes: &[u8], worker: Option<&Path>, stateful: bool) -> (PathBuf, PackageRecord) {
        let source = self.sandbox.path().join(format!("source-{}-{sequence}", scope.replace(':', "-"))); fs::create_dir(&source).unwrap();
        let worker = worker.unwrap_or_else(|| Path::new(option_env!("CARGO_BIN_EXE_mintcat-update-game-fixture").expect("test fixture worker executable required")));
        fs::copy(worker, source.join("worker")).unwrap();
        fs::write(source.join("data.bin"), bytes).unwrap();
        let worker_bytes = fs::read(source.join("worker")).unwrap();
        let manifest = ModuleManifest { schema_version: 1, id: format!("fixture.{scope}"), version: format!("{sequence}.0.0"), publisher: "fixture.publisher".into(), target: self.policy.target.clone(), host_api: "1".into(),
            components: vec![ModuleComponent { kind: "service".into(), runtime: "worker".into(), entry: "worker".into(), state_model: if stateful { "single-writer" } else { "stateless" }.into() }],
            provides: vec![ProvidedContract { contract_id: mintcat_game_api::GAME_ADAPTER_CONTRACT_ID.into(), version: mintcat_game_api::GAME_ADAPTER_CONTRACT_VERSION, cardinality: Some("multi".into()) }], requires: vec![], permissions: vec![],
            contributes: json!({"scopeId":scope,"games":[game_worker::descriptor(scope, &format!("{sequence}.0.0"))]}),
            state: ModuleState { schema_version: 1, readable_range: "1".into(), writable_range: "1".into(), rollback_before_write_only: Some(true) }, activation: ModuleActivation { trigger: "on-demand".into(), drain_policy: "finish-requests".into() },
            files: vec![ModuleFile { path: "data.bin".into(), size: bytes.len() as u64, sha256: sha256(bytes), executable: false }, ModuleFile { path: "worker".into(), size: worker_bytes.len() as u64, sha256: sha256(&worker_bytes), executable: true }] };
        let output = self.sandbox.path().join(format!("package-{}-{sequence}", scope.replace(':', "-")));
        let record = pack(&source, manifest, "package", &self.key, &self.policy, &output).unwrap(); (output, record)
    }
    pub fn publish(&self, scope: &str, sequence: u64, record: PackageRecord, source: PathBuf) {
        let previous = self.repository.current_bundle().unwrap();
        let version = previous.as_ref().map_or(1, |b| b.snapshot.signed.header.version + 1);
        let provider_bindings = record.manifest.provides.iter().map(|contract| mintcat_sdk::ScopedProviderBinding { scope_id: scope.into(), contract_id: contract.contract_id.clone(), contract_version: contract.version, provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone() }).collect();
        let plan = ScopePlan { scope_id: scope.into(), release_id: format!("{scope}-{sequence}"), sequence, channel: "stable".into(), target: self.policy.target.clone(), min_host_api: "1".into(), roots: vec![record.digest.clone()], provider_bindings, singleton_requirements: vec![], resource_dependencies: BTreeMap::new(), cohort: CohortPolicy { salt: "fixture".into(), basis_points: 10000 } };
        let bundle = compose_bundle(previous.as_ref(), plan, vec![record], &self.policy, &self.root, std::slice::from_ref(&self.key), version, 9000, 100).unwrap();
        self.repository.publish_bundle(&bundle, &[source], &self.root, &self.policy, 100).unwrap();
    }
    pub async fn stage(&self, scope: &str) -> UpdateOperation { self.updates.stage_local(&self.repository, scope, "stable", self.updates.scope_revision(scope).unwrap(), 100).await.unwrap() }
    pub async fn install(&self, scope: &str) -> UpdateOperation {
        let op = self.stage(scope).await; self.updates.prepare(&op.operation_id, 100).await.unwrap(); self.updates.activate(&op.operation_id, 100).unwrap(); self.updates.confirm(&op.operation_id, 100).await.unwrap()
    }
    pub fn restarted(&self) -> UpdatesCoordinator { UpdatesCoordinator::open(self.layout.clone(), self.database.clone(), self.artifacts.clone(), RepositoryVerifier::new(self.layout.state_dir.join("trust-state.json"), self.root.clone()), self.policy.clone(), "fixture-installation".into()).unwrap() }
}
