use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_runtime_broker::{
    open_interaction_broker, BrokerError, BrowserNavigationDecision, CancellationToken,
    CredentialVault, InteractionContext, InteractionHost, MemoryCredentialStore, NetworkBroker,
    Origin, ProxyPolicy, SecretValue,
};
use mintcat_runtime_jobs::{ConfirmationPolicy, EventUpdate, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{ActionState, JobState, ModRef};
use mintcat_source_api::{
    ReviewStatus, SearchRequest, SearchSortField, SortDirection, SourceError, SourceGameBinding,
    SourceProvider,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::mpsc,
};
use url::Url;

use crate::{NexusConfig, NexusDownloadOutcome, NexusProvider, SOURCE_ID};

struct TestServer {
    origin: Url,
    requests: mpsc::UnboundedReceiver<String>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_server(
    handler: impl Fn(&str) -> (u16, Value) + Send + Sync + 'static,
) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let handler = Arc::new(handler);
    let (sender, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 2048];
            let header_end = loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break bytes.len();
                }
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                })
                .unwrap_or(0);
            while bytes.len() < header_end + content_length {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8(bytes).unwrap();
            sender.send(request.clone()).unwrap();
            let (status, body) = handler(&request);
            let body = serde_json::to_vec(&body).unwrap();
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    TestServer {
        origin,
        requests,
        task,
    }
}

struct ProviderFixture {
    _sandbox: TempDir,
    provider: NexusProvider,
    host: InteractionHost,
    jobs: JobStore,
}

impl ProviderFixture {
    fn register_interaction(
        &self,
        suffix: &str,
        expires_in_ms: i64,
    ) -> (String, InteractionContext) {
        let registration = self
            .jobs
            .register_job(RegisterJob {
                command: "source.download".to_owned(),
                owner_scope: "owner:test".to_owned(),
                idempotency_key: format!("nexus-job-{suffix}"),
                request_hash: format!("sha256:request-{suffix}"),
                target: json!({"sourceId": SOURCE_ID, "suffix": suffix}),
                confirmation_policy: ConfirmationPolicy::Required,
            })
            .unwrap();
        self.jobs
            .append_event(
                &registration.job.job_id,
                EventUpdate::state("prepare", JobState::Preparing),
            )
            .unwrap();
        let expires_at_ms = now_ms() + expires_in_ms;
        (
            registration.job.job_id,
            InteractionContext {
                operation_id: format!("operation:{suffix}"),
                target_revision: 1,
                plan_hash: format!("sha256:plan-{suffix}"),
                module_lock_digest: "sha256:lock".to_owned(),
                expires_at_ms,
            },
        )
    }
}

fn fixture(origin: &Url, download_origins: Vec<Origin>, with_credential: bool) -> ProviderFixture {
    let sandbox = tempfile::tempdir().unwrap();
    let layout = RuntimePaths::new(sandbox.path().join("data"))
        .unwrap()
        .initialize(RuntimeEnvironment::Test)
        .unwrap();
    let database = KernelDatabase::open(&layout).unwrap();
    let jobs = JobStore::open(database.clone(), "generation-test", 64).unwrap();
    let (interactions, host) = open_interaction_broker(database, jobs.clone()).unwrap();
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let credential = with_credential.then(|| {
        vault
            .store(SecretValue::new(b"unit-test-key".to_vec()).unwrap())
            .unwrap()
    });
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let website_origin = Origin::from_url(origin).unwrap();
    let provider = NexusProvider::new(
        broker,
        interactions,
        NexusConfig {
            api_base: origin.clone(),
            website_base: origin.clone(),
            credential,
            proxy_policy: ProxyPolicy::Direct,
            navigation_origins: vec![website_origin],
            download_origins,
            application_name: "MintCat Tests".to_owned(),
            application_version: "0.1.0".to_owned(),
        },
    )
    .unwrap();
    ProviderFixture {
        _sandbox: sandbox,
        provider,
        host,
        jobs,
    }
}

fn game() -> SourceGameBinding {
    SourceGameBinding {
        game_id: "game:drg".to_owned(),
        remote_game_key: "drg".to_owned(),
        provider_game_id: "deeprockgalactic".to_owned(),
    }
}

fn mod_ref(file_id: Option<&str>) -> ModRef {
    ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: "drg".to_owned(),
        remote_mod_id: "7".to_owned(),
        version_id: file_id.map(|id| format!("file:{id}")),
        remote_file_id: file_id.map(str::to_owned),
    }
}

fn api_file(id: u64, version: &str, category: &str, primary: bool, uploaded: i64) -> Value {
    json!({
        "file_id": id,
        "name": format!("File {id}"),
        "version": version,
        "file_name": format!("file-{id}.zip"),
        "category_id": if category == "MAIN" { 1 } else { 4 },
        "category_name": category,
        "is_primary": primary,
        "uploaded_timestamp": uploaded,
        "size": 512
    })
}

fn files_response() -> Value {
    json!({"files": [
        api_file(10, "1.0", "OLD_VERSION", false, 100),
        api_file(11, "2.0", "MAIN", true, 200)
    ]})
}

fn graphql_node(id: u64, name: &str) -> Value {
    json!({
        "modId": id,
        "name": name,
        "summary": format!("{name} summary"),
        "description": format!("{name} description"),
        "author": "Author",
        "downloads": 50,
        "endorsements": 9,
        "pictureUrl": "https://images.example/mod.png",
        "createdAt": "2024-01-01T00:00:00Z",
        "updatedAt": "2024-02-01T00:00:00Z",
        "game": {"domainName": "deeprockgalactic"},
        "uploader": {"name": "Uploader"}
    })
}

#[tokio::test]
async fn empty_download_scope_allows_read_operations_but_never_authorizes_binary_downloads() {
    let mut server = spawn_server(|request| {
        if request.contains("/v2/graphql") {
            (200, json!({"data":{"mods":{"nodes":[graphql_node(7,"Available")],"totalCount":1,"nodesCount":1}}}))
        } else if request.contains("/files.json") {
            (200, files_response())
        } else if request.contains("/download_link.json") {
            (200, json!([{"URI":"https://downloads.example/file.zip"}]))
        } else {
            (200, json!({
                "mod_id":7,"name":"Available","summary":"summary","description":"description",
                "mod_downloads":55,"endorsement_count":4,"version":"2.0","author":"Author",
                "updated_time":200,"created_time":100,"domain_name":"deeprockgalactic"
            }))
        }
    }).await;
    let fixture = fixture(&server.origin, Vec::new(), true);
    let cancellation = CancellationToken::default();
    let page = fixture.provider.search(&game(), &SearchRequest {
        query:String::new(),cursor:None,page_size:20,sort_field:None,sort_direction:None,
    }, &cancellation).await.unwrap();
    assert_eq!(page.items[0].mod_ref.remote_mod_id,"7");
    let detail = fixture.provider.get_mod(&game(), &mod_ref(None), &cancellation).await.unwrap();
    assert_eq!(detail.mod_ref.remote_mod_id,"7");
    let versions = fixture.provider.list_versions(&game(), &mod_ref(None), &cancellation).await.unwrap();
    assert_eq!(versions.len(),2);
    assert!(matches!(
        fixture.provider.resolve_download(&game(), &mod_ref(Some("10")), &cancellation).await,
        Err(SourceError::InvalidResponse(message))
            if message == "Nexus returned a download URL outside the configured origins"
    ));
    for _ in 0..6 {
        let captured = server.requests.recv().await.unwrap();
        assert!(!captured.starts_with("GET /file.zip "));
    }
    assert!(server.requests.try_recv().is_err());
}

#[tokio::test]
async fn search_is_game_bound_live_paginated_and_reports_partial_graphql_failures() {
    let mut server = spawn_server(|request| {
        let second = request.contains("\"offset\":1");
        let node = if second {
            graphql_node(8, "Second")
        } else {
            graphql_node(7, "First")
        };
        (
            200,
            json!({
                "data": {"mods": {"nodes": [node], "totalCount": 2, "nodesCount": 1}},
                "errors": [{"message": "secondary index timed out"}]
            }),
        )
    })
    .await;
    let fixture = fixture(
        &server.origin,
        vec![Origin::parse("http://127.0.0.1:9").unwrap()],
        true,
    );
    let request = SearchRequest {
        query: "utility".to_owned(),
        cursor: None,
        page_size: 1,
        sort_field: Some(SearchSortField::Downloads),
        sort_direction: Some(SortDirection::Descending),
    };
    let first = fixture
        .provider
        .search(&game(), &request, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(first.items[0].mod_ref.remote_mod_id, "7");
    assert_eq!(first.total, Some(2));
    assert_eq!(first.warnings.len(), 1);
    assert_eq!(first.warnings[0].code, "NEXUS_GRAPHQL_PARTIAL");
    assert!(first.live_pagination);
    let first_request = server.requests.recv().await.unwrap();
    assert!(first_request
        .to_ascii_lowercase()
        .contains("apikey: unit-test-key"));
    assert!(first_request.contains("\"gameDomainName\""));
    assert!(first_request.contains("\"value\":\"deeprockgalactic\""));
    assert!(first_request.contains("\"downloads\":{\"direction\":\"DESC\"}"));

    let second = fixture
        .provider
        .search(
            &game(),
            &SearchRequest {
                cursor: first.next_cursor.clone(),
                ..request.clone()
            },
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(second.items[0].mod_ref.remote_mod_id, "8");
    assert!(second.next_cursor.is_none());
    assert!(matches!(
        fixture
            .provider
            .search(
                &game(),
                &SearchRequest {
                    query: "different".to_owned(),
                    cursor: first.next_cursor,
                    ..request
                },
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::InvalidArgument(_))
    ));
}

#[tokio::test]
async fn detail_urls_and_downloads_preserve_exact_file_identity() {
    let server = spawn_server(|request| {
        if request.contains("/files.json") {
            (200, files_response())
        } else if request.contains("/download_link.json") {
            (
                200,
                json!([{"URI": "http://127.0.0.1:9/downloads/old.zip"}]),
            )
        } else {
            (
                200,
                json!({
                    "mod_id": 7,
                    "name": "Exact Mod",
                    "summary": "summary",
                    "description": "description",
                    "mod_downloads": 55,
                    "endorsement_count": 4,
                    "version": "2.0",
                    "author": "Author",
                    "updated_time": 200,
                    "created_time": 100,
                    "domain_name": "deeprockgalactic"
                }),
            )
        }
    })
    .await;
    let download_origin = Origin::parse("http://127.0.0.1:9").unwrap();
    let fixture = fixture(&server.origin, vec![download_origin.clone()], true);
    let website = server
        .origin
        .join("deeprockgalactic/mods/7/files/10")
        .unwrap();
    let parsed = fixture
        .provider
        .parse_url(&game(), &website)
        .unwrap()
        .unwrap();
    assert_eq!(parsed.remote_file_id.as_deref(), Some("10"));
    let nxm = Url::parse(
        "nxm://deeprockgalactic/mods/7/files/10?key=opaque&expires=4102444800&user_id=1",
    )
    .unwrap();
    assert_eq!(
        fixture
            .provider
            .parse_url(&game(), &nxm)
            .unwrap()
            .unwrap()
            .version_id
            .as_deref(),
        Some("file:10")
    );

    let detail = fixture
        .provider
        .get_mod(&game(), &mod_ref(None), &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(detail.latest_file.unwrap().remote_file_id, "11");
    let versions = fixture
        .provider
        .list_versions(&game(), &mod_ref(None), &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(versions[0].version_id, "file:10");
    assert_eq!(versions[0].review_status, ReviewStatus::Accepted);
    assert_eq!(versions[0].file_size, 512 * 1024);

    let descriptor = fixture
        .provider
        .resolve_download(&game(), &mod_ref(Some("10")), &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(descriptor.mod_ref.remote_file_id.as_deref(), Some("10"));
    assert_eq!(descriptor.mod_ref.version_id.as_deref(), Some("file:10"));
    assert_eq!(descriptor.url.path(), "/downloads/old.zip");
    assert_eq!(descriptor.allowed_origins, vec![download_origin]);
    assert!(matches!(
        fixture
            .provider
            .resolve_download(
                &game(),
                &mod_ref(Some("999")),
                &CancellationToken::default()
            )
            .await,
        Err(SourceError::NotFound(value)) if value == "999"
    ));
}

#[tokio::test]
async fn paid_download_requires_a_constrained_headless_action_then_resumes_once() {
    let mut server = spawn_server(|request| {
        if request.contains("/files.json") {
            (
                200,
                json!({"files": [api_file(10, "1.0", "MAIN", true, 100)]}),
            )
        } else if request.contains("/download_link.json") && request.contains("key=opaque-key") {
            (
                200,
                json!([{"URI": "http://127.0.0.1:9/downloads/authorized.zip"}]),
            )
        } else if request.contains("/download_link.json") {
            (
                403,
                json!({"message": "premium or browser authorization required"}),
            )
        } else {
            (404, json!({"message": "not found"}))
        }
    })
    .await;
    let fixture = fixture(
        &server.origin,
        vec![Origin::parse("http://127.0.0.1:9").unwrap()],
        true,
    );
    let (job_id, context) = fixture.register_interaction("resume", 60_000);
    let outcome = fixture
        .provider
        .resolve_download_or_request_interaction(
            &job_id,
            &context,
            &game(),
            &mod_ref(Some("10")),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    let action = match outcome {
        NexusDownloadOutcome::InteractionRequired(action) => action,
        NexusDownloadOutcome::Ready(_) => {
            panic!("paid Nexus download bypassed browser interaction")
        }
    };
    assert_eq!(action.kind, "browser-download");
    assert!(action
        .target
        .get("authorizationUrl")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("file_id=10"));
    assert_eq!(
        fixture
            .host
            .validate_browser_navigation(
                "owner:test",
                &action.action_id,
                &server.origin.join("deeprockgalactic/mods/7").unwrap(),
            )
            .unwrap(),
        BrowserNavigationDecision::Allow
    );
    assert!(fixture
        .host
        .validate_browser_navigation(
            "owner:test",
            &action.action_id,
            &Url::parse("https://evil.example/phish").unwrap(),
        )
        .is_err());
    assert!(matches!(
        fixture.host.complete_browser_download(
            "owner:test",
            &action.action_id,
            action.revision,
            &Url::parse("https://evil.example/download.zip").unwrap(),
            "reject-out-of-scope",
        ),
        Err(BrokerError::InteractionMismatch(_))
    ));
    assert_eq!(
        fixture
            .jobs
            .get_action("owner:test", &action.action_id)
            .unwrap()
            .state,
        ActionState::Pending
    );
    let callback = Url::parse(
        "nxm://deeprockgalactic/mods/7/files/10?key=opaque-key&expires=4102444800&user_id=1",
    )
    .unwrap();
    assert_eq!(
        fixture
            .host
            .validate_browser_navigation("owner:test", &action.action_id, &callback)
            .unwrap(),
        BrowserNavigationDecision::CompleteCallback
    );
    fixture
        .host
        .complete_browser_download(
            "owner:test",
            &action.action_id,
            action.revision,
            &callback,
            "complete-resume",
        )
        .unwrap();
    let descriptor = fixture
        .provider
        .resume_download(
            &action,
            &game(),
            &mod_ref(Some("10")),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(descriptor.url.path(), "/downloads/authorized.zip");
    assert_eq!(descriptor.expires_at, Some(4_102_444_800));
    assert_eq!(descriptor.mod_ref.remote_file_id.as_deref(), Some("10"));
    assert_eq!(descriptor.mod_ref.remote_mod_id, "7");
    assert_eq!(descriptor.mod_ref.version_id.as_deref(), Some("file:10"));
    assert_eq!(
        fixture
            .jobs
            .get_action("owner:test", &action.action_id)
            .unwrap()
            .state,
        ActionState::Consumed
    );

    // The server records each request before responding, so all requests made by the
    // completed continuation are already available without waiting for another one.
    let mut requests = Vec::new();
    while let Ok(request) = server.requests.try_recv() {
        requests.push(request);
    }
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("key=opaque-key&expires=4102444800"))
            .count(),
        1
    );
    assert!(matches!(
        fixture
            .provider
            .resume_download(
                &action,
                &game(),
                &mod_ref(Some("10")),
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::Network(BrokerError::GrantUnavailable(_)))
    ));
    assert!(matches!(
        server.requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn wrong_callback_rejection_close_and_expiry_fail_structurally() {
    let server = spawn_server(|request| {
        if request.contains("/files.json") {
            (
                200,
                json!({"files": [api_file(10, "1.0", "MAIN", true, 100)]}),
            )
        } else if request.contains("/download_link.json") {
            (403, json!({"message": "browser required"}))
        } else {
            (404, json!({"message": "not found"}))
        }
    })
    .await;
    let fixture = fixture(
        &server.origin,
        vec![Origin::parse("http://127.0.0.1:9").unwrap()],
        true,
    );

    let (wrong_job, wrong_context) = fixture.register_interaction("wrong-file", 60_000);
    let wrong_action = required_action(
        fixture
            .provider
            .resolve_download_or_request_interaction(
                &wrong_job,
                &wrong_context,
                &game(),
                &mod_ref(Some("10")),
                &CancellationToken::default(),
            )
            .await
            .unwrap(),
    );
    let wrong_callback =
        Url::parse("nxm://deeprockgalactic/mods/7/files/11?key=wrong&expires=4102444800").unwrap();
    fixture
        .host
        .complete_browser_download(
            "owner:test",
            &wrong_action.action_id,
            wrong_action.revision,
            &wrong_callback,
            "complete-wrong",
        )
        .unwrap();
    assert!(matches!(
        fixture
            .provider
            .resume_download(
                &wrong_action,
                &game(),
                &mod_ref(Some("10")),
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::InvalidArgument(message)) if message.contains("does not match")
    ));

    let (closed_job, closed_context) = fixture.register_interaction("closed", 60_000);
    let closed_action = required_action(
        fixture
            .provider
            .resolve_download_or_request_interaction(
                &closed_job,
                &closed_context,
                &game(),
                &mod_ref(Some("10")),
                &CancellationToken::default(),
            )
            .await
            .unwrap(),
    );
    fixture
        .host
        .reject_interaction(
            "owner:test",
            &closed_action.action_id,
            closed_action.revision,
            "window-closed",
            "reject-closed",
        )
        .unwrap();
    let rejected = fixture
        .jobs
        .get_action("owner:test", &closed_action.action_id)
        .unwrap();
    assert_eq!(rejected.state, ActionState::Rejected);
    let failed = fixture.jobs.get_job("owner:test", &closed_job).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.error.unwrap().code, "INTERACTION_REJECTED");
    assert!(fixture
        .provider
        .resume_download(
            &closed_action,
            &game(),
            &mod_ref(Some("10")),
            &CancellationToken::default(),
        )
        .await
        .is_err());

    let (expired_job, expired_context) = fixture.register_interaction("expired-key", 60_000);
    let expired_action = required_action(
        fixture
            .provider
            .resolve_download_or_request_interaction(
                &expired_job,
                &expired_context,
                &game(),
                &mod_ref(Some("10")),
                &CancellationToken::default(),
            )
            .await
            .unwrap(),
    );
    let expired_callback =
        Url::parse("nxm://deeprockgalactic/mods/7/files/10?key=expired&expires=1").unwrap();
    fixture
        .host
        .complete_browser_download(
            "owner:test",
            &expired_action.action_id,
            expired_action.revision,
            &expired_callback,
            "complete-expired",
        )
        .unwrap();
    assert!(matches!(
        fixture
            .provider
            .resume_download(
                &expired_action,
                &game(),
                &mod_ref(Some("10")),
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::InteractionRequired { kind, .. }) if kind == "browser-download"
    ));
}

#[tokio::test]
async fn api_key_is_required_and_never_replaced_by_a_frontend_secret() {
    let server = spawn_server(|_| (200, files_response())).await;
    let fixture = fixture(
        &server.origin,
        Vec::new(),
        false,
    );
    assert!(matches!(
        fixture
            .provider
            .list_versions(&game(), &mod_ref(None), &CancellationToken::default())
            .await,
        Err(SourceError::Unauthorized)
    ));
}

fn required_action(outcome: NexusDownloadOutcome) -> mintcat_sdk::ActionRequired {
    match outcome {
        NexusDownloadOutcome::InteractionRequired(action) => action,
        NexusDownloadOutcome::Ready(_) => panic!("expected a controlled browser action"),
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
