use std::{sync::Arc, time::Duration};
use async_trait::async_trait;
use mintcat_feature_agent::{AgentConfiguration, AgentError, AgentSessions, ApplicationGateway, CreateSession, SendMessage, SessionTarget};
use mintcat_runtime_broker::{CredentialVault, MemoryCredentialStore, NetworkBroker, SecretValue};
use mintcat_sdk::{AppError, ValidatedRequest};
use serde_json::Value;
use tokio::io::AsyncReadExt;
struct Gateway;
#[async_trait]
impl ApplicationGateway for Gateway {
    async fn execute(&self, _: ValidatedRequest) -> Result<Value, AppError> { panic!("these provider fixtures must not invoke tools"); }
}
struct Fixture { sandbox: tempfile::TempDir, vault: CredentialVault, agent: Arc<AgentSessions> }
impl Fixture {
    fn new() -> Self {
        let sandbox = tempfile::tempdir().unwrap();
        let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
        let agent = Arc::new(AgentSessions::open(sandbox.path().join("agent.sqlite3"), NetworkBroker::new(&vault, None).unwrap(), Arc::new(Gateway)).unwrap());
        Self { sandbox, vault, agent }
    }
    fn request(&self, url: &str, credential_ref: Option<String>) -> CreateSession {
        CreateSession { configuration: AgentConfiguration { base_url: url.into(), model: "controlled".into(), credential_ref }, target: SessionTarget { game_id: "game:drg".into(), profile_id: "profile-1".into(), profile_revision: 7, content_revision: 11, installation_id: "installation-1".into(), installation_revision: 3 } }
    }
}
async fn http_request(stream:&mut tokio::net::TcpStream,secret:&str)->Value {
    let mut bytes=Vec::new();let mut buffer=[0;4096];
    let header_end=loop {
        let count=tokio::time::timeout(Duration::from_secs(10),stream.read(&mut buffer)).await.unwrap().unwrap();assert!(count>0);
        bytes.extend_from_slice(&buffer[..count]);assert!(bytes.len()<3*1024*1024);
        if let Some(index)=bytes.windows(4).position(|part|part==b"\r\n\r\n") {break index+4;}
    };
    let headers=String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    assert!(headers.to_ascii_lowercase().contains(&format!("authorization: bearer {secret}")));
    let length:usize=headers.lines().find_map(|line| {let (name,value)=line.split_once(':')?;name.eq_ignore_ascii_case("content-length").then(||value.trim().parse().unwrap())}).unwrap();
    while bytes.len()-header_end<length {
        let count=tokio::time::timeout(Duration::from_secs(10),stream.read(&mut buffer)).await.unwrap().unwrap();assert!(count>0);bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end+length]).unwrap()
}
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn live_queries_and_cancellation_keep_builtin_writer() {
    let fixture=Fixture::new();
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/v1",listener.local_addr().unwrap());let secret="cancel-private-bearer";
    let credential=fixture.vault.store(SecretValue::new(secret.as_bytes().to_vec()).unwrap()).unwrap();
    let first=fixture.agent.create_keyed(fixture.request(&url,Some(credential.as_str().into())),"first","first").unwrap();
    let second=fixture.agent.create_keyed(fixture.request(&url,None),"second","second").unwrap();
    let (accepted,ready)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {
        let (mut stream,_)=listener.accept().await.unwrap();let body=http_request(&mut stream,secret).await;
        assert!(body["messages"].as_array().unwrap().iter().any(|message|message["content"]=="Private cancellation prompt"));accepted.send(()).unwrap();
        let mut byte=[0;1];assert_eq!(tokio::time::timeout(Duration::from_secs(10),stream.read(&mut byte)).await.unwrap().unwrap(),0);
    });
    fixture.agent.prepare_send(SendMessage {session_id:first.session_id.clone(),expected_revision:1,content:"Private cancellation prompt".into()},"job:cancel").unwrap();
    assert_eq!(fixture.agent.prepare_send(SendMessage {session_id:second.session_id.clone(),expected_revision:1,content:"Must remain unsent".into()},"job:second").err(),Some(AgentError::Busy));
    assert_eq!(fixture.agent.get(&second.session_id).unwrap().state,"idle");
    let agent=fixture.agent.clone();let id=first.session_id.clone();let turn=tokio::spawn(async move {agent.run_turn(&id,"job:cancel","native-generation").await});
    tokio::time::timeout(Duration::from_secs(10),ready).await.unwrap().unwrap();
    let agent=fixture.agent.clone();let id=first.session_id.clone();
    let sending=tokio::time::timeout(Duration::from_secs(5),tokio::task::spawn_blocking(move ||agent.get(&id))).await.unwrap().unwrap().unwrap();
    assert_eq!(sending.state,"sending");assert!(matches!(fixture.agent.portable_snapshot(), Err(AgentError::Busy)));assert!(fixture.agent.has_nonempty().unwrap());
    let agent=fixture.agent.clone();let id=first.session_id.clone();
    tokio::time::timeout(Duration::from_secs(5),tokio::task::spawn_blocking(move ||agent.interrupt_turn(&id,"job:cancel"))).await.unwrap().unwrap().unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(5),turn).await.unwrap().unwrap().err(),Some(AgentError::Provider));server.await.unwrap();
    let terminal=fixture.agent.get(&first.session_id).unwrap();assert_eq!(terminal.state,"failed");assert!(terminal.revision>sending.revision);
    assert!(!serde_json::to_string(&terminal).unwrap().contains(secret));
    fixture.agent.prepare_send(SendMessage {session_id:second.session_id.clone(),expected_revision:1,content:"Cancel before run is polled".into()},"job:pending").unwrap();
    fixture.agent.interrupt_turn(&second.session_id,"job:pending").unwrap();
    assert_eq!(fixture.agent.run_turn(&second.session_id,"job:pending","native-generation").await.err(),Some(AgentError::Busy));
    let pending=fixture.agent.get(&second.session_id).unwrap();assert_eq!(pending.state,"failed");assert_eq!(pending.error_code.as_deref(),Some("TURN_INTERRUPTED"));
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn portable_owner_import_strips_authority_preserves_history_and_fences_new_writes() {
    let source=Fixture::new();
    let credential=source.vault.store(SecretValue::new(b"raw-private-token".to_vec()).unwrap()).unwrap();
    let session=source.agent.create_keyed(source.request("http://127.0.0.1:1/v1",Some(credential.as_str().into())),"create","create").unwrap();
    source.agent.prepare_send(SendMessage {session_id:session.session_id.clone(),expected_revision:1,content:"preserve this conversation".into()},"old-machine-job").unwrap();
    assert!(matches!(source.agent.portable_snapshot(),Err(AgentError::Busy)));
    source.agent.interrupt_turn(&session.session_id,"old-machine-job").unwrap();
    let snapshot=source.agent.portable_snapshot().unwrap();
    assert!(snapshot[0].configuration.credential_ref.is_none());
    assert!(snapshot[0].bindings.is_empty());
    assert!(snapshot[0].timeline.iter().any(|item|item.content.contains("Non-authoritative task history") && item.content.contains("old-machine-job")));
    let target=Fixture::new();
    let epoch=target.agent.write_epoch().unwrap();
    let mut contaminated=snapshot.clone();contaminated[0].configuration.credential_ref=Some(credential.as_str().into());
    assert!(matches!(target.agent.validate_portable(&contaminated),Err(AgentError::InvalidRequest)));
    assert!(!target.agent.has_nonempty().unwrap());
    assert!(matches!(target.agent.import_empty_portable(&snapshot,epoch+1),Err(AgentError::Busy)));
    target.agent.import_empty_portable(&snapshot,epoch).unwrap();
    assert_eq!(serde_json::to_value(target.agent.portable_snapshot().unwrap()).unwrap(),serde_json::to_value(&snapshot).unwrap());
    let restored=target.agent.get(&session.session_id).unwrap();
    let version=mintcat_sdk::command_spec("agent.sessions.get").unwrap().contract_version;
    mintcat_sdk::validate_command_result("agent.sessions.get",version,&serde_json::to_value(&restored).unwrap()).unwrap();
    assert!(restored.bindings.is_empty());
    let committed_epoch=target.agent.write_epoch().unwrap();
    assert!(committed_epoch>epoch);
    target.agent.create_keyed(target.request("http://127.0.0.1:1/v1",None),"new-write","new-write").unwrap();
    assert!(matches!(target.agent.import_empty_portable(&snapshot,committed_epoch),Err(AgentError::Busy)));
    assert_eq!(target.agent.get(&session.session_id).unwrap().revision,snapshot[0].revision);
}

#[test]
fn writer_lock_and_keyed_receipts_survive_restart() {
    let fixture = Fixture::new();
    let input = fixture.request("http://127.0.0.1:1/v1", None);
    let created = fixture.agent.create_keyed(input.clone(), "create", "hash").unwrap();
    assert_eq!(fixture.agent.create_keyed(input.clone(), "create", "hash").unwrap().session_id, created.session_id);
    assert!(matches!(fixture.agent.create_keyed(input.clone(), "create", "changed"), Err(AgentError::IdempotencyConflict)));
    let path = fixture.agent.database_path().to_owned();
    assert!(matches!(AgentSessions::open(&path, NetworkBroker::new(&fixture.vault, None).unwrap(), Arc::new(Gateway)), Err(AgentError::Busy)));
    let epoch = fixture.agent.write_epoch().unwrap();
    let Fixture { sandbox: _sandbox, vault, agent } = fixture;
    drop(agent);
    let restarted = AgentSessions::open(path, NetworkBroker::new(&vault, None).unwrap(), Arc::new(Gateway)).unwrap();
    assert_eq!(restarted.write_epoch().unwrap(), epoch);
    assert_eq!(restarted.create_keyed(input, "create", "hash").unwrap().session_id, created.session_id);
    assert_eq!(restarted.write_epoch().unwrap(), epoch);
}
