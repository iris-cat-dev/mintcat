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
            (403, json!({"error": {"code": 403, "message": "Your personal access token does not grant access to this resource"}}))
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
        "GET /v1/games/2475/mods?id=7&_limit=2 ",
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
            (200, json!({"data": [api_mod(7, "Main", 1)], "result_total": 1}))
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
            (200, json!({"data": [api_mod(7, "Pending", 0)], "result_total": 1}))
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
                "metadata" => json!({"data": [api_mod(2, "Wrong identity", 1)]}),
                "latest-file" => {
                    let mut item=api_mod(1,"Correct metadata",1);
                    item["modfile"]=api_file(2,99,"2.0","https://downloads.example/latest.zip");
                    json!({"data": [item]})
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

#[tokio::test]
async fn get_mod_uses_list_endpoint_for_pat_compatibility_and_enforces_existence_and_uniqueness() {
    let long_pat = format!("eyJ{}.{}.{}", "A".repeat(2500), "B".repeat(2500), "C".repeat(1000));
    let mut server = spawn_server(|request| {
        if request.starts_with("GET /v1/games/2475/mods/7 ") {
            (
                403,
                json!({"error": {"code": 403, "message": "Your personal access token does not grant access to this resource"}}),
            )
        } else if request.starts_with("GET /v1/games/2475/mods?id=7&_limit=2 ") {
            (200, json!({"data": [api_mod(7, "PAT Compatible", 1)], "result_total": 1}))
        } else if request.starts_with("GET /v1/games/2475/mods?id=404&_limit=2 ") {
            (200, json!({"data": [], "result_total": 0}))
        } else if request.starts_with("GET /v1/games/2475/mods?id=99&_limit=2 ") {
            (
                200,
                json!({"data": [api_mod(99, "Duplicate A", 1), api_mod(99, "Duplicate B", 1)], "result_total": 2}),
            )
        } else {
            (404, json!({"error": "unexpected route"}))
        }
    })
    .await;
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let credential = vault
        .store(SecretValue::new(long_pat.as_bytes().to_vec()).unwrap())
        .unwrap();
    let broker = NetworkBroker::new(&vault, None).unwrap();
    let source = ModioProvider::new(
        broker,
        ModioConfig {
            api_base: server.origin.join("v1/").unwrap(),
            credential: Some(credential),
            proxy_policy: ProxyPolicy::Direct,
            download_origins: Vec::new(),
        },
    )
    .unwrap();

    let found = source
        .get_mod(
            &game(),
            &ModRef {
                source_id: SOURCE_ID.into(),
                remote_game_key: "drg".into(),
                remote_mod_id: "7".into(),
                version_id: None,
                remote_file_id: None,
            },
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(found.name, "PAT Compatible");
    let captured = server.requests.recv().await.unwrap();
    assert!(captured.starts_with("GET /v1/games/2475/mods?id=7&_limit=2 "), "{captured}");
    assert!(captured.contains(&format!("Bearer {long_pat}")));

    assert!(matches!(
        source
            .get_mod(
                &game(),
                &ModRef {
                    source_id: SOURCE_ID.into(),
                    remote_game_key: "drg".into(),
                    remote_mod_id: "404".into(),
                    version_id: None,
                    remote_file_id: None,
                },
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::NotFound(_))
    ));
    assert!(matches!(
        source
            .get_mod(
                &game(),
                &ModRef {
                    source_id: SOURCE_ID.into(),
                    remote_game_key: "drg".into(),
                    remote_mod_id: "99".into(),
                    version_id: None,
                    remote_file_id: None,
                },
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn nullable_and_empty_object_modio_fields_and_dependency_objects_deserialize_cleanly() {
    let server = spawn_server(|request| {
        if request.contains("/dependencies") {
            (
                200,
                json!({
                    "data": [
                        {"mod_id": 88, "date_added": 1700000000}
                    ],
                    "result_total": 1
                }),
            )
        } else {
            (
                200,
                json!({
                    "data": [
                        {
                            "id": 11,
                            "game_id": 2475,
                            "status": 1,
                            "visible": 1,
                            "name": "No File Yet",
                            "name_id": "no-file-yet",
                            "summary": null,
                            "description_plaintext": null,
                            "profile_url": "",
                            "submitted_by": null,
                            "logo": {},
                            "modfile": {},
                            "tags": null,
                            "stats": null
                        },
                        {
                            "id": 12,
                            "game_id": 2475,
                            "status": 1,
                            "visible": 1,
                            "name": "Null File Version",
                            "name_id": "null-file-version",
                            "summary": "Real mod.io payload",
                            "description_plaintext": null,
                            "profile_url": "https://mod.io/g/drg/m/null-file-version",
                            "submitted_by": {"username": null},
                            "logo": {"thumb_640x360": "", "original": "https://images.example/orig.png"},
                            "modfile": {
                                "id": 55,
                                "mod_id": 12,
                                "date_added": 1700000000,
                                "virus_status": 1,
                                "virus_positive": 0,
                                "filesize": 2048,
                                "filename": "mod.zip",
                                "version": null,
                                "filehash": {},
                                "download": {}
                            },
                            "tags": [{"name": null}, {"name": "Verified"}],
                            "stats": {"downloads_total": null, "subscribers_total": 5, "ratings_weighted_aggregate": null}
                        }
                    ],
                    "result_total": 2
                }),
            )
        }
    })
    .await;
    let source = provider(&server.origin, Vec::new());
    let page = source
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
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].name, "No File Yet");
    assert!(page.items[0].latest_file.is_none());
    assert!(page.items[0].profile_url.is_none());
    assert!(page.items[0].logo_url.is_none());
    assert_eq!(page.items[1].name, "Null File Version");
    assert_eq!(page.items[1].logo_url.as_deref(), Some("https://images.example/orig.png"));
    assert_eq!(page.items[1].tags, vec!["Verified"]);
    let latest = page.items[1].latest_file.as_ref().unwrap();
    assert_eq!(latest.remote_file_id, "55");
    assert_eq!(latest.display_version, "");

    let dependencies = source
        .get_dependencies(&game(), &page.items[1].mod_ref, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(dependencies.len(), 1);
    assert_eq!(dependencies[0].mod_ref.remote_mod_id, "88");
}

#[tokio::test]
async fn list_subscribed_requires_credential_and_filters_to_game_binding() {
    let server = spawn_server(|request| {
        assert!(request.starts_with("GET /v1/me/subscribed?game_id=2475&"));
        (
            200,
            json!({
                "result_total": 2,
                "data": [
                    api_mod(101, "DRG Subscribed", 1),
                    {
                        "id": 202,
                        "game_id": 9999,
                        "name": "Other Game Mod",
                        "name_id": "other-game",
                        "status": 1,
                        "visible": 1
                    }
                ]
            }),
        )
    })
    .await;
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let unauthenticated = ModioProvider::new(
        NetworkBroker::new(&vault, None).unwrap(),
        ModioConfig {
            api_base: server.origin.join("v1/").unwrap(),
            credential: None,
            proxy_policy: ProxyPolicy::Direct,
            download_origins: Vec::new(),
        },
    )
    .unwrap();
    assert!(matches!(
        unauthenticated
            .list_subscribed(&game(), None, 20, &CancellationToken::default())
            .await,
        Err(SourceError::Unauthorized)
    ));

    let credential = vault
        .store(SecretValue::new("subscribed-pat").unwrap())
        .unwrap();
    let authed = ModioProvider::new(
        NetworkBroker::new(&vault, None).unwrap(),
        ModioConfig {
            api_base: server.origin.join("v1/").unwrap(),
            credential: Some(credential),
            proxy_policy: ProxyPolicy::Direct,
            download_origins: Vec::new(),
        },
    )
    .unwrap();
    let page = authed
        .list_subscribed(&game(), None, 20, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "DRG Subscribed");
    assert_eq!(page.items[0].mod_ref.remote_mod_id, "101");
}
