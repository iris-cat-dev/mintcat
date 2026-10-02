use std::{collections::BTreeMap, io::Cursor, process::Stdio};

use mintcat_engine_unreal_pak::{
    worker_api::{WorkerEngineBuildRequest, WorkerEngineBuildResponse, WorkerEngineConflictsRequest,
        WorkerEngineInput, WorkerEngineVerifyRequest},
    FileConflict, InputInspection, UnrealInputFormat, VerificationReport,
    ENGINE_CONTRACT_ID, ENGINE_CONTRACT_VERSION, ENGINE_MODULE_ID, ENGINE_MODULE_VERSION,
};
use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{
    read_worker_message, write_worker_message, ArtifactGrantDescriptor, WorkerHello,
    WorkerHostHello, WorkerRequest, WorkerRequestKind, WorkerResponse, DIRECTORY_SNAPSHOT_MAGIC,
};
use mintcat_unreal_pak::{normalize_entry_path, PakArchive, PakBuildCodec, PakBuildTarget};
use serde_json::{json, Value};
use tokio::{process::Command, time::{timeout, Duration}};

fn snapshot(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = DIRECTORY_SNAPSHOT_MAGIC.to_vec();
    bytes.extend_from_slice(&(entries.len() as u64).to_be_bytes());
    for (path, content) in entries {
        bytes.extend_from_slice(&(path.len() as u32).to_be_bytes());
        bytes.extend_from_slice(path.as_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_be_bytes());
        bytes.extend_from_slice(content);
    }
    bytes
}

fn request(method: &str, params: Value, grants: Vec<ArtifactGrantDescriptor>) -> WorkerRequest {
    WorkerRequest {
        id: method.to_owned(),
        kind: WorkerRequestKind::Invoke,
        contract_id: Some(ENGINE_CONTRACT_ID.to_owned()),
        contract_version: Some(ENGINE_CONTRACT_VERSION),
        method: Some(method.to_owned()),
        params,
        grants,
    }
}

#[tokio::test]
async fn native_worker_uses_only_envelope_grants_and_preserves_outputs_after_shutdown() {
    timeout(Duration::from_secs(30), async {
        let sandbox = tempfile::tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("host")).unwrap()
            .initialize(RuntimeEnvironment::Test).unwrap();
        let host_store = ArtifactStore::from_layout(
            &layout, RuntimeEnvironment::Test, "test", "1", TrustStore::default(),
        ).unwrap();
        let winner = vec![b'a'; 64 * 1024];
        let shadowed = vec![b'b'; 64 * 1024];
        let first = host_store.publish_blob_reader(&mut Cursor::new(snapshot(&[
            ("Content/Shared.bin", &winner), ("Content/First.bin", b"first"),
        ])), None).unwrap();
        let second = host_store.publish_blob_reader(&mut Cursor::new(snapshot(&[
            ("Content/Shared.bin", &shadowed), ("Content/Second.bin", b"second"),
        ])), None).unwrap();
        let first = host_store.grant_blob(&first).unwrap();
        let second = host_store.grant_blob(&second).unwrap();
        let grants = vec![first.descriptor.clone(), second.descriptor.clone()];
        let inputs = vec![WorkerEngineInput {
            input_id: "first".to_owned(), grant_id: first.descriptor.grant_id.clone(),
            format: UnrealInputFormat::DirectorySnapshot, content_prefix: Some("FSD".to_owned()),
        }, WorkerEngineInput {
            input_id: "second".to_owned(), grant_id: second.descriptor.grant_id.clone(),
            format: UnrealInputFormat::DirectorySnapshot, content_prefix: Some("FSD".to_owned()),
        }];
        let workspace = sandbox.path().join("worker");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_mintcat-engine-unreal-pak-worker"))
            .env("MINTCAT_WORKER_WORKSPACE", &workspace)
            .env("MINTCAT_WORKER_INSTANCE_ID", "engine-test")
            .env("MINTCAT_WORKER_MODULE_ID", ENGINE_MODULE_ID)
            .env("MINTCAT_WORKER_MODULE_VERSION", ENGINE_MODULE_VERSION)
            .env("MINTCAT_WORKER_MODULE_DIGEST", format!("sha256:{}", "e".repeat(64)))
            .env("MINTCAT_WORKER_PROVIDES_JSON", json!([{
                "contractId": ENGINE_CONTRACT_ID, "version": ENGINE_CONTRACT_VERSION,
            }]).to_string())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
            .kill_on_drop(true).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let hello: WorkerHello = read_worker_message(&mut output).await.unwrap();
        write_worker_message(&mut input, &WorkerHostHello {
            protocol_version: hello.protocol_version, generation_id: "test".to_owned(),
            accepted: true, error: None,
        }).await.unwrap();

        // Even a real readable input is unavailable without its envelope grant.
        write_worker_message(&mut input, &request("inspect", serde_json::to_value(&inputs[0]).unwrap(), vec![]))
            .await.unwrap();
        let denied: WorkerResponse = read_worker_message(&mut output).await.unwrap();
        assert!(!denied.ok);
        assert!(denied.error.is_some());

        write_worker_message(&mut input, &request("inspect", serde_json::to_value(&inputs[0]).unwrap(), grants.clone()))
            .await.unwrap();
        let inspected: WorkerResponse = read_worker_message(&mut output).await.unwrap();
        assert!(inspected.ok, "{:?}", inspected.error);
        let inspected: InputInspection = serde_json::from_value(inspected.result.unwrap()).unwrap();
        assert_eq!(inspected.files, vec!["FSD/Content/First.bin", "FSD/Content/Shared.bin"]);

        write_worker_message(&mut input, &request("conflicts", serde_json::to_value(WorkerEngineConflictsRequest {
            inputs: inputs.clone(),
        }).unwrap(), grants.clone())).await.unwrap();
        let conflicts: WorkerResponse = read_worker_message(&mut output).await.unwrap();
        assert!(conflicts.ok, "{:?}", conflicts.error);
        let conflicts: Vec<FileConflict> = serde_json::from_value(conflicts.result.unwrap()).unwrap();
        assert_eq!(conflicts, vec![FileConflict {
            relative_path: "FSD/Content/Shared.bin".to_owned(), winner_input_id: "first".to_owned(),
            shadowed_input_ids: vec!["second".to_owned()],
        }]);

        write_worker_message(&mut input, &request("build", serde_json::to_value(WorkerEngineBuildRequest {
            plan_hash: "sha256:native-plan".to_owned(), inputs,
            reference_paths: vec!["FSD/Content/Shared.bin".to_owned()],
            target: PakBuildTarget { version: 9, mount_point: "../../../FSD/".to_owned(), codec: PakBuildCodec::Zlib },
        }).unwrap(), grants)).await.unwrap();
        let built: WorkerResponse = read_worker_message(&mut output).await.unwrap();
        assert!(built.ok, "{:?}", built.error);
        let built: WorkerEngineBuildResponse = serde_json::from_value(built.result.unwrap()).unwrap();
        assert_eq!(built.result.conflicts, conflicts);
        assert_eq!(built.result.artifact.digest, built.grant.digest);
        assert_eq!(built.result.artifact.size, built.grant.size);
        assert!(std::path::Path::new(&built.grant.path).starts_with(&workspace));
        write_worker_message(&mut input, &request("verify", serde_json::to_value(WorkerEngineVerifyRequest {
            grant_id: built.grant.grant_id.clone(), manifest: built.result.manifest.clone(),
        }).unwrap(), vec![built.grant.clone()])).await.unwrap();
        let verified: WorkerResponse = read_worker_message(&mut output).await.unwrap();
        assert!(verified.ok, "{:?}", verified.error);
        let verified: VerificationReport = serde_json::from_value(verified.result.unwrap()).unwrap();
        assert_eq!(verified.artifact_digest, built.result.artifact.digest);
        assert_eq!(verified.file_count, 3);
        assert_eq!(verified.expanded_bytes, winner.len() as u64 + 11);

        write_worker_message(&mut input, &WorkerRequest {
            id: "shutdown".to_owned(), kind: WorkerRequestKind::Shutdown,
            contract_id: None, contract_version: None, method: None,
            params: Value::Null, grants: vec![],
        }).await.unwrap();
        let drained: WorkerResponse = read_worker_message(&mut output).await.unwrap();
        assert!(drained.ok, "{:?}", drained.error);
        assert!(child.wait().await.unwrap().success());

        // Host publication occurs after shutdown: the service must not own a
        // TempDir lifetime that deletes its authenticated build output.
        let envelope = request("verify", Value::Null, vec![built.grant.clone()]);
        let output_grant = mintcat_runtime_artifacts::ArtifactGrant::from_worker_request(
            &envelope, &built.grant.grant_id,
        ).unwrap();
        let adopted = host_store.adopt_verified_grant(&output_grant).unwrap();
        assert_eq!(adopted.digest, built.result.artifact.digest);
        let adopted = host_store.grant_blob(&adopted).unwrap();
        let mut file = adopted.open_read().unwrap();
        let archive = PakArchive::open(&mut file).unwrap();
        let mut files = BTreeMap::new();
        for path in &archive.metadata().files {
            let mut bytes = Vec::new();
            archive.read_file_to(path, &mut file, &mut bytes).unwrap();
            files.insert(normalize_entry_path(&archive.metadata().mount_point, path).unwrap(), bytes);
        }
        assert_eq!(files, BTreeMap::from([
            ("FSD/Content/First.bin".to_owned(), b"first".to_vec()),
            ("FSD/Content/Second.bin".to_owned(), b"second".to_vec()),
            ("FSD/Content/Shared.bin".to_owned(), winner),
        ]));
    }).await.expect("native engine protocol timed out");
}
