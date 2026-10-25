use std::{collections::HashSet, fs, path::Path, time::Duration};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_runtime_artifacts::{module_signing_digest, ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_runtime_supervisor::{
    InstanceLifecycle, Invocation, ModuleSupervisor, SupervisorError,
};
use mintcat_sdk::{
    ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleSignature, ModuleState,
    ProvidedContract, ScopedProviderBinding, SignatureAlgorithm,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

const CONTRACT: &str = "fixture.compute";

#[tokio::test]
async fn executes_real_worker_computation_and_artifact_grants_across_versions() {
    let sandbox = tempdir().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("data"))
        .unwrap()
        .initialize(RuntimeEnvironment::Test)
        .unwrap();
    let signing_key = SigningKey::from_bytes(&[19_u8; 32]);
    let mut trust = TrustStore::default();
    trust.trust_production_key("mintcat.test", "fixture", signing_key.verifying_key());
    let target = target_lexicon::HOST.to_string();
    let artifacts =
        ArtifactStore::from_layout(&layout, RuntimeEnvironment::Test, &target, "1", trust).unwrap();

    let first = publish_worker(&artifacts, sandbox.path(), &signing_key, &target, "1.0.0");
    let second = publish_worker(&artifacts, sandbox.path(), &signing_key, &target, "2.0.0");
    assert_ne!(first.digest, second.digest);

    let supervisor =
        ModuleSupervisor::new(artifacts.clone(), "daemon-generation", &layout.runtime_dir).unwrap();
    let first_instance = supervisor
        .activate(binding("scope-a", &first.digest))
        .await
        .unwrap();
    let second_instance = supervisor
        .activate(binding("scope-b", &second.digest))
        .await
        .unwrap();
    assert_ne!(first_instance, second_instance);

    let sum = supervisor
        .invoke(Invocation {
            scope_id: "scope-a",
            contract_id: CONTRACT,
            contract_version: 1,
            method: "math.sum",
            params: json!({ "values": [2, 3.5, 4] }),
            grants: &[],
            deadline: Duration::from_secs(5),
        })
        .await
        .unwrap();
    assert_eq!(sum, json!({ "sum": 9.5 }));

    // A valid signed worker's application errors retain structured diagnostics
    // without being mistaken for protocol failure or crashing its instance.
    let invalid = supervisor.invoke(Invocation {
        scope_id:"scope-a",contract_id:CONTRACT,contract_version:1,
        method:"math.sum",params:json!({}),grants:&[],
        deadline:Duration::from_secs(5),
    }).await.unwrap_err();
    assert!(matches!(invalid,
        SupervisorError::WorkerApplication {code,message_key,details}
            if code == "INVALID_ARGUMENT"
                && message_key == "worker.math.values_required"
                && details == json!({"field":"values"})
    ));
    assert!(supervisor.snapshots().iter().any(|instance|
        instance.instance_id == first_instance && instance.lifecycle == InstanceLifecycle::Active
    ));

    let input_path = sandbox.path().join("large-input.bin");
    fs::write(&input_path, b"real artifact input").unwrap();
    let artifact = artifacts
        .publish_blob_file(&input_path, Some("application/octet-stream".to_owned()))
        .unwrap();
    let grant = artifacts.grant_blob(&artifact).unwrap();
    let hashed = supervisor
        .invoke(Invocation {
            scope_id: "scope-b",
            contract_id: CONTRACT,
            contract_version: 1,
            method: "artifact.sha256",
            params: json!({ "grantId": grant.descriptor.grant_id }),
            grants: std::slice::from_ref(&grant),
            deadline: Duration::from_secs(5),
        })
        .await
        .unwrap();
    assert_eq!(hashed["digest"], artifact.digest);
    assert_eq!(hashed["size"], artifact.size);

    let gc = artifacts.collect_garbage(&HashSet::new()).unwrap();
    assert_eq!(gc.removed, Vec::<String>::new());
    assert!(gc.leased.contains(&format!("module:{}", first.digest)));
    assert!(gc.leased.contains(&format!("module:{}", second.digest)));
    assert!(gc.leased.contains(&format!("blob:{}", artifact.digest)));

    let crash = supervisor
        .invoke(Invocation {
            scope_id: "scope-a",
            contract_id: CONTRACT,
            contract_version: 1,
            method: "process.crash",
            params: json!({}),
            grants: &[],
            deadline: Duration::from_secs(5),
        })
        .await;
    assert!(matches!(
        crash,
        Err(SupervisorError::Protocol(_)) | Err(SupervisorError::WorkerExited { .. })
    ));
    let snapshots = supervisor.snapshots();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().any(|instance| {
        instance.instance_id == first_instance && instance.lifecycle == InstanceLifecycle::Crashed
    }));
    assert!(snapshots.iter().any(|instance| {
        instance.instance_id == second_instance && instance.lifecycle == InstanceLifecycle::Active
    }));

    let still_healthy = supervisor
        .invoke(Invocation {
            scope_id: "scope-b",
            contract_id: CONTRACT,
            contract_version: 1,
            method: "math.sum",
            params: json!({ "values": [10, 5] }),
            grants: &[],
            deadline: Duration::from_secs(5),
        })
        .await
        .unwrap();
    assert_eq!(still_healthy, json!({ "sum": 15.0 }));

    let invalid = supervisor
        .activate(ScopedProviderBinding {
            scope_id: "wrong".to_owned(),
            contract_id: "missing.contract".to_owned(),
            contract_version: 1,
            provider_module_id: "fixture.worker".to_owned(),
            provider_digest: second.digest.clone(),
        })
        .await
        .unwrap_err();
    assert!(matches!(invalid, SupervisorError::InvalidBinding(_)));

    drop(grant);
    supervisor
        .drain_binding("scope-a", CONTRACT, 1)
        .await
        .unwrap();
    supervisor
        .drain_binding("scope-b", CONTRACT, 1)
        .await
        .unwrap();
    let gc = artifacts.collect_garbage(&HashSet::new()).unwrap();
    assert!(gc.removed.contains(&format!("module:{}", first.digest)));
    assert!(gc.removed.contains(&format!("module:{}", second.digest)));
    assert!(gc.removed.contains(&format!("blob:{}", artifact.digest)));

    for snapshot in snapshots {
        let log = fs::read_to_string(snapshot.log_path).unwrap();
        assert!(log.contains("starting module fixture.worker@"));
    }
}

fn binding(scope: &str, digest: &str) -> ScopedProviderBinding {
    ScopedProviderBinding {
        scope_id: scope.to_owned(),
        contract_id: CONTRACT.to_owned(),
        contract_version: 1,
        provider_module_id: "fixture.worker".to_owned(),
        provider_digest: digest.to_owned(),
    }
}

fn publish_worker(
    store: &ArtifactStore,
    sandbox: &Path,
    signing_key: &SigningKey,
    target: &str,
    version: &str,
) -> mintcat_runtime_artifacts::InstalledModule {
    publish_package(store, sandbox, signing_key, target, version, "fixture.worker", CONTRACT, vec![])
}

fn publish_package(
    store: &ArtifactStore, sandbox: &Path, signing_key: &SigningKey,
    target: &str, version: &str, id: &str, contract: &str, requires: Vec<mintcat_sdk::RequiredContract>,
) -> mintcat_runtime_artifacts::InstalledModule {
    let source = sandbox.join(format!("worker-source-{id}-{version}"));
    fs::create_dir(&source).unwrap();
    let executable = Path::new(env!("CARGO_BIN_EXE_mintcat-worker-example"));
    let entry = executable
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let destination = source.join(&entry);
    fs::copy(executable, &destination).unwrap();
    let bytes = fs::read(&destination).unwrap();
    let file_digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    let manifest = ModuleManifest {
        schema_version: 1,
        id: id.to_owned(),
        version: version.to_owned(),
        publisher: "mintcat.test".to_owned(),
        target: target.to_owned(),
        host_api: "1".to_owned(),
        components: vec![ModuleComponent {
            kind: "service".to_owned(),
            entry: entry.clone(),
            runtime: "worker".to_owned(),
            state_model: "stateless".to_owned(),
        }],
        provides: vec![ProvidedContract {
            contract_id: contract.to_owned(),
            version: 1,
            cardinality: Some("single".to_owned()),
        }],
        requires,
        permissions: Vec::new(),
        contributes: json!({}),
        state: ModuleState {
            schema_version: 1,
            readable_range: "1".to_owned(),
            writable_range: "1".to_owned(),
            rollback_before_write_only: Some(true),
        },
        activation: ModuleActivation {
            trigger: "on-demand".to_owned(),
            drain_policy: "finish-requests".to_owned(),
        },
        files: vec![ModuleFile {
            path: entry,
            size: bytes.len() as u64,
            sha256: file_digest,
            executable: true,
        }],
    };
    let digest = module_signing_digest(&source, &manifest).unwrap();
    let signature = ModuleSignature {
        schema_version: 1,
        publisher: manifest.publisher.clone(),
        key_id: "fixture".to_owned(),
        algorithm: SignatureAlgorithm::Ed25519,
        signature: BASE64.encode(signing_key.sign(&digest).to_bytes()),
    };
    store
        .publish_module(&source, &manifest, &signature)
        .unwrap()
}

#[tokio::test]
async fn framed_callbacks_pin_engine_outputs_and_reject_forged_grants() {
    let sandbox = tempdir().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("data")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
    let key = SigningKey::from_bytes(&[23_u8; 32]);
    let mut trust = TrustStore::default();
    trust.trust_production_key("mintcat.test", "fixture", key.verifying_key());
    let target = target_lexicon::HOST.to_string();
    let artifacts = ArtifactStore::from_layout(&layout, RuntimeEnvironment::Test, &target, "1", trust).unwrap();
    let engine1 = publish_package(&artifacts, sandbox.path(), &key, &target, "1.0.0", "fixture.engine", "engine.unreal-pak", vec![]);
    let engine2 = publish_package(&artifacts, sandbox.path(), &key, &target, "2.0.0", "fixture.engine", "engine.unreal-pak", vec![]);
    let game = publish_package(&artifacts, sandbox.path(), &key, &target, "1.0.0", "fixture.game", CONTRACT, vec![mintcat_sdk::RequiredContract {
        contract_id: "engine.unreal-pak".into(), version_range: "=1".into(), optional: false, binding_scope: None,
    }]);
    let supervisor = ModuleSupervisor::new(artifacts.clone(), "frozen", &layout.runtime_dir).unwrap();
    for (scope, engine) in [("drg", &engine1), ("rc", &engine2)] {
        for (module, contract) in [(engine, "engine.unreal-pak"), (&game, CONTRACT)] {
            supervisor.activate(ScopedProviderBinding {
                scope_id: scope.into(), contract_id: contract.into(), contract_version: 1,
                provider_module_id: module.manifest.id.clone(), provider_digest: module.digest.clone(),
            }).await.unwrap();
        }
    }
    let artifact = artifacts.publish_blob_reader(&mut std::io::Cursor::new(b"actual-input"), None).unwrap();
    let grant = artifacts.grant_blob(&artifact).unwrap();
    let callback = |digest: &str| mintcat_sdk::WorkerBrokerRequest {
        id: String::new(), provider_digest: digest.into(), contract_id: "engine.unreal-pak".into(),
        contract_version: 1, method: "build".into(), params: json!({"grantId": grant.descriptor.grant_id}),
        grant_ids: vec![grant.descriptor.grant_id.clone()], produced: vec![],
    };
    let mut output_digests = Vec::new();
    for (scope, engine, expected) in [("drg", &engine1, b"actual-input-engine-1.0.0".as_slice()),
        ("rc", &engine2, b"actual-input-engine-2.0.0".as_slice())] {
        let output = supervisor.invoke(Invocation {
            scope_id: scope, contract_id: CONTRACT, contract_version: 1, method: "fixture.callback",
            params: serde_json::to_value(callback(&engine.digest)).unwrap(),
            grants: std::slice::from_ref(&grant), deadline: Duration::from_secs(5),
        }).await.unwrap();
        let descriptor: mintcat_sdk::ArtifactGrantDescriptor = serde_json::from_value(output["grant"].clone()).unwrap();
        assert_eq!(fs::read(&descriptor.path).unwrap(), expected);
        assert_eq!(descriptor.digest, format!("sha256:{:x}", Sha256::digest(expected)));
        output_digests.push(descriptor.digest);
    }
    assert_ne!(output_digests[0], output_digests[1]);
    let forged_path = sandbox.path().join("outside.bin");
    fs::write(&forged_path, b"actual-input").unwrap();
    let mut outside = grant.descriptor.clone();
    outside.grant_id = "forged".into();
    outside.path = forged_path.to_string_lossy().into_owned();
    let mut path_attack = callback(&engine1.digest);
    path_attack.params = json!({"grantId": "forged"});
    path_attack.grant_ids = vec!["forged".into()];
    path_attack.produced = vec![outside];
    let mut altered = callback(&engine1.digest);
    let mut altered_descriptor = grant.descriptor.clone();
    altered_descriptor.path = forged_path.to_string_lossy().into_owned();
    altered.produced = vec![altered_descriptor];
    let mut unknown = callback(&engine1.digest);
    unknown.grant_ids = vec!["ungranted".into()];
    for attack in [callback(&engine2.digest), path_attack, altered, unknown] {
        let denied = supervisor.invoke(Invocation {
            scope_id: "drg", contract_id: CONTRACT, contract_version: 1, method: "fixture.callback",
            params: serde_json::to_value(attack).unwrap(), grants: std::slice::from_ref(&grant),
            deadline: Duration::from_secs(5),
        }).await;
        assert!(matches!(denied, Err(SupervisorError::WorkerApplication { code, .. }) if code == "BROKER_REJECTED"));
    }
    assert!(matches!(supervisor.activate(ScopedProviderBinding {
        scope_id: "drg".into(), contract_id: "engine.unreal-pak".into(), contract_version: 1,
        provider_module_id: engine2.manifest.id.clone(), provider_digest: engine2.digest.clone(),
    }).await, Err(SupervisorError::InvalidBinding(_))));
    supervisor.shutdown_all().await.unwrap();
}

#[tokio::test]
async fn a_signed_module_provider_cannot_substitute_for_a_required_host_service() {
    let sandbox = tempdir().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("data")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
    let key = SigningKey::from_bytes(&[29_u8; 32]);
    let mut trust = TrustStore::default();
    trust.trust_production_key("mintcat.test", "fixture", key.verifying_key());
    let target = target_lexicon::HOST.to_string();
    let artifacts = ArtifactStore::from_layout(&layout, RuntimeEnvironment::Test, &target, "1", trust).unwrap();
    let provider = publish_package(&artifacts, sandbox.path(), &key, &target, "1.0.0", "fixture.network", "source.network", vec![]);
    let requirement = |scope: &str| mintcat_sdk::RequiredContract {
        contract_id: "source.network".into(), version_range: "=1".into(), optional: false, binding_scope: Some(scope.into()),
    };
    let host_consumer = publish_package(&artifacts, sandbox.path(), &key, &target, "1.0.0", "fixture.host-consumer", CONTRACT, vec![requirement("host")]);
    let scope_consumer = publish_package(&artifacts, sandbox.path(), &key, &target, "1.0.0", "fixture.scope-consumer", CONTRACT, vec![requirement("scope")]);
    let supervisor = ModuleSupervisor::new(artifacts, "host-boundary", &layout.runtime_dir).unwrap();
    let binding = |module: &mintcat_runtime_artifacts::InstalledModule, contract: &str| ScopedProviderBinding {
        scope_id: "game:drg".into(), contract_id: contract.into(), contract_version: 1,
        provider_module_id: module.manifest.id.clone(), provider_digest: module.digest.clone(),
    };
    supervisor.activate(binding(&provider, "source.network")).await.unwrap();
    let rejected = supervisor.activate(binding(&host_consumer, CONTRACT)).await.unwrap_err();
    assert!(matches!(rejected, SupervisorError::UnsatisfiedRequirement { contract_id, .. } if contract_id == "source.network"));
    supervisor.activate(binding(&scope_consumer, CONTRACT)).await.unwrap();
    let sum = supervisor.invoke(Invocation {
        scope_id: "game:drg", contract_id: CONTRACT, contract_version: 1, method: "math.sum",
        params: json!({"values": [2, 5]}), grants: &[], deadline: Duration::from_secs(5),
    }).await.unwrap();
    assert_eq!(sum, json!({"sum": 7.0}));
    supervisor.drain_binding("game:drg", CONTRACT, 1).await.unwrap();
    supervisor.drain_binding("game:drg", "source.network", 1).await.unwrap();
}
