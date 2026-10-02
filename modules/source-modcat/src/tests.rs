use std::sync::Arc;

use mintcat_runtime_broker::{
    CancellationToken, CredentialVault, MemoryCredentialStore, NetworkBroker, ProxyPolicy,
    SecretValue,
};
use mintcat_sdk::ModRef;
use mintcat_source_api::{SearchRequest, SourceError, SourceGameBinding, SourceProvider};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::mpsc,
};
use url::Url;

use crate::{ModcatConfig, ModcatProvider, SOURCE_ID};

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

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2048];
    let header_end = loop {
        let read = socket.read(&mut buffer).await.unwrap();
        assert!(read > 0);
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
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
    }
    String::from_utf8(bytes).unwrap()
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
            let request = read_request(&mut socket).await;
            sender.send(request.clone()).unwrap();
            let (status, body) = handler(&request);
            let body = serde_json::to_vec(&body).unwrap();
            let reason = if status == 200 { "OK" } else { "Unauthorized" };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    TestServer {
        origin,
        requests,
        task,
    }
}

fn binding(game_id: &str, remote: &str, provider: &str) -> SourceGameBinding {
    SourceGameBinding {
        game_id: game_id.to_owned(),
        remote_game_key: remote.to_owned(),
        provider_game_id: provider.to_owned(),
    }
}

fn search_request(cursor: Option<String>) -> SearchRequest {
    SearchRequest {
        query: "ore".to_owned(),
        cursor,
        page_size: 20,
        sort_field: None,
        sort_direction: None,
    }
}

fn provider(
    origin: &Url,
    credential: Option<mintcat_runtime_broker::CredentialRef>,
    vault: &CredentialVault,
) -> ModcatProvider {
    let broker = NetworkBroker::new(vault, None).unwrap();
    ModcatProvider::new(
        broker,
        ModcatConfig {
            api_base: origin.clone(),
            credential,
            proxy_policy: ProxyPolicy::Direct,
            download_request_origin: Some(Url::parse("http://tauri.localhost/").unwrap()),
        },
    )
    .unwrap()
}

fn list_mod(id: &str, name: &str) -> Value {
    json!({
        "ModId": id,
        "Name": name,
        "PicUrl": "https://images.example/mod.png",
        "ModTypeEntities": [{"TypeName": "Utility"}],
        "AVGPoint": 4.5,
        "DownloadCount": 10,
        "CreatorNickName": "Author"
    })
}

fn version(id: &str, file_id: &str, number: &str) -> Value {
    json!({
        "VersionId": id,
        "ModId": "mod-main",
        "VersionNumber": number,
        "FilesId": file_id,
        "CreatedAt": if id == "version-old" { "2024-01-01T00:00:00Z" } else { "2025-01-01T00:00:00Z" },
        "Status": "Approved",
        "Files": {
            "FilesId": file_id,
            "FilesName": format!("{file_id}.zip"),
            "Size": if id == "version-old" { "100" } else { "200" },
            "SoftDeleted": false
        }
    })
}

fn detail(game_id: &str) -> Value {
    json!({
        "ModId": "mod-main",
        "Name": "Main Mod",
        "Description": "Details",
        "CreatedAt": "2024-01-01 00:00:00",
        "UpdatedAt": "2025-01-01T00:00:00Z",
        "DownloadCount": 55,
        "SoftDeleted": false,
        "PicUrl": "https://images.example/main.png",
        "GameId": game_id,
        "ModTypeEntities": [{"Types": {"TypeName": "Framework"}}],
        "CreatorEntity": {"NickName": "Creator"},
        "AVGPoint": 4.8,
        "ModVersionEntities": [
            version("version-old", "file-old", "1.0"),
            version("version-new", "file-new", "2.0")
        ],
        "ModDependenceEntities": [
            {"DependenceModVersion": {
                "VersionId": "dependency-version",
                "ModId": "dependency-mod",
                "VersionNumber": "3.0",
                "FilesId": "dependency-file",
                "Status": "Approved",
                "Files": {"FilesId": "dependency-file", "FilesName": "dep.zip", "Size": "20"}
            }},
            {"ModIOURL": "https://mod.io/g/drg/m/modio-dependency"}
        ]
    })
}

#[tokio::test]
async fn search_keeps_drg_and_rogue_core_game_bindings_separate() {
    let mut server = spawn_server(|request| {
        let item = if request.contains("\"GameId\":\"drgrc\"") {
            list_mod("rc-mod", "Rogue Core Mod")
        } else {
            list_mod("drg-mod", "DRG Mod")
        };
        (200, json!({"ResultCode": 200, "ResultData": [item]}))
    })
    .await;
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let source = provider(&server.origin, None, &vault);
    let drg = source
        .search(
            &binding("game:drg", "drg", "drg"),
            &search_request(None),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    let rc = source
        .search(
            &binding("game:rogue-core", "rogue-core", "drgrc"),
            &search_request(None),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(drg.items[0].mod_ref.remote_mod_id, "drg-mod");
    assert_eq!(drg.items[0].mod_ref.remote_game_key, "drg");
    assert_eq!(rc.items[0].mod_ref.remote_mod_id, "rc-mod");
    assert_eq!(rc.items[0].mod_ref.remote_game_key, "rogue-core");
    assert!(server
        .requests
        .recv()
        .await
        .unwrap()
        .contains("\"GameId\":\"drg\""));
    assert!(server
        .requests
        .recv()
        .await
        .unwrap()
        .contains("\"GameId\":\"drgrc\""));
}

#[tokio::test]
async fn version_file_identity_and_download_resolution_are_exact() {
    let server =
        spawn_server(|_| (200, json!({"ResultCode": 200, "ResultData": detail("drg")}))).await;
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let credential = vault
        .store(SecretValue::new("modcat-token").unwrap())
        .unwrap();
    let source = provider(&server.origin, Some(credential.clone()), &vault);
    let old = ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: "drg".to_owned(),
        remote_mod_id: "mod-main".to_owned(),
        version_id: Some("version-old".to_owned()),
        remote_file_id: None,
    };
    let versions = source
        .list_versions(
            &binding("game:drg", "drg", "drg"),
            &old,
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(versions[0].remote_file_id, "file-old");
    assert_eq!(versions[1].remote_file_id, "file-new");
    let descriptor = source
        .resolve_download(
            &binding("game:drg", "drg", "drg"),
            &old,
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        descriptor.mod_ref.version_id.as_deref(),
        Some("version-old")
    );
    assert_eq!(
        descriptor.mod_ref.remote_file_id.as_deref(),
        Some("file-old")
    );
    assert_eq!(descriptor.expected_size, 100);
    assert!(descriptor.url.query().unwrap().contains("FileId=file-old"));
    assert_eq!(
        descriptor.headers.get("origin").unwrap(),
        "http://tauri.localhost"
    );
    assert_eq!(descriptor.credential.unwrap().reference, credential);

    let dependencies = source
        .get_dependencies(
            &binding("game:drg", "drg", "drg"),
            &old,
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    assert_eq!(dependencies[0].mod_ref.remote_mod_id, "dependency-mod");
    assert_eq!(
        dependencies[0].mod_ref.remote_file_id.as_deref(),
        Some("dependency-file")
    );
    assert_eq!(dependencies[1].mod_ref.source_id, "modio");
    assert_eq!(dependencies[1].mod_ref.remote_mod_id, "modio-dependency");
}

#[tokio::test]
async fn wrong_game_and_auth_failures_never_look_successful() {
    let server = spawn_server(|request| {
        if request
            .to_ascii_lowercase()
            .contains("authorization: bearer expired")
        {
            (401, json!({"ResultCode": 401, "ResultMsg": "expired"}))
        } else {
            (
                200,
                json!({"ResultCode": 200, "ResultData": detail("drgrc")}),
            )
        }
    })
    .await;
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let source = provider(&server.origin, None, &vault);
    let mod_ref = ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: "drg".to_owned(),
        remote_mod_id: "mod-main".to_owned(),
        version_id: None,
        remote_file_id: None,
    };
    assert!(matches!(
        source
            .get_mod(
                &binding("game:drg", "drg", "drg"),
                &mod_ref,
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::InvalidResponse(_))
    ));

    let credential = vault.store(SecretValue::new("expired").unwrap()).unwrap();
    let authenticated = provider(&server.origin, Some(credential), &vault);
    assert!(matches!(
        authenticated
            .search(
                &binding("game:drg", "drg", "drg"),
                &search_request(None),
                &CancellationToken::default(),
            )
            .await,
        Err(SourceError::Unauthorized)
    ));
}

#[tokio::test]
async fn link_parser_is_host_scoped_and_preserves_string_mod_ids() {
    let server = spawn_server(|_| (200, json!({"ResultCode": 200, "ResultData": []}))).await;
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
    let source = provider(&server.origin, None, &vault);
    let game = binding("game:drg", "drg", "drg");
    let parsed = source
        .parse_url(
            &game,
            &Url::parse("https://modcat.top/#/modDetail?ModId=uuid-like-id").unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(parsed.remote_mod_id, "uuid-like-id");
    assert!(source
        .parse_url(
            &game,
            &Url::parse("https://evil.example/?ModId=uuid-like-id").unwrap(),
        )
        .unwrap()
        .is_none());
}
