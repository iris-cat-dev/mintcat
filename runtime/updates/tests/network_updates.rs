mod common;
use common::Fixture;
use std::sync::Arc;
use mintcat_runtime_broker::{CredentialVault, MemoryCredentialStore, NetworkBroker, Origin};
use mintcat_runtime_transfers::{TransferConfig, TransferManager};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};
use url::Url;

#[tokio::test]
async fn broker_metadata_and_exact_transfer_payload_activate_the_signed_repository_closure() {
    let fixture = Fixture::new().await;
    let (source, record) = fixture.package("network", 1, b"actual HTTP payload", None, false); fixture.publish("network", 1, record.clone(), source);
    let root = fixture.repository.path().to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let mut requested = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop { let mut chunk = [0; 1024]; let n = socket.read(&mut chunk).await.unwrap(); assert!(n > 0); bytes.extend_from_slice(&chunk[..n]); if bytes.windows(4).any(|w| w == b"\r\n\r\n") { break; } }
            let request = std::str::from_utf8(&bytes).unwrap(); let path = request.split_whitespace().nth(1).unwrap().trim_start_matches('/');
            assert!(!path.contains("..")); requested.push(path.to_owned());
            let body = std::fs::read(root.join(path)).unwrap();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap(); socket.write_all(&body).await.unwrap();
        }
        requested
    });
    let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default())); let broker = NetworkBroker::new(&vault, None).unwrap();
    let transfers = TransferManager::open(fixture.database.clone(), fixture.artifacts.clone(), broker.clone(), fixture.sandbox.path().join("transfer-staging"), TransferConfig::default()).unwrap();
    let origins = vec![Origin::from_url(&base).unwrap()];
    let bundle = fixture.updates.fetch_repository(&broker, &base, origins.clone()).await.unwrap();
    let operation = fixture.updates.stage_network(bundle, &base, origins, &transfers, "network", "stable", 0, 100).await.unwrap();
    fixture.updates.prepare(&operation.operation_id, 100).await.unwrap(); fixture.updates.activate(&operation.operation_id, 100).unwrap(); fixture.updates.confirm(&operation.operation_id, 100).await.unwrap();
    let installed = fixture.artifacts.load_module(&record.digest).unwrap();
    assert_eq!(std::fs::read(installed.root.join("data.bin")).unwrap(), b"actual HTTP payload");
    assert_eq!(installed.manifest, record.manifest);
    let requests = server.await.unwrap(); assert_eq!(requests, vec!["current.json".to_owned(), format!("artifacts/sha256/{}/payload/data.bin", &record.digest[7..])]);
}
