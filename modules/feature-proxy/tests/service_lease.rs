use std::{collections::HashSet, fs, net::{IpAddr, Ipv4Addr}, path::Path};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_module_feature_proxy::{PrivilegeOperation, ProxyServiceManager, StartOptions, MODULE_ID, SERVICE_PROTOCOL};
use mintcat_runtime_artifacts::{module_signing_digest, ArtifactStore, TrustStore};
use mintcat_runtime_jobs::{ConfirmationPolicy, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{ModuleManifest, ModuleComponent, ModuleState, ModuleActivation, ModuleFile, ModuleSignature, SignatureAlgorithm};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture { sandbox:tempfile::TempDir, store:ArtifactStore, jobs:JobStore, manager:ProxyServiceManager, key:SigningKey, target:String }
impl Fixture {
    fn new() -> Self {
        let sandbox=tempfile::tempdir().unwrap();
        let layout=RuntimePaths::new(sandbox.path().join("runtime")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let key=SigningKey::from_bytes(&[27;32]);
        let mut trust=TrustStore::default(); trust.trust_production_key("mintcat.test","fixture",key.verifying_key());
        let target=target_lexicon::HOST.to_string();
        let store=ArtifactStore::from_layout(&layout,RuntimeEnvironment::Test,&target,"1",trust).unwrap();
        let jobs=JobStore::open(KernelDatabase::open(&layout).unwrap(),"proxy-test",128).unwrap();
        let manager=ProxyServiceManager::new(store.clone(),jobs.clone(),sandbox.path().join("proxy-data")).unwrap();
        fs::write(sandbox.path().join("proxy-data/accelerate_cache.json"),"[]").unwrap();
        Self{sandbox,store,jobs,manager,key,target}
    }
    fn publish(&self, version:&str, protocol:&str)->String {
        let source=self.sandbox.path().join(format!("package-{version}")); fs::create_dir(&source).unwrap();
        let executable=Path::new(env!("CARGO_BIN_EXE_mintcat-proxy"));
        let entry=executable.file_name().unwrap().to_string_lossy().to_string();
        fs::copy(executable,source.join(&entry)).unwrap();
        let bytes=fs::read(source.join(&entry)).unwrap();
        let manifest=ModuleManifest{
            schema_version:1,id:MODULE_ID.into(),version:version.into(),publisher:"mintcat.test".into(),target:self.target.clone(),host_api:"1".into(),
            components:vec![ModuleComponent{kind:"service".into(),entry:entry.clone(),runtime:protocol.into(),state_model:"daemon-owned".into()}],
            provides:vec![],requires:vec![],permissions:vec![],contributes:json!({}),
            state:ModuleState{schema_version:1,readable_range:"1".into(),writable_range:"1".into(),rollback_before_write_only:Some(true)},
            activation:ModuleActivation{trigger:"on-demand".into(),drain_policy:"finish-requests".into()},
            files:vec![ModuleFile{path:entry,size:bytes.len() as u64,sha256:format!("sha256:{:x}",Sha256::digest(bytes)),executable:true}],
        };
        let signed=module_signing_digest(&source,&manifest).unwrap();
        let signature=ModuleSignature{schema_version:1,publisher:manifest.publisher.clone(),key_id:"fixture".into(),algorithm:SignatureAlgorithm::Ed25519,signature:BASE64.encode(self.key.sign(&signed).to_bytes())};
        self.store.publish_module(&source,&manifest,&signature).unwrap().digest
    }
    fn credentials(&mut self,digest:&str) {
        let job=self.jobs.register_job(RegisterJob{command:"proxy.privilege".into(),owner_scope:MODULE_ID.into(),idempotency_key:"credentials".into(),request_hash:"credentials".into(),target:json!({"digest":digest}),confirmation_policy:ConfirmationPolicy::Required}).unwrap().job;
        let expires=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64+600_000;
        let action=self.manager.request_privilege(&job.job_id,digest,PrivilegeOperation::CreateCredentials,expires).unwrap();
        assert!(self.manager.execute_privilege(&action.action_id,digest,PrivilegeOperation::CreateCredentials).is_err());
        assert!(!self.sandbox.path().join("proxy-data/ca.key").exists());
        self.jobs.approve_action(MODULE_ID,&action.action_id,action.revision,&json!({"approved":true}),"approve-credentials","approve-credentials").unwrap();
        self.manager.execute_privilege(&action.action_id,digest,PrivilegeOperation::CreateCredentials).unwrap();
    }
    fn options(port:u16)->StartOptions { StartOptions{port,offline:true,bind:IpAddr::V4(Ipv4Addr::LOCALHOST)} }
}

#[tokio::test]
async fn real_offline_tls_proxy_preserves_leased_version_through_update_and_revocation() {
    let mut fixture=Fixture::new();
    let old=fixture.publish("1.0.0",SERVICE_PROTOCOL);
    let new=fixture.publish("2.0.0",SERVICE_PROTOCOL);
    fixture.credentials(&old);
    let upstream=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address=upstream.local_addr().unwrap();
    let upstream_task=tokio::spawn(async move {
        let (mut stream,_)=upstream.accept().await.unwrap();
        let mut request=vec![0;4096]; let mut count=0;
        while !request[..count].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            assert!(count < request.len(), "upstream request headers exceed fixture bound");
            let received=tokio::time::timeout(std::time::Duration::from_secs(5),stream.read(&mut request[count..])).await.unwrap().unwrap();
            assert!(received > 0, "proxy upstream request ended before headers");
            count += received;
        }
        assert!(String::from_utf8_lossy(&request[..count]).contains("GET /https%3A%2F%2Flocalhost%2Fproof"));
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 19\r\nConnection: close\r\n\r\noffline proxy works").await.unwrap();
    });
    fs::write(fixture.sandbox.path().join("proxy-data/custom_rules.json"),serde_json::to_vec(&json!([{"name":"offline","domains":["localhost"],"proxy":format!("http://{upstream_address}")}])).unwrap()).unwrap();
    let status=fixture.manager.start(&old,Fixture::options(0)).await.unwrap();
    let identity=status.identity.unwrap(); let address=status.address.unwrap();
    assert_eq!(identity.module_digest,old); assert_eq!(identity.module_version,"1.0.0");
    let ca=reqwest::Certificate::from_pem(&fs::read(fixture.sandbox.path().join("proxy-data/ca.pem")).unwrap()).unwrap();
    let client=reqwest::Client::builder().no_proxy().add_root_certificate(ca).resolve("localhost",address).build().unwrap();
    let response=client.get(format!("https://localhost:{}/proof",address.port())).send().await.unwrap();
    assert_eq!(response.status(),reqwest::StatusCode::OK); assert_eq!(response.text().await.unwrap(),"offline proxy works");
    upstream_task.await.unwrap();
    assert!(fixture.manager.start(&new,Fixture::options(0)).await.unwrap_err().to_string().contains("another proxy artifact"));
    fixture.manager.revoke(old.clone()).unwrap();
    assert_eq!(fixture.manager.health().await.unwrap().identity.unwrap(),identity);
    let retained=HashSet::from([new.clone()]);
    let gc=fixture.store.collect_garbage(&retained).unwrap();
    assert!(gc.leased.contains(&format!("module:{old}")));
    assert!(!fixture.manager.stop().await.unwrap().running);
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    assert!(fixture.manager.start(&old,Fixture::options(0)).await.unwrap_err().to_string().contains("revoked"));
    let mut reopened = ProxyServiceManager::new(fixture.store.clone(), fixture.jobs.clone(), fixture.sandbox.path().join("proxy-data")).unwrap();
    assert!(reopened.start(&old,Fixture::options(0)).await.unwrap_err().to_string().contains("revoked"));
    let gc=fixture.store.collect_garbage(&retained).unwrap(); assert!(gc.removed.contains(&format!("module:{old}")));
}

#[tokio::test]
async fn protocol_mismatch_and_failed_ready_never_claim_or_kill_unowned_listener() {
    let mut fixture=Fixture::new();
    let invalid=fixture.publish("3.0.0","mintcat.proxy-service/99");
    assert!(fixture.manager.start(&invalid,Fixture::options(0)).await.unwrap_err().to_string().contains("protocol missing"));
    let valid=fixture.publish("1.0.0",SERVICE_PROTOCOL); fixture.credentials(&valid);
    fs::write(fixture.sandbox.path().join("proxy-data/custom_rules.json"),b"[{\"name\":\"offline\",\"domains\":[\"localhost\"],\"forward\":\"http://127.0.0.1:12345\"}]").unwrap();
    let occupied=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address=occupied.local_addr().unwrap();
    let error=fixture.manager.start(&valid,Fixture::options(address.port())).await.unwrap_err();
    assert!(format!("{error:#}").contains("failed readiness"));
    assert!(!fixture.manager.status().unwrap().running);
    let connection=tokio::net::TcpStream::connect(address).await.unwrap();
    let (_accepted,_)=occupied.accept().await.unwrap(); drop(connection);
    assert!(!fixture.manager.stop().await.unwrap().running);
    let gc=fixture.store.collect_garbage(&HashSet::new()).unwrap(); assert!(gc.removed.contains(&format!("module:{valid}")));
}

#[test]
fn local_human_privilege_receipt_replays_without_recreating_credentials() {
    let mut fixture = Fixture::new();
    let digest = fixture.publish("1.0.0", SERVICE_PROTOCOL);
    fixture.manager = ProxyServiceManager::new_for_owner(fixture.store.clone(), fixture.jobs.clone(), fixture.sandbox.path().join("proxy-data"), "local-user").unwrap();
    let job = fixture.jobs.register_job(RegisterJob {
        command: "proxy.privilege".into(), owner_scope: "local-user".into(), idempotency_key: "local-credentials".into(),
        request_hash: "local-credentials".into(), target: json!({"digest":digest,"operation":{"operation":"create-credentials"}}),
        confirmation_policy: ConfirmationPolicy::Required,
    }).unwrap().job;
    let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64 + 600_000;
    let action = fixture.manager.request_privilege(&job.job_id, &digest, PrivilegeOperation::CreateCredentials, expires).unwrap();
    assert_eq!(fixture.jobs.get_action("local-user", &action.action_id).unwrap().state, mintcat_sdk::ActionState::Pending);
    assert!(fixture.jobs.get_action(MODULE_ID, &action.action_id).is_err());
    fixture.jobs.approve_action("local-user", &action.action_id, action.revision, &json!({"approved":true}), "local-approve", "local-approve").unwrap();
    fixture.manager.execute_privilege(&action.action_id, &digest, PrivilegeOperation::CreateCredentials).unwrap();
    let key = fs::read(fixture.sandbox.path().join("proxy-data/ca.key")).unwrap();
    let epoch = fixture.manager.write_epoch();
    fixture.manager.execute_privilege(&action.action_id, &digest, PrivilegeOperation::CreateCredentials).unwrap();
    assert_eq!(fs::read(fixture.sandbox.path().join("proxy-data/ca.key")).unwrap(), key);
    assert_eq!(fixture.manager.write_epoch(), epoch);
    assert_eq!(fixture.jobs.get_job("local-user", &job.job_id).unwrap().state, mintcat_sdk::JobState::Succeeded);
    assert_eq!(fixture.jobs.get_action("local-user", &action.action_id).unwrap().state, mintcat_sdk::ActionState::Consumed);
}

#[test]
fn portable_proxy_configuration_excludes_machine_authority_and_fences_import() {
    let source=Fixture::new();
    let root=source.sandbox.path().join("proxy-data");
    fs::write(root.join("custom_rules.json"),r#"[{"name":"fixture","domains":["fixture.invalid"]}]"#).unwrap();
    fs::write(root.join("ca.key"),"PRIVATE-KEY-SENTINEL").unwrap();
    fs::write(root.join("hosts-owner.json"),"HOST-AUTHORITY-SENTINEL").unwrap();
    let snapshot=source.manager.portable_snapshot().unwrap();
    assert_eq!(snapshot["custom_rules.json"][0]["domains"][0],"fixture.invalid");
    assert!(!snapshot.contains_key("ca.key"));
    assert!(!snapshot.contains_key("hosts-owner.json"));
    let mut target=Fixture::new();
    let root=target.sandbox.path().join("proxy-data");
    fs::remove_file(root.join("accelerate_cache.json")).unwrap();
    assert!(target.manager.import_empty_portable(&snapshot,1).is_err());
    assert!(!root.join("custom_rules.json").exists());
    target.manager.import_empty_portable(&snapshot,0).unwrap();
    assert_eq!(target.manager.portable_snapshot().unwrap(),snapshot);
    assert!(!root.join("ca.key").exists());
    assert!(!root.join("hosts-owner.json").exists());
    assert!(target.manager.import_empty_portable(&snapshot,0).is_err());
    assert_eq!(target.manager.portable_snapshot().unwrap(),snapshot);
}
