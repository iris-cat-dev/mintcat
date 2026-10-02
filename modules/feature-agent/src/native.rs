use std::{fs::{File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_sdk::*;
use serde::{Serialize, Deserialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use crate::*;
use crate::AgentConfiguration;

pub const AGENT_CONTRACT_ID: &str = "agent.owner";
pub const AGENT_CONTRACT_VERSION: u64 = 1;
pub(crate) const MAX_PRIVATE_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
pub(crate) enum WireResult { Produced(ArtifactGrantDescriptor), Failure(AgentError) }
fn storage<T>(result: std::io::Result<T>) -> Result<T, AgentError> { result.map_err(|_| AgentError::Storage) }
fn private_file(path: &Path) -> Result<File, AgentError> {
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    storage(options.open(path))
}
pub(crate) fn descriptor(path: &Path) -> Result<ArtifactGrantDescriptor, AgentError> {
    let mut file = storage(File::open(path))?;
    let size = storage(file.metadata())?.len();
    let mut hash = Sha256::new(); let mut buffer = [0; 65536];
    loop { let count = storage(file.read(&mut buffer))?; if count == 0 { break; } hash.update(&buffer[..count]); }
    Ok(ArtifactGrantDescriptor { grant_id:Uuid::new_v4().to_string(), path:path.to_string_lossy().into_owned(), digest:format!("sha256:{:x}",hash.finalize()), size, access:ArtifactGrantAccess::Read })
}
fn produced_file<T: Serialize>(workspace: &Path, value: &T) -> Result<ArtifactGrantDescriptor, AgentError> {
    let path = workspace.join(format!("{}.json",Uuid::new_v4()));
    let mut file = private_file(&path)?;
    serde_json::to_writer(&mut file, value).map_err(|_| AgentError::Storage)?;
    storage(file.sync_all())?; drop(file);
    let grant=descriptor(&path)?;
    if grant.size>MAX_PRIVATE_BYTES { let _=std::fs::remove_file(&path);return Err(AgentError::TurnLimit); }
    Ok(grant)
}
fn produced<T: Serialize>(workspace: &Path, value: &T) -> Result<Value, AgentError> {
    serde_json::to_value(WireResult::Produced(produced_file(workspace,value)?)).map_err(|_| AgentError::Storage)
}
fn arg<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, AgentError> {
    serde_json::from_value(args.get(name).cloned().ok_or(AgentError::InvalidRequest)?).map_err(|_| AgentError::InvalidRequest)
}
fn from_host<T: DeserializeOwned>(value: Value) -> Result<T, AgentError> {
    let grant:ArtifactGrantDescriptor = serde_json::from_value(value).map_err(|_| AgentError::InvalidResponse)?;
    if grant.size > MAX_PRIVATE_BYTES { return Err(AgentError::TurnLimit); }
    let request = WorkerRequest { id:String::new(), kind:WorkerRequestKind::Invoke, contract_id:None, contract_version:None, method:None, params:Value::Null, grants:vec![grant.clone()] };
    let grant = ArtifactGrant::from_worker_request(&request,&grant.grant_id).map_err(|_| AgentError::InvalidResponse)?;
    serde_json::from_reader(grant.open_read().map_err(|_| AgentError::InvalidResponse)?).map_err(|_| AgentError::InvalidResponse)
}
struct HostCallbacks { broker:WorkerBroker, workspace:PathBuf }
impl HostCallbacks {
    async fn call<T: DeserializeOwned>(&self, method:&str, value:&Value) -> Result<T, AgentError> {
        let grant = produced_file(&self.workspace,value)?;
        let path = grant.path.clone();
        let outcome = self.broker.invoke(WorkerBrokerRequest { id:String::new(), provider_digest:"host".into(), contract_id:"agent.host".into(), contract_version:1, method:method.into(), params:Value::Null, grant_ids:vec![], produced:vec![grant] }).await;
        let _ = std::fs::remove_file(path);
        from_host(outcome.map_err(|_| AgentError::Provider)?)
    }
}
#[async_trait]
impl CompletionProvider for HostCallbacks {
    async fn complete(&self, _configuration:&AgentConfiguration, body:Vec<u8>) -> Result<Value, AgentError> {
        let body:Value = serde_json::from_slice(&body).map_err(|_| AgentError::InvalidRequest)?;
        self.call("completion",&body).await
    }
}
#[async_trait]
impl ApplicationGateway for HostCallbacks {
    async fn execute(&self, request:ValidatedRequest) -> Result<Value,AppError> {
        let envelope = serde_json::to_value(request.envelope).map_err(|_| *worker_app_error("tool","INVALID_ARGUMENT","agent.tool.invalid",Value::Null))?;
        let result:Value = self.call("tool",&envelope).await.map_err(|_| *worker_app_error("tool","APPLICATION_REJECTED","agent.tool.rejected",Value::Null))?;
        if result["ok"].as_bool() == Some(true) { Ok(result["data"].clone()) } else {
            Err(*worker_app_error("tool",result["code"].as_str().unwrap_or("APPLICATION_REJECTED"),"agent.tool.rejected",Value::Null))
        }
    }
}
struct Owner { path:PathBuf, workspace:PathBuf, writer:Option<AgentSessions>, read_only:bool, _lock:Option<File> }
impl Owner {
    fn epoch_path(&self) -> PathBuf { self.path.with_extension("write-epoch") }
    fn epoch(&self) -> Result<u64,AgentError> {
        let path=self.epoch_path();
        match std::fs::symlink_metadata(&path) {
            Ok(metadata)=>{if !metadata.is_file() || metadata.file_type().is_symlink() {return Err(AgentError::Storage);}},
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(0),
            Err(_)=>return Err(AgentError::Storage)
        }
        std::fs::read_to_string(path).map_err(|_|AgentError::Storage)?.parse().map_err(|_|AgentError::Storage)
    }
    fn advance_epoch(&self) -> Result<(),AgentError> {
        let epoch=self.epoch()?.checked_add(1).ok_or(AgentError::Storage)?;
        let parent=self.path.parent().ok_or(AgentError::Storage)?;
        let temporary=parent.join(format!(".agent-epoch-{}",Uuid::new_v4()));
        let mut file=private_file(&temporary)?;storage(write!(file,"{epoch}"))?;storage(file.sync_all())?;drop(file);
        storage(std::fs::rename(temporary,self.epoch_path()))?;storage(storage(File::open(parent))?.sync_all())
    }
    fn readonly(&self) -> Result<Option<Connection>,AgentError> {
        match std::fs::symlink_metadata(&self.path) {Ok(_)=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(_)=>return Err(AgentError::Storage)}
        read_database(&self.path).map(Some)
    }
    async fn invoke(&mut self, request:WorkerRequest, broker:WorkerBroker) -> Result<Value,AgentError> {
        if request.contract_id.as_deref()!=Some(AGENT_CONTRACT_ID) || request.contract_version!=Some(1) { return Err(AgentError::PermissionDenied); }
        let method=request.method.as_deref().ok_or(AgentError::InvalidRequest)?;
        if self.read_only && !matches!(method,"get"|"has_nonempty"|"owner.health"|"owner.write_epoch") {return Err(AgentError::PermissionDenied);}
        let args=if let Some(id)=request.params.get("inputGrant").and_then(Value::as_str) {
            let grant=ArtifactGrant::from_worker_request(&request,id).map_err(|_|AgentError::InvalidRequest)?;
            if grant.descriptor.size>MAX_PRIVATE_BYTES { return Err(AgentError::TurnLimit); }
            serde_json::from_reader(grant.open_read().map_err(|_|AgentError::Storage)?).map_err(|_|AgentError::InvalidRequest)?
        } else { request.params.clone() };
        match method {
            "owner.health" => { self.readonly()?; return produced(&self.workspace,&()); },
            "owner.pause" => { self.writer=None; return produced(&self.workspace,&()); },
            "owner.write_epoch" => return produced(&self.workspace,&self.epoch()?),
            "owner.open_writes" => {
                if self.writer.is_none() {
                    let initialize=if let Some(db)=self.readonly()? {
                        let interrupted:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE json_extract(data,'$.session.state')='sending')",[],|row|row.get(0)).map_err(|_|AgentError::Storage)?;
                        let version:u64=db.query_row("PRAGMA user_version",[],|row|row.get(0)).map_err(|_|AgentError::Storage)?;
                        if interrupted || version==0 { self.advance_epoch()?; }
                        version==0
                    } else {
                        self.advance_epoch()?;
                        drop(private_file(&self.path)?);
                        true
                    };
                    let callbacks=Arc::new(HostCallbacks {broker,workspace:self.workspace.clone()});
                    let writer=AgentSessions::open_with_completion(&self.path,callbacks.clone(),callbacks)?;
                    if initialize {writer.db.lock().map_err(|_|AgentError::Storage)?.execute_batch("PRAGMA user_version=1;").map_err(|_|AgentError::Storage)?;}
                    self.writer=Some(writer);
                }
                return produced(&self.workspace,&());
            },
            "owner.portable_snapshot" => {
                let mut sessions = Vec::new();
                if let Some(mut db) = self.readonly()? {
                    let tx = db.transaction().map_err(|_|AgentError::Storage)?;
                    let mut query = tx.prepare("SELECT data FROM agent_sessions ORDER BY session_id").map_err(|_|AgentError::Storage)?;
                    let rows = query.query_map([],|row|row.get::<_,String>(0)).map_err(|_|AgentError::Storage)?;
                    for row in rows {
                        let mut stored:crate::StoredSession=serde_json::from_str(&row.map_err(|_|AgentError::Storage)?).map_err(|_|AgentError::Storage)?;
                        if stored.session.state=="sending" {return Err(AgentError::Busy);}
                        stored.session.configuration.credential_ref=None;
                        if !stored.session.bindings.is_empty() {
                            stored.session.timeline.push(crate::TimelineItem {
                                id:format!("imported-task-history:{}",stored.session.session_id),
                                role:"tool".into(),
                                content:format!("Non-authoritative task history; these IDs do not authorize tasks on a restored machine: {}",serde_json::to_string(&stored.session.bindings).map_err(|_|AgentError::Storage)?),
                                tool_name:None,
                            });
                            stored.session.bindings.clear();
                        }
                        sessions.push(stored.session);
                    }
                }
                return produced(&self.workspace,&sessions);
            },
            "owner.import_empty_portable" | "owner.validate_portable" => {
                if method=="owner.import_empty_portable" && self.epoch()?!=arg::<u64>(&args,"expectedEpoch")? {return Err(AgentError::Busy);}
                let sessions:Vec<crate::AgentSession>=arg(&args,"sessions")?;
                let mut ids=std::collections::BTreeSet::new();
                for session in &sessions {
                    if !ids.insert(&session.session_id) || session.revision==0 || !["idle","failed"].contains(&session.state.as_str())
                        || session.configuration.credential_ref.is_some() || !session.bindings.is_empty() {
                        return Err(AgentError::InvalidRequest);
                    }
                    crate::validate_configuration_target(&session.configuration,&session.target)?;
                }
                if method=="owner.validate_portable" {return produced(&self.workspace,&());}
                let writer=self.writer.as_ref().ok_or(AgentError::Busy)?;
                let mut db=writer.db.lock().map_err(|_|AgentError::Storage)?;
                let tx=db.transaction().map_err(|_|AgentError::Storage)?;
                let nonempty:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_sessions) OR EXISTS(SELECT 1 FROM agent_operations)",[],|row|row.get(0)).map_err(|_|AgentError::Storage)?;
                if nonempty {return Err(AgentError::Busy);}
                // Advance the durable fence before commit; ambiguous I/O cannot permit
                // an earlier owner snapshot to overwrite this new-machine import.
                self.advance_epoch()?;
                for session in sessions {
                    let history=session.timeline.iter().filter(|item|["user","assistant"].contains(&item.role.as_str()) && item.tool_name.is_none())
                        .map(|item|json!({"role":item.role,"content":item.content})).collect();
                    crate::persist(&tx,&crate::StoredSession {session,history})?;
                }
                tx.commit().map_err(|_|AgentError::Storage)?;
                return produced(&self.workspace,&());
            },
            "owner.backup" => {
                if self.writer.is_some() { return Err(AgentError::Busy); }
                let path=self.workspace.join(format!("{}.sqlite",Uuid::new_v4()));
                let db=match self.readonly()? { Some(db)=>db, None=>return Err(AgentError::Storage) };
                drop(private_file(&path)?);
                let mut target=Connection::open(&path).map_err(|_|AgentError::Storage)?;
                let backup=rusqlite::backup::Backup::new(&db,&mut target).map_err(|_|AgentError::Storage)?;
                backup.run_to_completion(128,Duration::from_millis(10),None).map_err(|_|AgentError::Storage)?;
                drop(backup);drop(target);storage(storage(File::open(&path))?.sync_all())?;
                return produced(&self.workspace,&descriptor(&path)?);
            },
            "owner.restore" => {
                if self.writer.is_some() || self.epoch()?!=arg::<u64>(&args,"expectedEpoch")? { return Err(AgentError::Busy); }
                let id:String=arg(&args,"sourceGrant")?;
                let grant=ArtifactGrant::from_worker_request(&request,&id).map_err(|_|AgentError::InvalidRequest)?;
                read_database(Path::new(&grant.descriptor.path))?;
                let temporary=self.path.with_extension(format!("restore-{}",Uuid::new_v4()));
                let mut output=private_file(&temporary)?;
                let mut input=grant.open_read().map_err(|_|AgentError::Storage)?;
                storage(std::io::copy(&mut input,&mut output))?;storage(output.sync_all())?;drop(output);
                for suffix in ["-wal","-shm"] { let sidecar=PathBuf::from(format!("{}{suffix}",self.path.display())); if sidecar.exists() { storage(std::fs::remove_file(sidecar))?; } }
                storage(std::fs::rename(temporary,&self.path))?;storage(storage(File::open(self.path.parent().ok_or(AgentError::Storage)?))?.sync_all())?;
                return produced(&self.workspace,&());
            },
            "get" => {
                let id:String=arg(&args,"sessionId")?;
                let session=if let Some(writer)=&self.writer { writer.get(&id)? } else { load(&self.readonly()?.ok_or(AgentError::NotFound)?,&id)?.session };
                return produced(&self.workspace,&session);
            },
            "has_nonempty" => {
                let result=if let Some(db)=self.readonly()? { db.query_row("SELECT EXISTS(SELECT 1 FROM agent_sessions) OR EXISTS(SELECT 1 FROM agent_operations)",[],|row|row.get::<_,bool>(0)).map_err(|_|AgentError::Storage)? } else { false };
                return produced(&self.workspace,&result);
            },
            _=>{}
        }
        if self.writer.is_none() { return Err(AgentError::Busy); }
        if matches!(method,"create_keyed"|"delete_keyed") {
            let command=if method=="create_keyed" {"create"}else{"delete"};
            let owner=self.writer.as_ref().ok_or(AgentError::Busy)?;
            let db=owner.db.lock().map_err(|_|AgentError::Storage)?;
            if let Some(result)=replay_operation(&db,command,&arg::<String>(&args,"key")?,&arg::<String>(&args,"hash")?)? {
                let result:Value=serde_json::from_str(&result).map_err(|_|AgentError::Storage)?;
                return produced(&self.workspace,&result);
            }
        }
        self.advance_epoch()?;
        // Each invocation gets the SDK callback channel belonging to that invocation.
        let writer=self.writer.as_mut().ok_or(AgentError::Busy)?;
        let callbacks=Arc::new(HostCallbacks {broker,workspace:self.workspace.clone()});
        writer.completion=callbacks.clone();writer.gateway=callbacks;
        match method {
            "create_keyed"=>produced(&self.workspace,&writer.create_keyed(arg(&args,"request")?,&arg::<String>(&args,"key")?,&arg::<String>(&args,"hash")?)?),
            "delete_keyed"=>{writer.delete_keyed(&arg::<String>(&args,"sessionId")?,arg(&args,"revision")?,&arg::<String>(&args,"key")?,&arg::<String>(&args,"hash")?)?;produced(&self.workspace,&())},
            "prepare_send"=>{writer.prepare_send(arg(&args,"request")?,&arg::<String>(&args,"jobId")?)?;produced(&self.workspace,&())},
            "run_turn"=>produced(&self.workspace,&writer.run_turn(&arg::<String>(&args,"sessionId")?,&arg::<String>(&args,"jobId")?,&arg::<String>(&args,"generation")?).await?),
            "interrupt_turn"=>{writer.interrupt_turn(&arg::<String>(&args,"sessionId")?,&arg::<String>(&args,"jobId")?)?;produced(&self.workspace,&())},
            _=>Err(AgentError::InvalidRequest)
        }
    }
}
fn read_database(path:&Path) -> Result<Connection,AgentError> {
    let metadata=storage(std::fs::symlink_metadata(path))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(AgentError::Storage); }
    let db=Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_|AgentError::Storage)?;
    let version:u64=db.query_row("PRAGMA user_version",[],|row|row.get(0)).map_err(|_|AgentError::Storage)?;
    if version>1 { return Err(AgentError::Storage); }
    let check:String=db.query_row("PRAGMA quick_check",[],|row|row.get(0)).map_err(|_|AgentError::Storage)?;
    if check!="ok" { return Err(AgentError::Storage); }
    db.prepare("SELECT session_id,data FROM agent_sessions LIMIT 0").map_err(|_|AgentError::Storage)?;
    db.prepare("SELECT command,operation_key,request_hash,result FROM agent_operations LIMIT 0").map_err(|_|AgentError::Storage)?;
    Ok(db)
}
pub async fn run_native_owner() -> Result<(),WorkerProtocolError> {
    let identity=WorkerIdentity::from_env()?;
    if identity.module_id!="mintcat.feature.agent" || !identity.provides.iter().any(|p|p.contract_id==AGENT_CONTRACT_ID && p.version==1) { return Err(WorkerProtocolError::Rejected("Agent owner identity required".into())); }
    let path=PathBuf::from(std::env::var_os("MINTCAT_AGENT_DATABASE").ok_or_else(||WorkerProtocolError::Environment("Agent database missing".into()))?);
    let workspace=std::fs::canonicalize(std::env::var_os(WORKER_WORKSPACE_ENV).ok_or_else(||WorkerProtocolError::Environment("workspace missing".into()))?)?;
    let read_only=std::env::var("MINTCAT_AGENT_READ_ONLY").as_deref()==Ok("1");
    let lock=if read_only {None}else{
        let lock_path=path.with_extension("writer-lock");
        if let Ok(metadata)=std::fs::symlink_metadata(&lock_path) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {return Err(WorkerProtocolError::Rejected("Agent lock invalid".into()));}
        }
        let mut options=OpenOptions::new();options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let lock=options.open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_|WorkerProtocolError::Rejected("Agent writer already active".into()))?;
        Some(lock)
    };
    let owner=Arc::new(Mutex::new(Owner {path,workspace,writer:None,read_only,_lock:lock}));
    run_worker_with_broker(identity,move |request,broker| {
        let owner=owner.clone();
        async move { tokio::task::spawn_blocking(move || {
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_|worker_app_error("owner","INTERNAL","agent.owner.failed",Value::Null))?;
            let mut owner=owner.lock().map_err(|_|worker_app_error("owner","INTERNAL","agent.owner.failed",Value::Null))?;
            let result=runtime.block_on(owner.invoke(request,broker));
            Ok(match result {Ok(value)=>value,Err(error)=>serde_json::to_value(WireResult::Failure(error)).expect("typed Agent error")})
        }).await.map_err(|_|worker_app_error("owner","INTERNAL","agent.owner.failed",Value::Null))? }
    }).await
}
