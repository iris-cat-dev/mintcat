use std::{collections::HashSet, fs, path::Path, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_feature_agent::{AgentConfiguration, AgentError, ApplicationGateway, CreateSession, NativeAgentSessions, NativeAgentStateOwner, SendMessage, SessionTarget, AGENT_CONTRACT_ID};
use mintcat_runtime_artifacts::{module_signing_digest, ArtifactStore, TrustStore};
use mintcat_runtime_broker::{CredentialVault, MemoryCredentialStore, NetworkBroker, SecretValue};
use mintcat_runtime_jobs::{ActionSpec, ConfirmationPolicy, EventUpdate, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_runtime_updates::StateOwner;
use mintcat_sdk::{ActionState, AppError, JobState, ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleSignature, ModuleState, ProvidedContract, SignatureAlgorithm, ValidatedRequest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Gateway { jobs:JobStore }
#[async_trait]
impl ApplicationGateway for Gateway {
    async fn execute(&self, request:ValidatedRequest)->Result<Value,AppError> {
        // The only reachable gateway operation is the canonically decoded read. A provider
        // suggestion to approve must be denied before this authority boundary.
        assert_eq!(request.envelope.method,"actions.get");
        assert_eq!(request.envelope.session_generation,"native-generation");
        let id=request.envelope.params["actionId"].as_str().unwrap();
        Ok(serde_json::to_value(self.jobs.get_action("human",id).unwrap()).unwrap())
    }
}
struct Fixture { sandbox:tempfile::TempDir, artifacts:ArtifactStore, key:SigningKey, target:String, vault:CredentialVault, agent:Arc<NativeAgentSessions>, jobs:JobStore }
impl Fixture {
    fn new()->Self {
        let sandbox=tempfile::tempdir().unwrap();
        let layout=RuntimePaths::new(sandbox.path().join("runtime")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let key=SigningKey::from_bytes(&[31;32]);
        let mut trust=TrustStore::default();trust.trust_production_key("mintcat.test","fixture",key.verifying_key());
        let target=target_lexicon::HOST.to_string();
        let artifacts=ArtifactStore::from_layout(&layout,RuntimeEnvironment::Test,&target,"1",trust).unwrap();
        let jobs=JobStore::open(KernelDatabase::open(&layout).unwrap(),"native-generation",128).unwrap();
        let vault=CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
        let broker=NetworkBroker::new(&vault,None).unwrap();
        let agent=Arc::new(NativeAgentSessions::unavailable(&sandbox.path().join("agent.sqlite3"),broker,Arc::new(Gateway {jobs:jobs.clone()})));
        Self {sandbox,artifacts,key,target,vault,agent,jobs}
    }
    fn package(&self,version:&str)->String {
        let source=self.sandbox.path().join(format!("package-{version}"));fs::create_dir(&source).unwrap();
        let entry="mintcat-agent-worker";
        fs::copy(Path::new(env!("CARGO_BIN_EXE_mintcat-agent-worker")),source.join(entry)).unwrap();
        let bytes=fs::read(source.join(entry)).unwrap();
        let manifest=ModuleManifest {
            schema_version:1,id:"mintcat.feature.agent".into(),version:version.into(),publisher:"mintcat.test".into(),target:self.target.clone(),host_api:"1".into(),
            components:vec![ModuleComponent {kind:"service".into(),entry:entry.into(),runtime:"worker".into(),state_model:"single-writer".into()}],
            provides:vec![ProvidedContract {contract_id:AGENT_CONTRACT_ID.into(),version:1,cardinality:Some("single".into())}],requires:vec![],permissions:vec![],contributes:json!({}),
            state:ModuleState {schema_version:1,readable_range:"1".into(),writable_range:"1".into(),rollback_before_write_only:Some(true)},
            activation:ModuleActivation {trigger:"on-demand".into(),drain_policy:"finish-requests".into()},
            files:vec![ModuleFile {path:entry.into(),size:bytes.len() as u64,sha256:format!("sha256:{:x}",Sha256::digest(bytes)),executable:true}],
        };
        let signed=module_signing_digest(&source,&manifest).unwrap();
        let signature=ModuleSignature {schema_version:1,publisher:manifest.publisher.clone(),key_id:"fixture".into(),algorithm:SignatureAlgorithm::Ed25519,signature:BASE64.encode(self.key.sign(&signed).to_bytes())};
        self.artifacts.publish_module(&source,&manifest,&signature).unwrap().digest
    }
    fn request(&self,url:&str,credential_ref:Option<String>)->CreateSession {
        CreateSession {configuration:AgentConfiguration {base_url:url.into(),model:"controlled".into(),credential_ref},target:SessionTarget {game_id:"game:drg".into(),profile_id:"profile-1".into(),profile_revision:7,content_revision:11,installation_id:"installation-1".into(),installation_revision:3}}
    }
    fn action(&self)->mintcat_sdk::ActionRequired {
        let job=self.jobs.register_job(RegisterJob {command:"install.plan".into(),owner_scope:"human".into(),idempotency_key:"action-job".into(),request_hash:"action-hash".into(),target:json!({"profileId":"profile-1","contentRevision":11}),confirmation_policy:ConfirmationPolicy::Required}).unwrap().job;
        self.jobs.append_event(&job.job_id,EventUpdate::state("preparing",JobState::Preparing)).unwrap();
        self.jobs.create_action(&job.job_id,ActionSpec {kind:"install".into(),operation_id:job.job_id.clone(),target:json!({"profileId":"profile-1","contentRevision":11}),target_revision:11,plan_hash:format!("sha256:{}","1".repeat(64)),module_lock_digest:format!("sha256:{}","2".repeat(64)),expires_at_ms:SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64+60_000,response_schema:json!({"type":"object","additionalProperties":false,"required":["approved"],"properties":{"approved":{"type":"boolean"}}})}).unwrap()
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
async fn http_reply(stream:&mut tokio::net::TcpStream,value:Value) {
    let body=serde_json::to_vec(&value).unwrap();
    let header=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",body.len());
    stream.write_all(header.as_bytes()).await.unwrap();stream.write_all(&body).await.unwrap();
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn signed_owner_callbacks_replay_and_first_new_write_restore_fence() {
    let fixture=Fixture::new();assert!(fixture.agent.is_pristine_absent().unwrap());assert!(!fixture.agent.has_nonempty().unwrap());
    assert_eq!(fixture.agent.get("unavailable").err(),Some(AgentError::ModuleUnavailable));
    let old=fixture.package("1.0.0");let new=fixture.package("2.0.0");
    fixture.agent.replace_native(fixture.artifacts.clone(),&old).unwrap();fixture.agent.read_only_health().unwrap();
    assert_eq!(fixture.agent.create_keyed(fixture.request("http://127.0.0.1:1/v1",None),"paused","paused").err(),Some(AgentError::Busy));
    let owner=NativeAgentStateOwner::new(fixture.agent.clone(),fixture.sandbox.path().join("owner-backups"));
    owner.drain().await.unwrap();
    let absent=owner.backup(1,1).await.unwrap();
    assert_eq!(absent.reference["absent"],true);
    assert!(!fixture.agent.database_path().exists());
    owner.restore(&absent).await.unwrap();
    fixture.agent.open_writes().unwrap();
    let action=fixture.action();
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/v1",listener.local_addr().unwrap());
    let secret="native-private-bearer";
    let credential=fixture.vault.store(SecretValue::new(secret.as_bytes().to_vec()).unwrap()).unwrap();
    let request=fixture.request(&url,Some(credential.as_str().into()));
    let session=fixture.agent.create_keyed(request.clone(),"create","create-hash").unwrap();
    assert!(owner.restore(&absent).await.is_err());
    fixture.agent.open_writes().unwrap();
    let epoch=fixture.agent.write_epoch().unwrap();
    let replay=fixture.agent.create_keyed(request.clone(),"create","create-hash").unwrap();assert_eq!(replay.session_id,session.session_id);assert_eq!(fixture.agent.write_epoch().unwrap(),epoch);
    assert_eq!(fixture.agent.create_keyed(request,"create","different-hash").err(),Some(AgentError::IdempotencyConflict));
    let action_id=action.action_id.clone();let revision=action.revision;
    let server=tokio::spawn(async move {
        let mut requests=Vec::new();
        for index in 0..2 {
            let (mut stream,_)=listener.accept().await.unwrap();requests.push(http_request(&mut stream,secret).await);
            let response=if index==0 {json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
                {"id":"approve","type":"function","function":{"name":"actions__approve","arguments":json!({"actionId":action_id,"expectedRevision":revision,"response":{"approved":true}}).to_string()}},
                {"id":"inspect","type":"function","function":{"name":"actions__get","arguments":json!({"actionId":action_id}).to_string()}}
            ]}}]})}else{json!({"choices":[{"message":{"role":"assistant","content":"A human must approve the durable action."}}]})};
            http_reply(&mut stream,response).await;
        }
        requests
    });
    fixture.agent.prepare_send(SendMessage {session_id:session.session_id.clone(),expected_revision:session.revision,content:"Inspect but do not approve".into()},"job:callback").unwrap();
    let result=fixture.agent.run_turn(&session.session_id,"job:callback","native-generation").await.unwrap();
    assert_eq!(result.state,"idle");assert!(result.bindings.iter().any(|binding|binding.action_id.as_deref()==Some(action.action_id.as_str())));
    assert!(result.timeline.iter().any(|item|item.role=="tool" && item.content.contains("PERMISSION_DENIED")));
    assert_eq!(fixture.jobs.get_action("human",&action.action_id).unwrap().state,ActionState::Pending);
    assert!(!serde_json::to_string(&result).unwrap().contains(secret));
    let requests=server.await.unwrap();assert!(requests[1]["messages"].as_array().unwrap().iter().any(|message|message["role"]=="tool" && message["content"].as_str().unwrap().contains("PERMISSION_DENIED")));
    let gc=fixture.artifacts.collect_garbage(&HashSet::from([new.clone()])).unwrap();assert!(gc.leased.contains(&format!("module:{old}")));
    let competing=NativeAgentSessions::unavailable(fixture.agent.database_path(),NetworkBroker::new(&fixture.vault,None).unwrap(),Arc::new(Gateway {jobs:fixture.jobs.clone()}));
    assert_eq!(competing.replace_native(fixture.artifacts.clone(),&old).err(),Some(AgentError::ModuleUnavailable));
    fixture.agent.pause().unwrap();let backup=fixture.sandbox.path().join("agent-backup.sqlite3");fixture.agent.backup_to(&backup).unwrap();let snapshot_epoch=fixture.agent.write_epoch().unwrap();
    fixture.agent.replace_native(fixture.artifacts.clone(),&new).unwrap();fixture.agent.read_only_health().unwrap();assert_eq!(fixture.agent.get(&session.session_id).unwrap().revision,result.revision);
    fixture.agent.restore_from(&backup,snapshot_epoch).unwrap();fixture.agent.open_writes().unwrap();
    let added=fixture.agent.create_keyed(fixture.request(&url,None),"new-generation","new-generation").unwrap();assert!(fixture.agent.write_epoch().unwrap()>snapshot_epoch);
    fixture.agent.pause().unwrap();assert_eq!(fixture.agent.restore_from(&backup,snapshot_epoch).err(),Some(AgentError::Busy));
    fixture.agent.replace_native(fixture.artifacts.clone(),&old).unwrap();assert_eq!(fixture.agent.restore_from(&backup,snapshot_epoch).err(),Some(AgentError::Busy));assert_eq!(fixture.agent.get(&added.session_id).unwrap().revision,1);
    fixture.agent.open_writes().unwrap();fixture.agent.delete_keyed(&added.session_id,1,"delete-added","delete-added").unwrap();fixture.agent.delete_keyed(&session.session_id,result.revision,"delete","delete").unwrap();
    assert_eq!(fixture.agent.get(&session.session_id).err(),Some(AgentError::NotFound));assert!(fixture.agent.has_nonempty().unwrap());
    // Other Arc holders remain alive: shutdown, not facade destruction, releases both
    // signed-process leases and the writer lock so a distinct owner can take over.
    fixture.agent.shutdown().unwrap();assert!(!fixture.agent.has_native().unwrap());
    assert_eq!(fixture.agent.get(&session.session_id).err(),Some(AgentError::ModuleUnavailable));
    competing.replace_native(fixture.artifacts.clone(),&old).unwrap();competing.read_only_health().unwrap();
    assert!(competing.has_nonempty().unwrap());assert_eq!(competing.get(&session.session_id).err(),Some(AgentError::NotFound));
    let retained_epoch=competing.write_epoch().unwrap();competing.deactivate_native().unwrap();
    assert!(!competing.has_native().unwrap());assert!(!competing.is_pristine_absent().unwrap());
    fixture.agent.replace_native(fixture.artifacts.clone(),&old).unwrap();
    assert_eq!(fixture.agent.write_epoch().unwrap(),retained_epoch);assert!(fixture.agent.has_nonempty().unwrap());
    fixture.agent.shutdown().unwrap();
    let gc=fixture.artifacts.collect_garbage(&HashSet::from([new])).unwrap();assert!(gc.removed.contains(&format!("module:{old}")));
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn live_readonly_queries_and_cancellation_do_not_abandon_native_writer() {
    let fixture=Fixture::new();let digest=fixture.package("1.0.0");fixture.agent.replace_native(fixture.artifacts.clone(),&digest).unwrap();fixture.agent.open_writes().unwrap();
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
    assert_eq!(sending.state,"sending");assert_eq!(fixture.agent.pause().err(),Some(AgentError::Busy));assert!(fixture.agent.has_nonempty().unwrap());
    assert_eq!(fixture.agent.shutdown().err(),Some(AgentError::Busy));
    let agent=fixture.agent.clone();let id=first.session_id.clone();
    tokio::time::timeout(Duration::from_secs(5),tokio::task::spawn_blocking(move ||agent.interrupt_turn(&id,"job:cancel"))).await.unwrap().unwrap().unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(5),turn).await.unwrap().unwrap().err(),Some(AgentError::Provider));server.await.unwrap();
    let terminal=fixture.agent.get(&first.session_id).unwrap();assert_eq!(terminal.state,"failed");assert!(terminal.revision>sending.revision);
    assert!(!serde_json::to_string(&terminal).unwrap().contains(secret));
    fixture.agent.prepare_send(SendMessage {session_id:second.session_id.clone(),expected_revision:1,content:"Cancel before run is polled".into()},"job:pending").unwrap();
    fixture.agent.interrupt_turn(&second.session_id,"job:pending").unwrap();
    assert_eq!(fixture.agent.run_turn(&second.session_id,"job:pending","native-generation").await.err(),Some(AgentError::Provider));
    let pending=fixture.agent.get(&second.session_id).unwrap();assert_eq!(pending.state,"failed");assert_eq!(pending.error_code.as_deref(),Some("TURN_INTERRUPTED"));
    fixture.agent.pause().unwrap();fixture.agent.read_only_health().unwrap();
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn portable_owner_import_strips_authority_preserves_history_and_fences_new_writes() {
    let source=Fixture::new();
    let digest=source.package("portable-source");
    source.agent.replace_native(source.artifacts.clone(),&digest).unwrap();source.agent.open_writes().unwrap();
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
    let digest=target.package("portable-target");
    target.agent.replace_native(target.artifacts.clone(),&digest).unwrap();target.agent.open_writes().unwrap();
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
    source.agent.shutdown().unwrap();target.agent.shutdown().unwrap();
}
