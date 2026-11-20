use std::{
    io::{Read as _, Write as _},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flate2::{write::GzEncoder, Compression};
use mintcat_runtime_jobs::{ConfirmationPolicy, EventUpdate, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::JobState;
use serde_json::json;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
};
use url::Url;

use crate::{
    interaction_binding, open_interaction_broker, BrokerError, BrowserAuthorizationRequest,
    BrowserNavigationDecision, CancellationToken, CredentialBinding, CredentialPlacement,
    CredentialRef, CredentialStore, CredentialVault, FileSelectionRequest, ForwardProxy,
    InteractionContext, MemoryCredentialStore, NetworkBroker, NetworkRequest, Origin, ProxyPolicy,
    RouteKind, SecretValue,
};

async fn bind_server() -> (TcpListener, Url) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    (listener, url)
}

fn serve_once(listener: TcpListener, response: String) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
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
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap()
    })
}

fn ok_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn memory_vault() -> CredentialVault {
    CredentialVault::new(Arc::new(MemoryCredentialStore::default()))
}

#[tokio::test]
async fn credentials_are_never_forwarded_outside_their_origin_scope() {
    let (redirect_listener, redirect_url) = bind_server().await;
    let (destination_listener, destination_url) = bind_server().await;
    let redirect_response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        destination_url
    );
    let redirect_request = serve_once(redirect_listener, redirect_response);
    let destination_request = serve_once(destination_listener, ok_response("done"));

    let vault = memory_vault();
    let credential = vault
        .store(SecretValue::new("source-secret").unwrap())
        .unwrap();
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let mut request = NetworkRequest::get(
        "redirect-test",
        redirect_url.clone(),
        vec![
            Origin::from_url(&redirect_url).unwrap(),
            Origin::from_url(&destination_url).unwrap(),
        ],
    );
    request.credential = Some(CredentialBinding {
        reference: credential,
        placement: CredentialPlacement::Bearer,
        allowed_origins: vec![Origin::from_url(&redirect_url).unwrap()],
    });

    let response = broker
        .execute(&request, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(response.body, b"done");
    let first = redirect_request.await.unwrap().to_ascii_lowercase();
    let second = destination_request.await.unwrap().to_ascii_lowercase();
    assert!(first.contains("authorization: bearer source-secret"));
    assert!(!second.contains("authorization"));
    assert!(!second.contains("source-secret"));
}

#[tokio::test]
async fn redirect_scope_and_diagnostics_reject_and_redact_untrusted_targets() {
    let (listener, url) = bind_server().await;
    let response = "HTTP/1.1 302 Found\r\nLocation: https://outside.invalid/callback?token=leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    let server = serve_once(listener, response);
    let vault = memory_vault();
    let credential = vault
        .store(SecretValue::new("never-log-this").unwrap())
        .unwrap();
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let mut request = NetworkRequest::get(
        "redirect-reject",
        url.clone(),
        vec![Origin::from_url(&url).unwrap()],
    );
    request.credential = Some(CredentialBinding {
        reference: credential,
        placement: CredentialPlacement::Header("x-provider-token".to_owned()),
        allowed_origins: vec![Origin::from_url(&url).unwrap()],
    });

    let error = broker
        .execute(&request, &CancellationToken::default())
        .await
        .unwrap_err();
    assert!(matches!(error, BrokerError::DisallowedOrigin(_)));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains("never-log-this"));
    assert!(!diagnostic.contains("token=leak"));
    server.await.unwrap();
}

#[tokio::test]
async fn direct_failure_uses_the_configured_forward_proxy() {
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_url = Url::parse(&format!(
        "http://{}/archive.zip?signature=private",
        unavailable.local_addr().unwrap()
    ))
    .unwrap();
    drop(unavailable);
    let (proxy_listener, proxy_url) = bind_server().await;
    let proxy_request = serve_once(proxy_listener, ok_response("proxied"));
    let vault = memory_vault();
    let broker =
        NetworkBroker::new(&vault, Some(ForwardProxy::new(proxy_url.clone()).unwrap())).unwrap();
    let mut request = NetworkRequest::get(
        "proxy-fallback",
        target_url.clone(),
        vec![Origin::from_url(&target_url).unwrap()],
    );
    request.proxy_policy = ProxyPolicy::DirectThenForward;
    request.timeout = Duration::from_secs(2);

    let response = broker
        .execute(&request, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(response.route, RouteKind::ForwardProxy);
    assert_eq!(response.body, b"proxied");
    let received = proxy_request.await.unwrap();
    assert!(received.starts_with("GET /?url="));
    assert!(received.contains("archive.zip"));
}

#[tokio::test]
async fn explicit_routes_ignore_ambient_proxies_for_buffered_and_streaming_requests() {
    const CHILD_ENV: &str = "MINTCAT_BROKER_ROUTE_REGRESSION_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        // A child process gives reqwest ambient proxies without mutating this
        // process's environment or racing other concurrently running tests.
        let ambient_proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ambient_url = format!("http://{}", ambient_proxy.local_addr().unwrap());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::explicit_routes_ignore_ambient_proxies_for_buffered_and_streaming_requests",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env("NO_PROXY", "")
            .env("no_proxy", "");
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            command.env(name, &ambient_url);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "isolated route regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    for streaming in [false, true] {
        for policy in [ProxyPolicy::Direct, ProxyPolicy::ForwardOnly] {
            let (target_listener, target_url) = bind_server().await;
            let (proxy_listener, proxy_url) = bind_server().await;
            let (server, expected_route, expected_body) = match policy {
                ProxyPolicy::Direct => {
                    drop(proxy_listener);
                    (
                        serve_once(target_listener, ok_response("direct-response")),
                        RouteKind::Direct,
                        b"direct-response".as_slice(),
                    )
                }
                ProxyPolicy::ForwardOnly => {
                    drop(target_listener);
                    (
                        serve_once(proxy_listener, ok_response("forward-response")),
                        RouteKind::ForwardProxy,
                        b"forward-response".as_slice(),
                    )
                }
                _ => unreachable!(),
            };
            let vault = memory_vault();
            let broker =
                NetworkBroker::new(&vault, Some(ForwardProxy::new(proxy_url).unwrap())).unwrap();
            let mut request = NetworkRequest::get(
                "explicit-route-test",
                target_url.clone(),
                vec![Origin::from_url(&target_url).unwrap()],
            );
            request.proxy_policy = policy;
            request.timeout = Duration::from_secs(2);
            let cancellation = CancellationToken::default();
            if streaming {
                let mut response = broker.open_stream(&request, &cancellation).await.unwrap();
                assert_eq!(response.route, expected_route);
                let mut body = Vec::new();
                while let Some(chunk) = response.next_chunk(&cancellation).await.unwrap() {
                    body.extend_from_slice(&chunk);
                }
                assert_eq!(body, expected_body);
            } else {
                let response = broker.execute(&request, &cancellation).await.unwrap();
                assert_eq!(response.route, expected_route);
                assert_eq!(response.body, expected_body);
            }
            let received = server.await.unwrap();
            match policy {
                ProxyPolicy::Direct => assert!(received.starts_with("GET / HTTP/1.1")),
                ProxyPolicy::ForwardOnly => {
                    assert!(received.starts_with("GET /?url=http%3A%2F%2F"));
                }
                _ => unreachable!(),
            }
        }
    }
}

#[tokio::test]
async fn cancellation_interrupts_an_in_flight_request() {
    let (listener, url) = bind_server().await;
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        accepted_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let vault = memory_vault();
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let request = NetworkRequest::get(
        "cancel-test",
        url.clone(),
        vec![Origin::from_url(&url).unwrap()],
    );
    let cancellation = CancellationToken::default();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move { broker.execute(&request, &task_cancellation).await });
    accepted_rx.await.unwrap();
    cancellation.cancel();

    assert!(matches!(
        task.await.unwrap(),
        Err(BrokerError::Cancelled { .. })
    ));
    server.abort();
}

fn interaction_fixture() -> (
    tempfile::TempDir,
    KernelDatabase,
    JobStore,
    String,
    InteractionContext,
) {
    let sandbox = tempdir().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("data"))
        .unwrap()
        .initialize(RuntimeEnvironment::Test)
        .unwrap();
    let database = KernelDatabase::open(&layout).unwrap();
    let jobs = JobStore::open(database.clone(), "generation-1", 32).unwrap();
    let registration = jobs
        .register_job(RegisterJob {
            command: "source.import".to_owned(),
            owner_scope: "owner:test".to_owned(),
            idempotency_key: "interaction-job".to_owned(),
            request_hash: "sha256:request".to_owned(),
            target: json!({"sourceId": "local"}),
            confirmation_policy: ConfirmationPolicy::Required,
        })
        .unwrap();
    let expires_at_ms = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 60_000;
    jobs.append_event(
        &registration.job.job_id,
        EventUpdate::state("prepare", JobState::Preparing),
    )
    .unwrap();
    (
        sandbox,
        database,
        jobs,
        registration.job.job_id,
        InteractionContext {
            operation_id: "operation:interaction".to_owned(),
            target_revision: 1,
            plan_hash: "sha256:plan".to_owned(),
            module_lock_digest: "sha256:lock".to_owned(),
            expires_at_ms,
        },
    )
}

#[test]
fn file_selection_is_a_durable_one_time_grant_not_an_arbitrary_path_api() {
    let (sandbox, database, jobs, job_id, context) = interaction_fixture();
    let selected = sandbox.path().join("selected.zip");
    std::fs::write(&selected, b"immutable input").unwrap();
    let rejected = sandbox.path().join("rejected.exe");
    std::fs::write(&rejected, b"no").unwrap();
    let (client, host) = open_interaction_broker(database.clone(), jobs.clone()).unwrap();
    let action = client
        .request_file_selection(
            &job_id,
            &context,
            FileSelectionRequest {
                purpose: "Import a mod archive".to_owned(),
                allowed_extensions: vec![".zip".to_owned()],
            },
        )
        .unwrap();
    assert!(!action
        .target
        .to_string()
        .contains(selected.to_str().unwrap()));
    assert!(matches!(
        host.complete_file_selection(
            "owner:test",
            &action.action_id,
            action.revision,
            &rejected,
            "reject-extension",
        ),
        Err(BrokerError::InteractionMismatch(_))
    ));
    host.complete_file_selection(
        "owner:test",
        &action.action_id,
        action.revision,
        &selected,
        "select-file",
    )
    .unwrap();

    drop(client);
    drop(host);
    let (restarted, _) = open_interaction_broker(database, jobs).unwrap();
    let mut file = restarted
        .open_selected_file(&action.action_id, &interaction_binding(&action))
        .unwrap();
    let mut content = String::new();
    file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "immutable input");
    assert!(matches!(
        restarted.open_selected_file(&action.action_id, &interaction_binding(&action)),
        Err(BrokerError::GrantUnavailable(_))
    ));
}

#[test]
fn headless_browser_authorization_resumes_only_with_an_allowed_callback() {
    let (_sandbox, database, jobs, job_id, context) = interaction_fixture();
    let (client, host) = open_interaction_broker(database, jobs).unwrap();
    let authorization_url = Url::parse("https://auth.example/authorize?state=opaque").unwrap();
    let callback_origin = Origin::parse("http://127.0.0.1:43119").unwrap();
    let action = client
        .request_browser_authorization(
            &job_id,
            &context,
            BrowserAuthorizationRequest {
                authorization_url,
                allowed_navigation_origins: vec![Origin::parse("https://auth.example").unwrap()],
                callback_origins: vec![callback_origin],
            },
        )
        .unwrap();
    assert_eq!(action.kind, "browser-authorization");
    assert_eq!(
        host.validate_browser_navigation(
            "owner:test",
            &action.action_id,
            &Url::parse("https://auth.example/login").unwrap(),
        )
        .unwrap(),
        BrowserNavigationDecision::Allow
    );
    assert!(matches!(
        host.validate_browser_navigation(
            "owner:test",
            &action.action_id,
            &Url::parse("https://evil.example/phish").unwrap(),
        ),
        Err(BrokerError::InteractionMismatch(_))
    ));
    assert_eq!(
        host.validate_browser_navigation(
            "owner:test",
            &action.action_id,
            &Url::parse("http://127.0.0.1:43119/callback").unwrap(),
        )
        .unwrap(),
        BrowserNavigationDecision::CompleteCallback
    );
    assert!(matches!(
        host.complete_browser_authorization(
            "owner:test",
            &action.action_id,
            action.revision,
            &Url::parse("https://evil.example/callback?code=stolen").unwrap(),
            "evil-callback",
        ),
        Err(BrokerError::InteractionMismatch(_))
    ));
    host.complete_browser_authorization(
        "owner:test",
        &action.action_id,
        action.revision,
        &Url::parse("http://127.0.0.1:43119/callback?code=good&state=opaque").unwrap(),
        "valid-callback",
    )
    .unwrap();
    let callback = client
        .consume_browser_callback(&action.action_id, &interaction_binding(&action))
        .unwrap();
    assert_eq!(
        callback
            .query_pairs()
            .find(|(key, _)| key == "code")
            .unwrap()
            .1,
        "good"
    );
}

fn serve_once_bytes(listener: TcpListener, response: Vec<u8>) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
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
        socket.write_all(&response).await.unwrap();
        String::from_utf8(request).unwrap()
    })
}

fn gzip_bytes(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

struct BlockingCredentialStore {
    inner: MemoryCredentialStore,
    started: Mutex<Option<oneshot::Sender<()>>>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl CredentialStore for BlockingCredentialStore {
    fn store(&self, secret: &SecretValue) -> Result<CredentialRef, BrokerError> {
        self.inner.store(secret)
    }

    fn resolve(&self, reference: &CredentialRef) -> Result<Option<SecretValue>, BrokerError> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        if let Some(release) = self.release.lock().unwrap().take() {
            let _ = release.recv_timeout(Duration::from_secs(5));
        }
        self.inner.resolve(reference)
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), BrokerError> {
        self.inner.delete(reference)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_credential_resolution_honors_timeout_without_starving_runtime() {
    for streaming in [false, true] {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = Arc::new(BlockingCredentialStore {
            inner: MemoryCredentialStore::default(),
            started: Mutex::new(Some(started_tx)),
            release: Mutex::new(Some(release_rx)),
        });
        let vault = CredentialVault::new(store);
        let credential = vault
            .store(SecretValue::new("blocked-secret").unwrap())
            .unwrap();
        let broker = NetworkBroker::new(&vault, None).unwrap();
        let url = Url::parse("http://127.0.0.1:1/").unwrap();
        let mut request = NetworkRequest::get(
            "blocking-credential-test",
            url.clone(),
            vec![Origin::from_url(&url).unwrap()],
        );
        request.timeout = Duration::from_millis(120);
        request.credential = Some(CredentialBinding {
            reference: credential,
            placement: CredentialPlacement::Bearer,
            allowed_origins: vec![Origin::from_url(&url).unwrap()],
        });

        let broker_task = tokio::spawn(async move {
            let cancellation = CancellationToken::default();
            if streaming {
                broker
                    .open_stream(&request, &cancellation)
                    .await
                    .map(|_| ())
            } else {
                broker.execute(&request, &cancellation).await.map(|_| ())
            }
        });

        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("credential store resolve should have started")
            .unwrap();

        // Concurrent timers/tasks on the current_thread runtime must stay responsive.
        let heartbeat = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            42_u8
        });
        assert_eq!(heartbeat.await.unwrap(), 42);

        let result = broker_task.await.unwrap();
        assert!(
            matches!(result, Err(BrokerError::Timeout { .. })),
            "expected Timeout while credential resolution is blocked, got {result:?}"
        );
        let _ = release_tx.send(());
    }
}

#[tokio::test]
async fn execute_requests_and_decodes_gzip_responses_and_leaves_open_stream_raw() {
    // Single-member and multi-member gzip responses.
    let part1 = br#"{"items":["alpha","beta"],"#;
    let part2 = br#""total":2}"#;
    let mut multi_member = gzip_bytes(part1);
    multi_member.extend_from_slice(&gzip_bytes(part2));

    for (payload, expected) in [
        (gzip_bytes(br#"{"ok":true}"#), br#"{"ok":true}"#.to_vec()),
        (multi_member, [part1.as_slice(), part2.as_slice()].concat()),
    ] {
        let (listener, url) = bind_server().await;
        let mut raw_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        )
        .into_bytes();
        raw_response.extend_from_slice(&payload);
        let server = serve_once_bytes(listener, raw_response);

        let vault = memory_vault();
        let broker = NetworkBroker::new(&vault, None).unwrap();
        let request = NetworkRequest::get(
            "gzip-execute-test",
            url.clone(),
            vec![Origin::from_url(&url).unwrap()],
        );
        let response = broker
            .execute(&request, &CancellationToken::default())
            .await
            .unwrap();
        assert_eq!(response.body, expected);
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert!(
            !response.headers.contains_key("content-length"),
            "compressed Content-Length must be removed after gzip decoding"
        );
        let received = server.await.unwrap().to_ascii_lowercase();
        assert!(
            received.contains("accept-encoding: gzip"),
            "expected Accept-Encoding: gzip header in request:\n{received}"
        );
    }

    // open_stream must NOT request gzip so binary Range/Content-Length downloads remain intact.
    let (stream_listener, stream_url) = bind_server().await;
    let stream_server = serve_once(stream_listener, ok_response("raw-binary"));
    let vault = memory_vault();
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let stream_request = NetworkRequest::get(
        "stream-no-gzip-test",
        stream_url.clone(),
        vec![Origin::from_url(&stream_url).unwrap()],
    );
    let cancellation = CancellationToken::default();
    let mut stream_response = broker
        .open_stream(&stream_request, &cancellation)
        .await
        .unwrap();
    assert_eq!(
        stream_response
            .headers
            .get("content-length")
            .map(String::as_str),
        Some("10")
    );
    let mut stream_body = Vec::new();
    while let Some(chunk) = stream_response.next_chunk(&cancellation).await.unwrap() {
        stream_body.extend_from_slice(&chunk);
    }
    assert_eq!(stream_body, b"raw-binary");
    let received_stream = stream_server.await.unwrap().to_ascii_lowercase();
    assert!(!received_stream.contains("accept-encoding"));
}

#[tokio::test]
async fn execute_enforces_gzip_bomb_limit_empty_body_and_encoding_validation() {
    let vault = memory_vault();
    let broker = NetworkBroker::new(&vault, None).unwrap();

    // 1. Gzip bomb: small compressed payload expanding beyond max_response_bytes.
    let bomb = gzip_bytes(&vec![0_u8; 64 * 1024]);
    assert!(bomb.len() < 256);
    let (listener, url) = bind_server().await;
    let mut raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bomb.len()
    )
    .into_bytes();
    raw.extend_from_slice(&bomb);
    let server = serve_once_bytes(listener, raw);
    let mut request = NetworkRequest::get(
        "gzip-bomb-test",
        url.clone(),
        vec![Origin::from_url(&url).unwrap()],
    );
    request.max_response_bytes = 256;
    let err = broker
        .execute(&request, &CancellationToken::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, BrokerError::ResponseTooLarge { .. }),
        "expected ResponseTooLarge for gzip bomb, got {err:?}"
    );
    let _ = server.await;

    // 2. Empty body with Content-Encoding: gzip (e.g. 204 No Content).
    let (listener, url) = bind_server().await;
    let server = serve_once(
        listener,
        "HTTP/1.1 204 No Content\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_owned(),
    );
    let request = NetworkRequest::get(
        "gzip-empty-test",
        url.clone(),
        vec![Origin::from_url(&url).unwrap()],
    );
    let response = broker
        .execute(&request, &CancellationToken::default())
        .await
        .unwrap();
    assert!(response.body.is_empty());
    assert!(!response.headers.contains_key("content-length"));
    server.await.unwrap();

    // 3. Stacked gzip, unsupported encoding, and corrupt gzip bytes are rejected as Transport.
    for (encoding_header, body) in [
        ("gzip, gzip", gzip_bytes(&gzip_bytes(b"nested"))),
        ("br", b"brotli-bytes".to_vec()),
        ("gzip", b"not-a-valid-gzip-stream".to_vec()),
    ] {
        let (listener, url) = bind_server().await;
        let mut raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: {encoding_header}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        raw.extend_from_slice(&body);
        let server = serve_once_bytes(listener, raw);
        let request = NetworkRequest::get(
            "gzip-invalid-test",
            url.clone(),
            vec![Origin::from_url(&url).unwrap()],
        );
        let err = broker
            .execute(&request, &CancellationToken::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, BrokerError::Transport { .. }),
            "expected Transport for Content-Encoding '{encoding_header}', got {err:?}"
        );
        let _ = server.await;
    }
}
