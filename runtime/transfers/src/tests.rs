use std::sync::Arc;

use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_broker::{CredentialVault, MemoryCredentialStore, NetworkBroker, Origin};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::ModRef;
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::Notify,
    time::{sleep, timeout, Duration},
};
use url::Url;

use super::*;

#[derive(Clone)]
struct ResponseScript {
    status: &'static str,
    etag: Option<&'static str>,
    content_range: Option<String>,
    declared_length: usize,
    first_chunk: Vec<u8>,
    remaining: Vec<u8>,
    sent_first: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

impl ResponseScript {
    fn complete(body: &[u8], etag: &'static str) -> Self {
        Self {
            status: "200 OK",
            etag: Some(etag),
            content_range: None,
            declared_length: body.len(),
            first_chunk: body.to_vec(),
            remaining: Vec::new(),
            sent_first: None,
            release: None,
        }
    }

    fn interrupted(body_prefix: &[u8], full_length: usize, etag: &'static str) -> Self {
        Self {
            status: "200 OK",
            etag: Some(etag),
            content_range: None,
            declared_length: full_length,
            first_chunk: body_prefix.to_vec(),
            remaining: Vec::new(),
            sent_first: None,
            release: None,
        }
    }

    fn partial(body: &[u8], offset: usize, full_length: usize, etag: &'static str) -> Self {
        Self {
            status: "206 Partial Content",
            etag: Some(etag),
            content_range: Some(format!(
                "bytes {offset}-{}/{full_length}",
                offset + body.len() - 1
            )),
            declared_length: body.len(),
            first_chunk: body.to_vec(),
            remaining: Vec::new(),
            sent_first: None,
            release: None,
        }
    }

    fn gated(
        body: &[u8],
        etag: &'static str,
        sent_first: Arc<Notify>,
        release: Arc<Notify>,
    ) -> Self {
        Self {
            status: "200 OK",
            etag: Some(etag),
            content_range: None,
            declared_length: body.len(),
            first_chunk: body[..1].to_vec(),
            remaining: body[1..].to_vec(),
            sent_first: Some(sent_first),
            release: Some(release),
        }
    }
}

async fn scripted_server(
    scripts: Vec<ResponseScript>,
) -> (Url, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/archive.bin",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for script in scripts {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            let mut headers = format!(
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                script.status, script.declared_length
            );
            if let Some(etag) = script.etag {
                headers.push_str(&format!("ETag: {etag}\r\n"));
            }
            if let Some(content_range) = script.content_range {
                headers.push_str(&format!("Content-Range: {content_range}\r\n"));
            }
            headers.push_str("\r\n");
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(&script.first_chunk).await.unwrap();
            if let Some(sent_first) = script.sent_first {
                sent_first.notify_one();
            }
            if let Some(release) = script.release {
                release.notified().await;
            }
            if !script.remaining.is_empty() {
                socket.write_all(&script.remaining).await.unwrap();
            }
        }
        requests
    });
    (url, server)
}

struct Fixture {
    _sandbox: TempDir,
    database: KernelDatabase,
    artifacts: ArtifactStore,
    broker: NetworkBroker,
    staging: PathBuf,
    manager: TransferManager,
}

fn fixture(progress_step_bytes: u64) -> Fixture {
    let sandbox = tempfile::tempdir().unwrap();
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
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let staging = sandbox.path().join("transfer-staging");
    let manager = TransferManager::open(
        database.clone(),
        artifacts.clone(),
        broker.clone(),
        &staging,
        TransferConfig {
            max_bytes: 1024 * 1024,
            progress_step_bytes,
        },
    )
    .unwrap();
    Fixture {
        _sandbox: sandbox,
        database,
        artifacts,
        broker,
        staging,
        manager,
    }
}

fn request(
    url: Url,
    version_id: &str,
    file_id: &str,
    body: &[u8],
    sharing: TransferSharing,
) -> TransferRequest {
    let source_id = "fixture".to_owned();
    let remote_game_key = "game".to_owned();
    let remote_mod_id = "mod-1".to_owned();
    let version_id = version_id.to_owned();
    let file_id = file_id.to_owned();
    TransferRequest {
        file_key: TransferFileKey {
            source_id: source_id.clone(),
            remote_game_key: remote_game_key.clone(),
            remote_mod_id: remote_mod_id.clone(),
            resolved_version_id: version_id.clone(),
            remote_file_id: file_id.clone(),
            byte_variant: "default".to_owned(),
        },
        sharing,
        descriptor: DownloadDescriptor {
            mod_ref: ModRef {
                source_id,
                remote_game_key,
                remote_mod_id,
                version_id: Some(version_id),
                remote_file_id: Some(file_id),
            },
            allowed_origins: vec![Origin::from_url(&url).unwrap()],
            url,
            headers: BTreeMap::new(),
            credential: None,
            expires_at: None,
            expected_size: body.len() as u64,
            expected_md5: Some(format!("{:x}", Md5::digest(body))),
        },
        validation: TransferValidation {
            expected_sha256: Some(format!("sha256:{:x}", Sha256::digest(body))),
            required_prefix: body[..body.len().min(2)].to_vec(),
            media_type: Some("application/octet-stream".to_owned()),
        },
    }
}

#[tokio::test]
async fn shared_transfer_survives_one_profile_cancellation() {
    let fixture = fixture(2);
    let sent_first = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let body = b"shared-content";
    let (url, server) = scripted_server(vec![ResponseScript {
        status: "200 OK",
        etag: Some("\"shared-v1\""),
        content_range: None,
        declared_length: body.len(),
        first_chunk: body[..3].to_vec(),
        remaining: body[3..].to_vec(),
        sent_first: Some(sent_first.clone()),
        release: Some(release.clone()),
    }])
    .await;
    let request = request(
        url,
        "version-1",
        "file-1",
        body,
        TransferSharing::StableFileIdentity,
    );
    let first = fixture.manager.attach("profile:a", &request).unwrap();
    let second = fixture.manager.attach("profile:b", &request).unwrap();
    assert_eq!(first.transfer_id, second.transfer_id);

    let manager = fixture.manager.clone();
    let transfer_id = first.transfer_id.clone();
    let task_request = request.clone();
    let transfer = tokio::spawn(async move { manager.run(&transfer_id, &task_request).await });
    sent_first.notified().await;
    fixture
        .manager
        .detach_consumer(&first.transfer_id, "profile:a")
        .unwrap();
    release.notify_one();

    let artifact = transfer.await.unwrap().unwrap();
    assert_eq!(
        fixture.manager.get(&first.transfer_id).unwrap().state,
        TransferState::Succeeded
    );
    assert!(fixture
        .manager
        .artifact_for_consumer(&first.transfer_id, "profile:b")
        .is_ok());
    assert!(fixture
        .manager
        .artifact_for_consumer(&first.transfer_id, "profile:a")
        .is_err());
    assert!(fixture
        .manager
        .retained_artifact_digests()
        .unwrap()
        .contains(&artifact.digest));
    let reopened = TransferManager::open(
        fixture.database.clone(),
        fixture.artifacts.clone(),
        fixture.broker.clone(),
        &fixture.staging,
        TransferConfig {
            max_bytes: 1024 * 1024,
            progress_step_bytes: 2,
        },
    )
    .unwrap();
    assert!(reopened
        .retained_artifact_digests()
        .unwrap()
        .contains(&artifact.digest));
    assert!(reopened
        .artifact_for_consumer(&first.transfer_id, "profile:b")
        .is_ok());
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn different_versions_of_one_mod_run_as_distinct_transfers() {
    let fixture = fixture(2);
    let first_started = Arc::new(Notify::new());
    let first_release = Arc::new(Notify::new());
    let second_started = Arc::new(Notify::new());
    let second_release = Arc::new(Notify::new());
    let first_body = b"version-one";
    let second_body = b"version-two";
    let (first_url, first_server) = scripted_server(vec![ResponseScript::gated(
        first_body,
        "\"v1\"",
        first_started.clone(),
        first_release.clone(),
    )])
    .await;
    let (second_url, second_server) = scripted_server(vec![ResponseScript::gated(
        second_body,
        "\"v2\"",
        second_started.clone(),
        second_release.clone(),
    )])
    .await;
    let first_request = request(
        first_url,
        "version-1",
        "file-1",
        first_body,
        TransferSharing::StableFileIdentity,
    );
    let second_request = request(
        second_url,
        "version-2",
        "file-2",
        second_body,
        TransferSharing::StableFileIdentity,
    );
    let first = fixture.manager.attach("profile:a", &first_request).unwrap();
    let second = fixture
        .manager
        .attach("profile:a", &second_request)
        .unwrap();
    assert_ne!(first.transfer_id, second.transfer_id);

    let first_manager = fixture.manager.clone();
    let first_transfer_id = first.transfer_id.clone();
    let first_run_request = first_request.clone();
    let first_run = tokio::spawn(async move {
        first_manager
            .run(&first_transfer_id, &first_run_request)
            .await
    });
    let second_manager = fixture.manager.clone();
    let second_transfer_id = second.transfer_id.clone();
    let second_run_request = second_request.clone();
    let second_run = tokio::spawn(async move {
        second_manager
            .run(&second_transfer_id, &second_run_request)
            .await
    });
    first_started.notified().await;
    second_started.notified().await;
    first_release.notify_one();
    second_release.notify_one();
    let (first_artifact, second_artifact) = tokio::join!(first_run, second_run);
    let first_artifact = first_artifact.unwrap();
    let second_artifact = second_artifact.unwrap();
    assert_ne!(
        first_artifact.unwrap().digest,
        second_artifact.unwrap().digest
    );
    assert_eq!(first_server.await.unwrap().len(), 1);
    assert_eq!(second_server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn interrupted_attempt_resumes_only_with_the_same_validator() {
    let fixture = fixture(1);
    let body = b"AAAABBBB";
    let (url, server) = scripted_server(vec![
        ResponseScript::interrupted(&body[..4], body.len(), "\"v1\""),
        ResponseScript::partial(&body[4..], 4, body.len(), "\"v1\""),
    ])
    .await;
    let request = request(
        url,
        "version-1",
        "file-1",
        body,
        TransferSharing::StableFileIdentity,
    );
    let transfer = fixture.manager.attach("profile:a", &request).unwrap();
    assert!(fixture
        .manager
        .run(&transfer.transfer_id, &request)
        .await
        .is_err());

    let artifact = fixture
        .manager
        .run(&transfer.transfer_id, &request)
        .await
        .unwrap();
    assert_eq!(artifact.size, body.len() as u64);
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    let resumed = requests[1].to_ascii_lowercase();
    assert!(resumed.contains("range: bytes=4-"));
    assert!(resumed.contains("if-range: \"v1\""));
}

#[tokio::test]
async fn changed_remote_validator_restarts_instead_of_appending() {
    let fixture = fixture(1);
    let old_prefix = b"AAAA";
    let current = b"CCCCDDDD";
    let (url, server) = scripted_server(vec![
        ResponseScript::interrupted(old_prefix, current.len(), "\"old\""),
        ResponseScript::complete(current, "\"new\""),
    ])
    .await;
    let request = request(
        url,
        "version-1",
        "file-1",
        current,
        TransferSharing::StableFileIdentity,
    );
    let transfer = fixture.manager.attach("profile:a", &request).unwrap();
    assert!(fixture
        .manager
        .run(&transfer.transfer_id, &request)
        .await
        .is_err());

    let artifact = fixture
        .manager
        .run(&transfer.transfer_id, &request)
        .await
        .unwrap();
    assert_eq!(
        artifact.digest,
        format!("sha256:{:x}", Sha256::digest(current))
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].to_ascii_lowercase().contains("range: bytes=4-"));
}

#[tokio::test]
async fn byte_completion_precedes_verification_publication_and_success() {
    let fixture = fixture(2);
    let body = b"phase-order";
    let (url, server) = scripted_server(vec![ResponseScript::complete(body, "\"phase\"")]).await;
    let request = request(
        url,
        "version-1",
        "file-1",
        body,
        TransferSharing::StableFileIdentity,
    );
    let transfer = fixture.manager.attach("profile:a", &request).unwrap();
    fixture
        .manager
        .run(&transfer.transfer_id, &request)
        .await
        .unwrap();
    let events = fixture.manager.events(&transfer.transfer_id).unwrap();
    let full_download = events
        .iter()
        .find(|event| event.downloaded_bytes == body.len() as u64)
        .unwrap();
    assert_eq!(full_download.state, TransferState::Downloading);
    let phases = events
        .iter()
        .map(|event| event.phase.as_str())
        .collect::<Vec<_>>();
    let verifying = phases
        .iter()
        .position(|phase| *phase == "verifying")
        .unwrap();
    let publishing = phases
        .iter()
        .position(|phase| *phase == "publishing")
        .unwrap();
    let succeeded = phases
        .iter()
        .position(|phase| *phase == "succeeded")
        .unwrap();
    assert!(verifying < publishing && publishing < succeeded);
    server.await.unwrap();
}

#[tokio::test]
async fn restart_recovers_incomplete_attempt_without_exposing_a_part_file() {
    let fixture = fixture(1);
    let sent_first = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let body = b"never-complete";
    let (url, server) = scripted_server(vec![ResponseScript {
        status: "200 OK",
        etag: Some("\"power-loss\""),
        content_range: None,
        declared_length: body.len(),
        first_chunk: body[..4].to_vec(),
        remaining: body[4..].to_vec(),
        sent_first: Some(sent_first.clone()),
        release: Some(release),
    }])
    .await;
    let request = request(
        url,
        "version-1",
        "file-1",
        body,
        TransferSharing::StableFileIdentity,
    );
    let transfer = fixture.manager.attach("profile:a", &request).unwrap();
    let manager = fixture.manager.clone();
    let transfer_id = transfer.transfer_id.clone();
    let task_request = request.clone();
    let running = tokio::spawn(async move { manager.run(&transfer_id, &task_request).await });
    sent_first.notified().await;
    timeout(Duration::from_secs(5), async {
        loop {
            if fixture
                .manager
                .events(&transfer.transfer_id)
                .unwrap()
                .iter()
                .any(|event| event.downloaded_bytes >= 4)
            {
                break;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("transfer did not persist its durable checkpoint");
    running.abort();
    let _ = running.await;

    let reopened = TransferManager::open(
        fixture.database.clone(),
        fixture.artifacts.clone(),
        fixture.broker.clone(),
        &fixture.staging,
        TransferConfig {
            max_bytes: 1024 * 1024,
            progress_step_bytes: 1,
        },
    )
    .unwrap();
    let recovered = reopened.get(&transfer.transfer_id).unwrap();
    assert_eq!(recovered.state, TransferState::Queued);
    assert!(recovered.artifact.is_none());
    assert!(reopened
        .artifact_for_consumer(&transfer.transfer_id, "profile:a")
        .is_err());
    server.abort();
}

#[tokio::test]
async fn unshareable_transfers_deduplicate_only_after_digest_publication() {
    let fixture = fixture(2);
    let body = b"same-bytes";
    let (url, server) = scripted_server(vec![
        ResponseScript::complete(body, "\"one\""),
        ResponseScript::complete(body, "\"two\""),
    ])
    .await;
    let request = request(
        url,
        "version-1",
        "file-1",
        body,
        TransferSharing::AfterDigestOnly,
    );
    let first = fixture.manager.attach("profile:a", &request).unwrap();
    let second = fixture.manager.attach("profile:b", &request).unwrap();
    assert_ne!(first.transfer_id, second.transfer_id);

    let first_artifact = fixture
        .manager
        .run(&first.transfer_id, &request)
        .await
        .unwrap();
    let second_artifact = fixture
        .manager
        .run(&second.transfer_id, &request)
        .await
        .unwrap();
    assert_eq!(first_artifact.digest, second_artifact.digest);
    assert_eq!(server.await.unwrap().len(), 2);
}
