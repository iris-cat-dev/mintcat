use std::{sync::Arc, time::Duration};

use mintcat_runtime_broker::{
    CancellationToken, CredentialVault, MemoryCredentialStore, NetworkBroker, Origin, ProxyPolicy,
    SecretValue,
};
use mintcat_sdk::ModRef;
use mintcat_source_api::{
    ReviewStatus, SearchRequest, SearchSortField, SortDirection, SourceError, SourceGameBinding,
    SourceProvider,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::mpsc,
};
use url::Url;

use crate::{ModioConfig, ModioProvider, SOURCE_ID};

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
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            sender.send(request.clone()).unwrap();
            let (status, body) = handler(&request);
            let body = serde_json::to_vec(&body).unwrap();
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let header = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    TestServer {
        origin,
        requests,
        task,
    }
}

fn game() -> SourceGameBinding {
    SourceGameBinding {
        game_id: "game:drg".to_owned(),
        remote_game_key: "drg".to_owned(),
        provider_game_id: "2475".to_owned(),
    }
}

fn api_mod(id: u64, name: &str, status: i64) -> Value {
    json!({
        "id": id,
        "game_id": 2475,
        "status": status,
        "visible": 1,
        "date_added": 100,
        "date_updated": 200,
        "maturity_option": 2,
        "name": name,
        "name_id": name.to_ascii_lowercase().replace(' ', "-"),
        "summary": format!("{name} summary"),
        "description_plaintext": format!("{name} description"),
        "profile_url": format!("https://mod.io/g/drg/m/{}", name.to_ascii_lowercase()),
        "submitted_by": { "username": "Author" },
        "logo": { "thumb_640x360": "https://images.example/logo.png" },
        "tags": [{"name": "Approved"}, {"name": "approved"}, {"name": "Utility"}],
        "stats": {
            "downloads_total": 44,
            "subscribers_total": 12,
            "ratings_weighted_aggregate": 4.5
        },
        "modfile": api_file(id, 99, "2.0", "https://downloads.example/latest.zip")
    })
}

fn api_file(mod_id: u64, id: u64, version: &str, download_url: &str) -> Value {
    json!({
        "id": id,
        "mod_id": mod_id,
        "date_added": 123,
        "virus_status": 1,
        "virus_positive": 0,
        "filesize": 1024,
        "filename": format!("{id}.zip"),
        "version": version,
        "filehash": {"md5": format!("md5-{id}")},
        "download": {"binary_url": download_url, "date_expires": 9999999999_i64}
    })
}

fn provider(origin: &Url, download_origins: Vec<Origin>) -> ModioProvider {
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let broker = NetworkBroker::new(&vault, None).unwrap();
    ModioProvider::new(
        broker,
        ModioConfig {
            api_base: origin.join("v1/").unwrap(),
            credential: None,
            proxy_policy: ProxyPolicy::Direct,
            download_origins,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn empty_download_scope_allows_read_operations_but_never_authorizes_binary_downloads() {
    let mut server = spawn_server(|request| {
        if request.contains("/files") {
            (200, json!({"data": [api_file(7, 99, "2.0", "https://downloads.example/file.zip")]}))
        } else if request.starts_with("GET /v1/games/2475/mods/7 ") {
            (200, api_mod(7, "Available", 1))
        } else {
            (200, json!({"data": [api_mod(7, "Available", 1)], "result_total": 1}))
        }
    }).await;
    let source = provider(&server.origin, Vec::new());
    let cancellation = CancellationToken::default();
    let page = source.search(&game(), &SearchRequest {
        query: String::new(),
        cursor: None,
        page_size: 20,
        sort_field: None,
        sort_direction: None,
    }, &cancellation).await.unwrap();
    assert_eq!(page.items.len(), 1);
    let reference = &page.items[0].mod_ref;
    let details = source.get_mod(&game(), reference, &cancellation).await.unwrap();
    assert_eq!(details.mod_ref.remote_mod_id, "7");
    let versions = source.list_versions(&game(), reference, &cancellation).await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].remote_file_id, "99");
    let version_reference = ModRef {
        version_id: Some(versions[0].version_id.clone()),
        remote_file_id: Some(versions[0].remote_file_id.clone()),
        ..reference.clone()
    };
    let approved = provider(&server.origin, vec![Origin::parse("https://downloads.example").unwrap()]);
    let descriptor = approved.resolve_download(&game(), &version_reference, &cancellation).await.unwrap();
    assert_eq!(descriptor.mod_ref.remote_file_id.as_deref(), Some("99"));
    let denied = source.resolve_download(&game(), &version_reference, &cancellation).await;
    assert!(matches!(denied, Err(SourceError::InvalidResponse(_))), "{denied:?}");
    for expected_path in [
        "GET /v1/games/2475/mods?",
        "GET /v1/games/2475/mods/7 ",
        "GET /v1/games/2475/mods/7/files?",
        "GET /v1/games/2475/mods/7/files?",
        "GET /v1/games/2475/mods/7/files?",
    ] {
        let captured = server.requests.recv().await.unwrap();
        assert!(captured.starts_with(expected_path), "{captured}");
        assert!(!captured.to_ascii_lowercase().contains("authorization:"));
    }
    assert!(server.requests.try_recv().is_err());
}

#[tokio::test]
async fn missing_credentials_with_empty_download_scope_remain_unauthorized() {
    let mut server = spawn_server(|_| (401, json!({"error": "authentication required"}))).await;
    let source = provider(&server.origin, Vec::new());
    assert!(matches!(
        source.search(&game(), &SearchRequest {
            query: String::new(),
            cursor: None,
            page_size: 20,
            sort_field: None,
            sort_direction: None,
        }, &CancellationToken::default()).await,
        Err(SourceError::Unauthorized)
    ));
    let captured = server.requests.recv().await.unwrap();
    assert!(!captured.to_ascii_lowercase().contains("authorization:"));
}

#[tokio::test]
async fn search_is_game_bound_paginated_sorted_and_normalized() {
    let mut server = spawn_server(|request| {
        if request.contains("_offset=1") {
            (
                200,
                json!({"data": [api_mod(2, "Second", 1)], "result_total": 2}),
            )
        } else {
            (
                200,
                json!({"data": [api_mod(1, "First", 1)], "result_total": 2}),
            )
        }
    })
    .await;
    let source = provider(
        &server.origin,
        vec![Origin::parse("https://downloads.example").unwrap()],
    );
    let request = SearchRequest {
        query: "utility".to_owned(),
        cursor: None,
        page_size: 1,
        sort_field: Some(SearchSortField::Downloads),
        sort_direction: Some(SortDirection::Descending),
    };
    let first = source
        .search(&game(), &request, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(first.total, Some(2));
    assert!(first.live_pagination);
    assert_eq!(first.items[0].mod_ref.remote_mod_id, "1");
    assert_eq!(first.items[0].tags, vec!["Approved", "Utility"]);
    assert_eq!(first.items[0].maturity_flags, 2);
    assert_eq!(first.items[0].review_status, ReviewStatus::Accepted);
    let first_request = server.requests.recv().await.unwrap();
    assert!(
        first_request.contains("name-lk=*utility*")
            || first_request.contains("name-lk=%2Autility%2A")
    );
    assert!(first_request.contains("_sort=-downloads_total"));

    let second = source
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
    assert_eq!(second.items[0].mod_ref.remote_mod_id, "2");
    assert!(second.next_cursor.is_none());
    assert!(matches!(
        source
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
async fn exact_old_file_resolution_never_falls_back_to_latest() {
    let server = spawn_server(|request| {
        if request.contains("/files") {
            (
                200,
                json!({"data": [
                    api_file(7, 10, "1.0", "http://127.0.0.1:9/old.zip"),
                    api_file(7, 11, "2.0", "http://127.0.0.1:9/latest.zip")
                ]}),
            )
        } else if request.contains("/dependencies") {
            (200, json!({"data": [api_mod(8, "Dependency", 1)]}))
        } else {
            (200, api_mod(7, "Main", 1))
        }
    })
    .await;
    let download_origin = Origin::parse("http://127.0.0.1:9").unwrap();
    let source = provider(&server.origin, vec![download_origin]);
    let old = ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: "drg".to_owned(),
        remote_mod_id: "7".to_owned(),
        version_id: Some("file:10".to_owned()),
        remote_file_id: None,
    };
    let descriptor = source
        .resolve_download(&game(), &old, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(descriptor.mod_ref.remote_file_id.as_deref(), Some("10"));
    assert_eq!(descriptor.mod_ref.version_id.as_deref(), Some("file:10"));
    assert!(descriptor.url.path().ends_with("old.zip"));
    assert_eq!(descriptor.expected_md5.as_deref(), Some("md5-10"));

    let versions = source
        .list_versions(&game(), &old, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[1].version_id, "file:11");
    let dependencies = source
        .get_dependencies(&game(), &old, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(dependencies[0].mod_ref.remote_mod_id, "8");
}

#[tokio::test]
async fn unavailable_mod_and_untrusted_download_are_structured_failures() {
    let server = spawn_server(|request| {
        if request.contains("/files") {
            (
                200,
                json!({"data": [api_file(7, 10, "1.0", "https://evil.example/file.zip")]}),
            )
        } else {
            (200, api_mod(7, "Pending", 0))
        }
    })
    .await;
    let source = provider(
        &server.origin,
        vec![Origin::parse("https://downloads.example").unwrap()],
    );
    let mod_ref = ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: "drg".to_owned(),
        remote_mod_id: "7".to_owned(),
        version_id: Some("file:10".to_owned()),
        remote_file_id: None,
    };
    assert!(matches!(
        source
            .get_mod(&game(), &mod_ref, &CancellationToken::default())
            .await,
        Err(SourceError::Unavailable(_))
    ));
    assert!(matches!(
        source
            .resolve_download(&game(), &mod_ref, &CancellationToken::default())
            .await,
        Err(SourceError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn expired_credentials_remain_unauthorized_and_links_are_game_scoped() {
    let mut server = spawn_server(|_| (401, json!({"error": "expired"}))).await;
    let store = Arc::new(MemoryCredentialStore::default());
    let vault = CredentialVault::new(store);
    let credential = vault
        .store(SecretValue::new("expired-token").unwrap())
        .unwrap();
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let source = ModioProvider::new(
        broker,
        ModioConfig {
            api_base: server.origin.join("v1/").unwrap(),
            credential: Some(credential),
            proxy_policy: ProxyPolicy::Direct,
            download_origins: vec![Origin::from_url(&server.origin).unwrap()],
        },
    )
    .unwrap();
    assert!(matches!(
        source
            .search(
                &game(),
                &SearchRequest {
                    query: String::new(),
                    cursor: None,
                    page_size: 20,
                    sort_field: None,
                    sort_direction: None,
                },
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::Unauthorized)
    ));
    let captured = server.requests.recv().await.unwrap();
    assert!(captured
        .to_ascii_lowercase()
        .contains("authorization: bearer expired-token"));

    let parsed = source
        .parse_url(
            &game(),
            &Url::parse("https://mod.io/g/drg/m/verified-link").unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(parsed.remote_mod_id, "verified-link");
    assert!(source
        .parse_url(
            &game(),
            &Url::parse("https://mod.io/g/rogue-core/m/wrong-game").unwrap(),
        )
        .unwrap()
        .is_none());

    tokio::time::sleep(Duration::from_millis(1)).await;
}

#[tokio::test]
async fn metadata_and_files_cannot_cross_the_requested_mod_identity() {
    for case in ["metadata", "latest-file", "version-list"] {
        let server = spawn_server(move |_| {
            let body = match case {
                "metadata" => api_mod(2, "Wrong identity", 1),
                "latest-file" => {
                    let mut item=api_mod(1,"Correct metadata",1);
                    item["modfile"]=api_file(2,99,"2.0","https://downloads.example/latest.zip");
                    item
                },
                _ => json!({"data":[api_file(2,99,"2.0","https://downloads.example/latest.zip")]}),
            };
            (200,body)
        }).await;
        let source=provider(&server.origin,vec![Origin::parse("https://downloads.example").unwrap()]);
        let reference=ModRef {source_id:SOURCE_ID.into(),remote_game_key:"drg".into(),remote_mod_id:"1".into(),version_id:None,remote_file_id:None};
        let result=if case=="version-list" {
            source.list_versions(&game(),&reference,&CancellationToken::default()).await.map(|_|())
        } else {
            source.get_mod(&game(),&reference,&CancellationToken::default()).await.map(|_|())
        };
        assert!(matches!(result,Err(SourceError::InvalidResponse(_))),"{case}");
    }
}
