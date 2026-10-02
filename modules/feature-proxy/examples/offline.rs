use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_module_feature_proxy::{PrivilegeOperation, ProxyServiceManager, StartOptions, MODULE_ID, SERVICE_PROTOCOL};
use mintcat_runtime_artifacts::{module_signing_digest, ArtifactStore, TrustStore};
use mintcat_runtime_jobs::{ConfirmationPolicy, JobStore, RegisterJob};
use mintcat_runtime_kernel::{KernelDatabase, RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{ModuleManifest, ModuleComponent, ModuleState, ModuleActivation, ModuleFile, ModuleSignature, SignatureAlgorithm};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let executable=PathBuf::from(std::env::args().nth(1).expect("usage: offline <built mintcat-proxy binary>"));
    let sandbox=tempfile::tempdir()?;
    let layout=RuntimePaths::new(sandbox.path().join("runtime"))?.initialize(RuntimeEnvironment::Test)?;
    let key=SigningKey::from_bytes(&[43;32]); // isolated Test-environment fixture, never a production signer
    let mut trust=TrustStore::default(); trust.trust_production_key("mintcat.smoke","offline",key.verifying_key());
    let target=target_lexicon::HOST.to_string();
    let store=ArtifactStore::from_layout(&layout,RuntimeEnvironment::Test,&target,"1",trust)?;
    let source=sandbox.path().join("source"); fs::create_dir(&source)?;
    let entry=executable.file_name().unwrap().to_string_lossy().to_string(); fs::copy(&executable,source.join(&entry))?;
    let bytes=fs::read(source.join(&entry))?;
    let manifest=ModuleManifest{
        schema_version:1,id:MODULE_ID.into(),version:"1.0.0".into(),publisher:"mintcat.smoke".into(),target,host_api:"1".into(),
        components:vec![ModuleComponent{kind:"service".into(),entry:entry.clone(),runtime:SERVICE_PROTOCOL.into(),state_model:"daemon-owned".into()}],
        provides:vec![],requires:vec![],permissions:vec![],contributes:json!({}),
        state:ModuleState{schema_version:1,readable_range:"1".into(),writable_range:"1".into(),rollback_before_write_only:Some(true)},
        activation:ModuleActivation{trigger:"on-demand".into(),drain_policy:"finish-requests".into()},
        files:vec![ModuleFile{path:entry,size:bytes.len() as u64,sha256:format!("sha256:{:x}",Sha256::digest(bytes)),executable:true}],
    };
    let digest=module_signing_digest(&source,&manifest)?;
    let signature=ModuleSignature{schema_version:1,publisher:manifest.publisher.clone(),key_id:"offline".into(),algorithm:SignatureAlgorithm::Ed25519,signature:BASE64.encode(key.sign(&digest).to_bytes())};
    let installed=store.publish_module(&source,&manifest,&signature)?;
    let jobs=JobStore::open(KernelDatabase::open(&layout)?,"offline-smoke",128)?;
    let data=sandbox.path().join("proxy-data");
    let mut manager=ProxyServiceManager::new(store,jobs.clone(),data.clone())?;
    fs::write(data.join("accelerate_cache.json"),b"[]")?;
    fs::write(data.join("custom_rules.json"),br#"[{"name":"offline","domains":["localhost"],"forward":"http://127.0.0.1:12345"}]"#)?;
    let job=jobs.register_job(RegisterJob{command:"proxy.privilege".into(),owner_scope:MODULE_ID.into(),idempotency_key:"credentials".into(),request_hash:"credentials".into(),target:json!({"digest":installed.digest}),confirmation_policy:ConfirmationPolicy::Required})?.job;
    let expires=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64+600_000;
    let action=manager.request_privilege(&job.job_id,&installed.digest,PrivilegeOperation::CreateCredentials,expires)?;
    jobs.approve_action(MODULE_ID,&action.action_id,action.revision,&json!({"approved":true}),"approve","approve")?;
    manager.execute_privilege(&action.action_id,&installed.digest,PrivilegeOperation::CreateCredentials)?;
    let options=StartOptions{port:0,offline:true,bind:"127.0.0.1".parse()?};
    let ready=manager.start(&installed.digest,options.clone()).await?;
    println!("ready {}",serde_json::to_string(&ready)?);
    println!("health {}",serde_json::to_string(&manager.health().await?)?);
    let address=ready.address.unwrap();
    println!("stop {}",serde_json::to_string(&manager.stop().await?)?);
    anyhow::ensure!(tokio::net::TcpStream::connect(address).await.is_err(),"owned service listener survived stop");
    let occupied=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let error=manager.start(&installed.digest,StartOptions{port:occupied.local_addr()?.port(),..options.clone()}).await.expect_err("occupied listen must fail readiness");
    anyhow::ensure!(!manager.status()?.running,"failed-ready service remained running");
    println!("failed-ready cleanup {error:#}");
    manager.start(&installed.digest,options.clone()).await?;
    manager.revoke(installed.digest.clone())?;
    println!("revoked leased service still healthy {}",manager.health().await?.running);
    println!("revoked stop {}",serde_json::to_string(&manager.stop().await?)?);
    anyhow::ensure!(manager.start(&installed.digest,options).await.is_err(),"revoked artifact restarted");
    println!("offline proxy startup/health/stop/revocation/cleanup smoke complete; no system hosts/trust modified");
    Ok(())
}
