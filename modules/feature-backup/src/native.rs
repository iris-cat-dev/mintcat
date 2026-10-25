use std::{collections::BTreeMap, fs::{self, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, sync::mpsc, time::Duration};
use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_runtime_supervisor::{Invocation, ModuleSupervisor};
use mintcat_sdk::{ArtifactGrantAccess, ArtifactGrantDescriptor, ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState, PermissionRequest, ProvidedContract, ScopedProviderBinding, WorkerRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use crate::{BackupError, BackupManifest, BackupResult, ModuleVersion, PortableSnapshot};

pub const BACKUP_CONTRACT_ID: &str = "backup.worker";
pub const BACKUP_CONTRACT_VERSION: u64 = 2;
const MODULE_ID: &str = "mintcat.feature.backup";

#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CodecRequest { input_grant: String, modules: BTreeMap<String,ModuleVersion> }
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CodecResponse { output: ArtifactGrantDescriptor, result: BackupResult }
struct Work { method: &'static str, input: ArtifactGrant, modules: BTreeMap<String,ModuleVersion>, response: mpsc::Sender<Result<(ArtifactGrant,BackupResult),BackupError>> }

/// Daemon-owned stateless facade over the exact signed immutable scope binding.
/// Large snapshots/archives travel in authenticated artifact grants, not RPC frames.
pub struct NativeBackupCodec { sender: mpsc::Sender<Work>, supervisor: ModuleSupervisor, scope_id:String, provider_digest:String, _lease:mintcat_runtime_artifacts::ModuleLease }
impl NativeBackupCodec {
    pub fn new(supervisor: ModuleSupervisor, scope_id: String, digest: String) -> Result<Self,BackupError> {
        let scope=scope_id.clone(); let provider_digest=digest.clone();
        let lease=supervisor.artifacts().acquire_module(&digest).map_err(|e|BackupError::Module(e.to_string()))?;
        let (sender, receive) = mpsc::channel::<Work>();
        let (ready, readiness) = mpsc::sync_channel(1);
        let transport = supervisor.clone();
        std::thread::Builder::new().name("backup-native-codec".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime)=>runtime, Err(error)=>{let _=ready.send(Err(BackupError::Module(error.to_string())));return;}
            };
            let binding = ScopedProviderBinding { scope_id:scope_id.clone(),contract_id:BACKUP_CONTRACT_ID.into(),contract_version:BACKUP_CONTRACT_VERSION,provider_module_id:MODULE_ID.into(),provider_digest:digest };
            let activated = runtime.block_on(transport.activate(binding)).map(|_|()).map_err(|e|BackupError::Module(e.to_string()));
            let failed = activated.is_err(); let _=ready.send(activated); if failed{return;}
            for work in receive {
                let result = runtime.block_on(async {
                    let params=serde_json::to_value(CodecRequest {input_grant:work.input.descriptor.grant_id.clone(),modules:work.modules})?;
                    let grants=[work.input];
                    let response=transport.invoke(Invocation {scope_id:&scope_id,contract_id:BACKUP_CONTRACT_ID,contract_version:BACKUP_CONTRACT_VERSION,method:work.method,params,grants:&grants,deadline:Duration::from_secs(120)}).await.map_err(|e|match e {
                        mintcat_runtime_supervisor::SupervisorError::WorkerApplication {code,message_key,..} if code=="BACKUP_INVALID"=>BackupError::Invalid(message_key),
                        other=>BackupError::Module(other.to_string()),
                    })?;
                    let response:CodecResponse=serde_json::from_value(response)?;
                    let output=transport.grant_output(&scope_id,BACKUP_CONTRACT_ID,BACKUP_CONTRACT_VERSION,&response.output).map_err(|e|BackupError::Module(e.to_string()))?;
                    Ok((output,response.result))
                });
                let _=work.response.send(result);
            }
        })?;
        readiness.recv().map_err(|_|BackupError::Module("backup worker activation thread exited".into()))??;
        Ok(Self {sender,supervisor,scope_id:scope,provider_digest,_lease:lease})
    }
    pub fn scope_id(&self)->&str {&self.scope_id}
    pub fn provider_digest(&self)->&str {&self.provider_digest}
    fn invoke(&self,method:&'static str,input:&[u8],modules:&BTreeMap<String,ModuleVersion>)->Result<(ArtifactGrant,BackupResult),BackupError> {
        let artifact=self.supervisor.artifacts().publish_blob_reader(&mut std::io::Cursor::new(input),Some("application/json".into())).map_err(|e|BackupError::Module(e.to_string()))?;
        let input=self.supervisor.artifacts().grant_blob(&artifact).map_err(|e|BackupError::Module(e.to_string()))?;
        let (response,receive)=mpsc::channel();
        self.sender.send(Work {method,input,modules:modules.clone(),response}).map_err(|_|BackupError::Module("backup codec stopped".into()))?;
        receive.recv().map_err(|_|BackupError::Module("backup codec response lost".into()))?
    }
    pub fn encode(&self,snapshot:&PortableSnapshot,modules:&BTreeMap<String,ModuleVersion>)->Result<(Vec<u8>,BackupResult),BackupError> {
        let (output,result)=self.invoke("encode",&serde_json::to_vec(snapshot)?,modules)?;
        let mut bytes=Vec::with_capacity(output.descriptor.size.try_into().map_err(|_|BackupError::Invalid("archive size overflow".into()))?);
        output.open_read().map_err(|e|BackupError::Module(e.to_string()))?.read_to_end(&mut bytes)?;
        Ok((bytes,result))
    }
    pub fn decode(&self,bytes:&[u8],modules:&BTreeMap<String,ModuleVersion>)->Result<(BackupManifest,PortableSnapshot),BackupError> {
        let (output,result)=self.invoke("decode",bytes,modules)?;
        let snapshot:PortableSnapshot=serde_json::from_reader(output.open_read().map_err(|e|BackupError::Module(e.to_string()))?)?;
        if snapshot.counts()!=result.counts {return Err(BackupError::Invalid("native decoded snapshot counts mismatch".into()));}
        Ok((result.manifest,snapshot))
    }
    pub fn validate(&self,bytes:&[u8],modules:&BTreeMap<String,ModuleVersion>)->Result<BackupResult,BackupError> {
        let (manifest,snapshot)=self.decode(bytes,modules)?;
        Ok(BackupResult {manifest,counts:snapshot.counts()})
    }
}

pub fn module_manifest(target:&str,host_api:&str,worker:ModuleFile)->ModuleManifest {
    ModuleManifest {schema_version:1,id:MODULE_ID.into(),version:env!("CARGO_PKG_VERSION").into(),publisher:"mintcat.official".into(),target:target.into(),host_api:host_api.into(),components:vec![ModuleComponent {kind:"service".into(),entry:worker.path.clone(),runtime:"worker".into(),state_model:"stateless".into()}],provides:vec![ProvidedContract {contract_id:BACKUP_CONTRACT_ID.into(),version:BACKUP_CONTRACT_VERSION,cardinality:Some("multi".into())}],requires:vec![],permissions:vec![PermissionRequest {capability:"artifact.read".into(),scope:"operation-grants".into()},PermissionRequest {capability:"staging.write".into(),scope:"operation-staging".into()}],contributes:json!({}),state:ModuleState {schema_version:1,readable_range:"1".into(),writable_range:"1".into(),rollback_before_write_only:Some(true)},activation:ModuleActivation {trigger:"on-demand".into(),drain_policy:"finish-requests".into()},files:vec![worker]}
}

pub fn execute_codec(request:&WorkerRequest,workspace:&Path)->Result<Value,BackupError> {
    if request.contract_id.as_deref()!=Some(BACKUP_CONTRACT_ID)||request.contract_version!=Some(BACKUP_CONTRACT_VERSION){return Err(BackupError::Invalid("incompatible backup codec contract".into()));}
    let params:CodecRequest=serde_json::from_value(request.params.clone())?;
    let input=ArtifactGrant::from_worker_request(request,&params.input_grant).map_err(|e|BackupError::Invalid(e.to_string()))?;
    let (bytes,result)=match request.method.as_deref() {
        Some("encode")=>{
            let snapshot:PortableSnapshot=serde_json::from_reader(input.open_read().map_err(|e|BackupError::Invalid(e.to_string()))?)?;
            crate::encode_archive(&snapshot,params.modules)?
        },
        Some("decode")=>{
            let mut bytes=Vec::with_capacity(input.descriptor.size.try_into().map_err(|_|BackupError::Invalid("archive size overflow".into()))?);
            input.open_read().map_err(|e|BackupError::Invalid(e.to_string()))?.read_to_end(&mut bytes)?;
            let (manifest,snapshot)=crate::decode(&bytes,&params.modules)?;
            let result=BackupResult {manifest,counts:snapshot.counts()};
            (serde_json::to_vec(&snapshot)?,result)
        },
        _=>return Err(BackupError::Invalid("unknown backup codec operation".into())),
    };
    let path=workspace.join(format!("backup-codec-{}.json",Uuid::new_v4()));
    let mut options=OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut output=options.open(&path)?;output.write_all(&bytes)?;output.sync_all()?;drop(output);
    let descriptor=ArtifactGrantDescriptor {grant_id:Uuid::new_v4().to_string(),digest:crate::digest(&bytes),size:bytes.len() as u64,path:path.to_string_lossy().into_owned(),access:ArtifactGrantAccess::Read};
    Ok(serde_json::to_value(CodecResponse {output:descriptor,result})?)
}

pub fn worker_workspace()->Result<PathBuf,BackupError> {
    let path=PathBuf::from(std::env::var_os(mintcat_sdk::WORKER_WORKSPACE_ENV).ok_or_else(||BackupError::Invalid("worker workspace absent".into()))?);
    let metadata=fs::symlink_metadata(&path)?;
    if !path.is_absolute()||!metadata.is_dir()||metadata.file_type().is_symlink(){return Err(BackupError::Invalid("worker workspace must be an absolute real directory".into()));}
    Ok(fs::canonicalize(path)?)
}
