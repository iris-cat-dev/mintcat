use std::{collections::BTreeSet, fs::OpenOptions, path::{Path, PathBuf}, sync::{Arc, Mutex}};
use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_runtime_kernel::{RuntimeLayout, RuntimePaths};
use mintcat_sdk::*;
use serde::{Serialize, Deserialize};
use serde_json::{json, Value};
use crate::*;
pub const LIBRARY_CONTRACT_ID:&str="library.owner";
pub const LIBRARY_CONTRACT_VERSION:u64=2;
/// Private transport envelope, built only by the trusted host facade. Application arguments
/// cannot replace host authorization, state paths, or authenticated grant descriptors.
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostCall { pub authorization:Option<LibraryAuthorization>, pub args:Value }
#[derive(Serialize,Deserialize)]
pub(crate) enum WireResult { Produced(ArtifactGrantDescriptor), Failure(WireError) }
#[derive(Serialize,Deserialize)]
pub(crate) struct WireError { kind:String, details:Value }
impl WireError {
    fn from_error(error:LibraryError)->Self {
        let (kind,details)=match error {
            LibraryError::ResourceBusy {owner_operation_id} => ("busy",json!(owner_operation_id)),
            LibraryError::RevisionConflict {expected,actual} => ("revision",json!([expected,actual])),
            LibraryError::StaleFence {expected,actual} => ("fence",json!([expected,actual])),
            LibraryError::NotFound(v)=>("not-found",json!(v)), LibraryError::PermissionDenied(v)=>("permission",json!(v)),
            LibraryError::UnsupportedSchema(v)=>("schema",json!(v)), LibraryError::StaleOperation=>("stale",Value::Null),
            LibraryError::IdempotencyConflict=>("idempotency",Value::Null), LibraryError::LibraryNotEmpty=>("not-empty",Value::Null),
            LibraryError::OperationState(v)=>("state",json!(v)),LibraryError::ArtifactNotVerified(v)=>("artifact",json!(v)),LibraryError::StaleTarget(v)=>("target",json!(v)),
            LibraryError::CredentialVault {provider,..}=>("credential",json!(provider)),LibraryError::CredentialCleanup {..}=>("invalid",json!("credential cleanup rejected")),
            other=>("invalid",json!(other.to_string())),
        };Self {kind:kind.into(),details}
    }
    pub(crate) fn into_error(self)->LibraryError { let text=||self.details.as_str().unwrap_or("owner error").to_owned();match self.kind.as_str() {
        "busy"=>LibraryError::ResourceBusy {owner_operation_id:text()},"revision"=>LibraryError::RevisionConflict {expected:self.details[0].as_u64().unwrap_or(0),actual:self.details[1].as_u64().unwrap_or(0)},"fence"=>LibraryError::StaleFence {expected:self.details[0].as_u64().unwrap_or(0),actual:self.details[1].as_u64().unwrap_or(0)},"not-found"=>LibraryError::NotFound(text()),"permission"=>LibraryError::PermissionDenied(text()),"schema"=>LibraryError::UnsupportedSchema(self.details.as_u64().unwrap_or(0) as u32),"stale"=>LibraryError::StaleOperation,"idempotency"=>LibraryError::IdempotencyConflict,"not-empty"=>LibraryError::LibraryNotEmpty,"state"=>LibraryError::OperationState(text()),"artifact"=>LibraryError::ArtifactNotVerified(text()),"target"=>LibraryError::StaleTarget(text()),"credential"=>LibraryError::CredentialVault {provider:text(),message:"credential vault rejected import".into()},_=>LibraryError::InvalidRecord(text()) } }
}
struct Owner { layout:RuntimeLayout, workspace:PathBuf, writer:Option<OwnerStore>, reader:Option<OwnerStore>, identity:crate::NativeOwnerIdentity, database_seen:bool }
impl Owner {
    fn invoke(&mut self, request:WorkerRequest, broker:WorkerBroker)->Result<Value,LibraryError> {
        if request.contract_id.as_deref()!=Some(LIBRARY_CONTRACT_ID) || request.contract_version!=Some(LIBRARY_CONTRACT_VERSION) {return Err(LibraryError::PermissionDenied("Library owner contract required".into()));}
        let method=request.method.as_deref().ok_or_else(|| LibraryError::InvalidRecord("method missing".into()))?;
        let params=if let Some(id)=request.params.get("inputGrant").and_then(Value::as_str) {let grant=ArtifactGrant::from_worker_request(&request,id).map_err(|_| LibraryError::ArtifactNotVerified(id.into()))?;serde_json::from_reader(grant.open_read().map_err(|e| LibraryError::InvalidRecord(e.to_string()))?)?}else{request.params.clone()};
        let call:HostCall=serde_json::from_value(params)?;
        let args=&call.args;
        let authorization=call.authorization.as_ref();
        let path=self.layout.state_dir.join("mintcat.library/library.sqlite");
        if self.database_seen && !path.exists() {
            self.reader=None;
            return Err(LibraryError::NotFound("Library owner database disappeared".into()));
        }
        match method {
            "owner.health" => {
                self.reader=None;
                if path.exists() {
                    if self.writer.is_some() { OwnerStore::read_only_health(&path,true)?; }
                    else { self.reader=Some(OwnerStore::open_read_only(&path)?); }
                    self.database_seen=true;
                }
                return produced(&self.workspace,&());
            },
            "owner.pause" => {self.reader=None;if let Some(writer)=&self.writer {writer.ensure_backup_quiescent()?;}self.writer=None;return produced(&self.workspace,&());},
            "owner.open_writes" => {
                self.reader=None;
                if self.writer.is_none() {
                    if path.exists() { OwnerStore::read_only_health(&path, true)?; }
                    self.writer=Some(OwnerStore::open(&self.layout)?);
                }
                self.database_seen=true;
                return produced(&self.workspace,&());
            },
            "owner.write_epoch" => return produced(&self.workspace, &crate::import_outcome::epoch(&path)?),
            "owner.identity" => return produced(&self.workspace, &self.identity),
            _=>{}
        }
        if method == "legacy_import_outcome" {
            let source_digest: String = arg(args, "source_digest")?;
            let result = if path.exists() { crate::import_outcome::lookup(&path, &source_digest)? } else { None };
            return produced(&self.workspace, &result);
        }
        if self.writer.is_none() && ["snapshot", "backup_snapshot", "ensure_backup_quiescent"].contains(&method) {
            let snapshot = if path.exists() { OwnerStore::paused_snapshot(&path)? } else { LibrarySnapshot::default() };
            return if method == "ensure_backup_quiescent" { produced(&self.workspace, &()) } else { produced(&self.workspace, &snapshot) };
        }
        // A healthy probation owner may serve only explicitly admitted queries. SQLite
        // additionally enforces read-only access; mutation dispatch never sees the reader.
        let read_only=read_method(method);
        let owner=self.writer.as_ref().or_else(|| if read_only {self.reader.as_ref()} else {None})
            .ok_or_else(|| LibraryError::ResourceBusy {owner_operation_id:"library-paused".into()})?;
        if !read_only && method != "import_legacy" { crate::import_outcome::advance(&path)?; }
        dispatch(owner,method,args,authorization,&request,&broker,&self.workspace)
    }
}
fn read_method(method: &str) -> bool {
    matches!(method, "snapshot" | "backup_snapshot" | "ensure_backup_quiescent" |
        "get_global_setting" | "get_game_setting" | "list_games" | "list_profiles" |
        "list_groups" | "list_mods" | "get_mod" | "validate_profile_content_guard" |
        "download_operation" | "download_execution_plan" | "profile_download_state" |
        "retained_artifact_digests" | "profile_content_capability" | "pending_outbox" |
        "source_add_target" | "source_add_result" | "registered_installation" | "get_settings")
}
fn produced<T:Serialize>(workspace:&Path,value:&T)->Result<Value,LibraryError> {
    let path=workspace.join(format!("{}.json",uuid::Uuid::new_v4()));
    let mut options=OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut writer=crate::digest::DigestWriter::new(options.open(&path)?);
    serde_json::to_writer(&mut writer,value)?;
    writer.inner.sync_all()?;
    let size=writer.inner.metadata()?.len();
    let (file,digest)=writer.finish();drop(file);
    Ok(serde_json::to_value(WireResult::Produced(ArtifactGrantDescriptor {grant_id:uuid::Uuid::new_v4().to_string(),path:path.to_string_lossy().into_owned(),digest,size,access:ArtifactGrantAccess::Read}))?)
}
fn failure(error:LibraryError)->Value { serde_json::to_value(WireResult::Failure(WireError::from_error(error))).expect("wire error is JSON") }
fn arg<T:serde::de::DeserializeOwned>(args:&Value,name:&str)->Result<T,LibraryError> {Ok(serde_json::from_value(args.get(name).cloned().ok_or_else(|| LibraryError::InvalidRecord(format!("missing argument {name}")))?)?)}
fn auth(value:Option<&LibraryAuthorization>)->Result<&LibraryAuthorization,LibraryError> {value.ok_or_else(|| LibraryError::PermissionDenied("host authorization missing".into()))}
struct ImportVault {broker:WorkerBroker,source_grant:String}
impl CredentialVault for ImportVault {
    fn store(&self,credential:LegacyCredential<'_>)->Result<String,String> {let value=self.broker.blocking_invoke(WorkerBrokerRequest {id:String::new(),provider_digest:"host".into(),contract_id:"library.credentials".into(),contract_version:1,method:"store".into(),params:json!({"ownerScope":credential.owner_scope,"provider":credential.provider,"secret":credential.secret}),grant_ids:vec![self.source_grant.clone()],produced:vec![]}).map_err(|_| "credential import denied".to_owned())?;value.as_str().map(str::to_owned).ok_or_else(|| "credential vault returned invalid reference".into())}
    fn delete(&self,credential_ref:&str)->Result<(),String> {self.broker.blocking_invoke(WorkerBrokerRequest {id:String::new(),provider_digest:"host".into(),contract_id:"library.credentials".into(),contract_version:1,method:"delete".into(),params:json!({"credentialRef":credential_ref}),grant_ids:vec![self.source_grant.clone()],produced:vec![]}).map(|_|()).map_err(|_| "credential cleanup denied".into())}
}
pub async fn run_native_owner()->Result<(),WorkerProtocolError> {
    let identity=WorkerIdentity::from_env()?;
    if identity.module_id!="mintcat.library" || !identity.provides.iter().any(|p|p.contract_id==LIBRARY_CONTRACT_ID && p.version==LIBRARY_CONTRACT_VERSION) {return Err(WorkerProtocolError::Rejected("Library identity required".into()));}
    let data_dir=std::env::var_os("MINTCAT_LIBRARY_DATA_DIR").ok_or_else(|| WorkerProtocolError::Environment("host Library data directory missing".into()))?;
    let layout=RuntimePaths::new(PathBuf::from(data_dir)).map_err(|e|WorkerProtocolError::Environment(e.to_string()))?.layout();
    let workspace=std::fs::canonicalize(std::env::var_os(WORKER_WORKSPACE_ENV).ok_or_else(||WorkerProtocolError::Environment("workspace missing".into()))?)?;
    let native_identity=crate::NativeOwnerIdentity { instance_id:identity.instance_id.clone(), module_version:identity.module_version.clone(), module_digest:identity.module_digest.clone(), process_id:std::process::id() };
    let database_seen=layout.state_dir.join("mintcat.library/library.sqlite").exists() || layout.state_dir.join("mintcat.library/writer.lock").exists();
    let owner=Arc::new(Mutex::new(Owner {layout,workspace,writer:None,reader:None,identity:native_identity,database_seen}));
    run_worker_with_broker(identity,move |request,broker| {let owner=owner.clone();async move {tokio::task::spawn_blocking(move || {let mut owner=owner.lock().map_err(|_|worker_app_error(&request.id,"INTERNAL","library.owner.poisoned",Value::Null))?;Ok(match owner.invoke(request,broker) {Ok(value)=>value,Err(error)=>failure(error)})}).await.map_err(|_|worker_app_error("owner","INTERNAL","library.owner.failed",Value::Null))?}}).await
}
include!("native_dispatch.rs");
