use std::{fs::{File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, process::{Child, ChildStdin, ChildStdout, Command, Stdio}, sync::{Arc, Mutex, Condvar, mpsc}, time::Duration};
use mintcat_runtime_artifacts::{ArtifactStore, ArtifactGrant, ModuleLease};
use mintcat_sdk::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{json, Value};
use crate::*;
use crate::AgentSession;
use crate::native::{WireResult, MAX_PRIVATE_BYTES};

struct Work { request:WorkerRequest, context:Option<TurnContext>, result:mpsc::Sender<Result<Value,AgentError>> }
enum CommandMessage { Invoke(Work), Stop }
struct Remote { sender:mpsc::Sender<CommandMessage>, join:Option<std::thread::JoinHandle<()>>, artifacts:ArtifactStore, workspace:PathBuf, digest:String }
impl Drop for Remote {
    fn drop(&mut self) {
        let _=self.sender.send(CommandMessage::Stop);
        if let Some(join)=self.join.take() {let _=join.join();}
        // The directory is a host-created UUID workspace owned only by this process.
        let _=std::fs::remove_dir_all(&self.workspace);
    }
}
#[derive(Clone)]
struct TurnContext { session:AgentSession, generation:String, cancellation:CancellationToken }
struct Active { session_id:String, job_id:String, cancellation:CancellationToken, started:bool }
struct Inner { remote:Mutex<Option<Remote>>, query:Mutex<Option<Remote>>, active:Mutex<Option<Active>>, finished:Condvar, path:PathBuf, broker:NetworkBroker, gateway:Arc<dyn ApplicationGateway> }
#[derive(Clone)]
pub struct NativeAgentSessions { inner:Arc<Inner> }
impl NativeAgentSessions {
    pub fn unavailable(path:&Path, broker:NetworkBroker, gateway:Arc<dyn ApplicationGateway>) -> Self {
        Self {inner:Arc::new(Inner {remote:Mutex::new(None),query:Mutex::new(None),active:Mutex::new(None),finished:Condvar::new(),path:path.to_path_buf(),broker,gateway})}
    }
    pub fn database_path(&self)->&Path { &self.inner.path }
    pub fn owner_digest(&self)->Result<String,AgentError> { self.inner.remote.lock().map_err(|_|AgentError::Storage)?.as_ref().map(|remote|remote.digest.clone()).ok_or(AgentError::ModuleUnavailable) }
    pub fn write_epoch(&self)->Result<u64,AgentError> { self.call("owner.write_epoch",json!({}),vec![],None) }
    pub fn has_native(&self)->Result<bool,AgentError> { Ok(self.inner.remote.lock().map_err(|_|AgentError::Storage)?.is_some()) }
    /// State absence is independent of a selected, paused native owner. First-install
    /// preparation starts that owner before taking its durable absence snapshot.
    pub fn is_pristine_absent(&self)->Result<bool,AgentError> {
        for path in [self.inner.path.clone(),PathBuf::from(format!("{}-wal",self.inner.path.display())),PathBuf::from(format!("{}-shm",self.inner.path.display()))] {
            match std::fs::symlink_metadata(path) {Ok(_)=>return Ok(false),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},Err(_)=>return Err(AgentError::Storage)}
        }
        let epoch_path=self.inner.path.with_extension("write-epoch");
        match std::fs::symlink_metadata(&epoch_path) {
            Ok(metadata)=>{
                if !metadata.is_file() || metadata.file_type().is_symlink() {return Err(AgentError::Storage);}
                let epoch:u64=std::fs::read_to_string(epoch_path).map_err(|_|AgentError::Storage)?.parse().map_err(|_|AgentError::Storage)?;
                Ok(epoch==0)
            },
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(true),
            Err(_)=>Err(AgentError::Storage)
        }
    }
    fn idle(&self)->Result<(),AgentError> { if self.inner.active.lock().map_err(|_|AgentError::Storage)?.is_some() {Err(AgentError::Busy)}else{Ok(())} }
    pub fn pause(&self)->Result<(),AgentError> { self.idle()?;self.call("owner.pause",json!({}),vec![],None) }
    pub fn read_only_health(&self)->Result<(),AgentError> { self.call("owner.health",json!({}),vec![],None) }
    pub fn open_writes(&self)->Result<(),AgentError> { self.call("owner.open_writes",json!({}),vec![],None) }
    pub fn portable_snapshot(&self)->Result<Vec<AgentSession>,AgentError> {
        self.idle()?;
        if !self.has_native()? && self.is_pristine_absent()? { return Ok(Vec::new()); }
        self.call("owner.portable_snapshot",json!({}),vec![],None)
    }
    pub fn validate_portable(&self,sessions:&[AgentSession])->Result<(),AgentError> {
        self.call("owner.validate_portable",json!({"sessions":sessions}),vec![],None)
    }
    pub fn import_empty_portable(&self,sessions:&[AgentSession],expected_epoch:u64)->Result<(),AgentError> {
        self.idle()?;
        self.call("owner.import_empty_portable",json!({"sessions":sessions,"expectedEpoch":expected_epoch}),vec![],None)
    }
    /// Reap both native processes without discarding their durable state.
    pub fn shutdown(&self)->Result<(),AgentError> {
        let active=self.inner.active.lock().map_err(|_|AgentError::Storage)?;
        if active.is_some() {return Err(AgentError::Busy);}
        let mut writer=self.inner.remote.lock().map_err(|_|AgentError::Storage)?;
        let mut query=self.inner.query.lock().map_err(|_|AgentError::Storage)?;
        *query=None;*writer=None;
        Ok(())
    }
    /// Roll back an absent predecessor by pausing and deselecting candidate code,
    /// never by deleting or resetting any owner state or its restoration fence.
    pub fn deactivate_native(&self)->Result<(),AgentError> {
        if self.has_native()? {self.pause()?;}
        self.shutdown()
    }
    pub fn replace_native(&self,artifacts:ArtifactStore,digest:&str)->Result<(),AgentError> {
        self.idle()?;
        let mut slot=self.inner.remote.lock().map_err(|_|AgentError::Storage)?;
        if slot.as_ref().is_some_and(|remote|remote.digest==digest) {return Ok(());}
        // The previous process must exit and release its lock and module lease before replacement.
        let mut query=self.inner.query.lock().map_err(|_|AgentError::Storage)?;
        *query=None;*slot=None;
        let writer=spawn_owner(artifacts.clone(),digest,&self.inner.path,self.inner.broker.clone(),self.inner.gateway.clone(),false)?;
        let reader=spawn_owner(artifacts,digest,&self.inner.path,self.inner.broker.clone(),self.inner.gateway.clone(),true)?;
        *slot=Some(writer);*query=Some(reader);
        Ok(())
    }
    pub fn create_keyed(&self,request:CreateSession,key:&str,hash:&str)->Result<AgentSession,AgentError> {self.call("create_keyed",json!({"request":request,"key":key,"hash":hash}),vec![],None)}
    pub fn delete_keyed(&self,id:&str,revision:u64,key:&str,hash:&str)->Result<(),AgentError> {self.call("delete_keyed",json!({"sessionId":id,"revision":revision,"key":key,"hash":hash}),vec![],None)}
    pub fn get(&self,id:&str)->Result<AgentSession,AgentError> {self.call("get",json!({"sessionId":id}),vec![],None)}
    pub fn has_nonempty(&self)->Result<bool,AgentError> {
        let connected=self.has_native()?;
        if !connected {
            return if self.is_pristine_absent()? {Ok(false)}else{Err(AgentError::ModuleUnavailable)};
        }
        self.call("has_nonempty",json!({}),vec![],None)
    }
    pub fn prepare_send(&self,request:SendMessage,job:&str)->Result<(),AgentError> {
        let mut active=self.inner.active.lock().map_err(|_|AgentError::Storage)?;
        if active.is_some() {return Err(AgentError::Busy);}
        *active=Some(Active {session_id:request.session_id.clone(),job_id:job.into(),cancellation:CancellationToken::default(),started:false});
        // Keep the reservation locked until the owner has durably accepted or rejected it.
        let result=self.call("prepare_send",json!({"request":request,"jobId":job}),vec![],None);
        if result.is_err() {*active=None;self.inner.finished.notify_all();}
        result
    }
    pub async fn run_turn(&self,id:&str,job:&str,generation:&str)->Result<AgentSession,AgentError> {
        let token={
            let mut active=self.inner.active.lock().map_err(|_|AgentError::Storage)?;
            let turn=active.as_mut().filter(|turn|turn.session_id==id && turn.job_id==job && !turn.started).ok_or(AgentError::Provider)?;
            if turn.cancellation.is_cancelled() {return Err(AgentError::Provider);}
            turn.started=true;turn.cancellation.clone()
        };
        let facade=self.clone();let args=json!({"sessionId":id,"jobId":job,"generation":generation});
        let id=id.to_owned();let generation=generation.to_owned();
        tokio::task::spawn_blocking(move || {
            let result=match facade.get(&id) {
                Ok(session)=>facade.call("run_turn",args,vec![],Some(TurnContext {session,generation,cancellation:token})),
                Err(error)=>{let _=facade.call::<()>("interrupt_turn",args,vec![],None);Err(error)}
            };
            if let Ok(mut active)=facade.inner.active.lock() {*active=None;facade.inner.finished.notify_all();}
            result
        }).await.map_err(|_|AgentError::ModuleUnavailable)?
    }
    /// Cancel the host provider, then wait for the child to persist a terminal turn.
    /// Never queue an interrupt behind a provider that cannot finish until cancellation.
    pub fn interrupt_turn(&self,id:&str,job:&str)->Result<(),AgentError> {
        let mut active=self.inner.active.lock().map_err(|_|AgentError::Storage)?;
        if active.as_ref().is_some_and(|turn|turn.session_id==id && turn.job_id==job) {
            active.as_ref().unwrap().cancellation.cancel();
            if !active.as_ref().unwrap().started {
                // A reserved job may never have been polled. Complete its interruption in
                // the actual owner now rather than waiting for an unscheduled future.
                *active=None;self.inner.finished.notify_all();drop(active);
                return self.call("interrupt_turn",json!({"sessionId":id,"jobId":job}),vec![],None);
            }
            while active.as_ref().is_some_and(|turn|turn.session_id==id && turn.job_id==job) {
                active=self.inner.finished.wait(active).map_err(|_|AgentError::Storage)?;
            }
        }
        drop(active);
        self.call("interrupt_turn",json!({"sessionId":id,"jobId":job}),vec![],None)
    }
    pub fn backup_to(&self,target:&Path)->Result<(),AgentError> {
        self.idle()?;
        let descriptor:ArtifactGrantDescriptor=self.call("owner.backup",json!({}),vec![],None)?;
        let slot=self.inner.remote.lock().map_err(|_|AgentError::Storage)?;
        let remote=slot.as_ref().ok_or(AgentError::ModuleUnavailable)?;
        let grant=remote.artifacts.grant_produced(&remote.workspace,&descriptor).map_err(|_|AgentError::Storage)?;
        let mut input=grant.open_read().map_err(|_|AgentError::Storage)?;
        let mut options=OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        let mut output=options.open(target).map_err(|_|AgentError::Storage)?;
        std::io::copy(&mut input,&mut output).map_err(|_|AgentError::Storage)?;output.sync_all().map_err(|_|AgentError::Storage)?;
        File::open(target.parent().ok_or(AgentError::Storage)?).and_then(|file|file.sync_all()).map_err(|_|AgentError::Storage)?;
        std::fs::remove_file(&descriptor.path).map_err(|_|AgentError::Storage)
    }
    pub fn restore_from(&self,source:&Path,expected_epoch:u64)->Result<(),AgentError> {
        self.idle()?;
        let artifacts=self.inner.remote.lock().map_err(|_|AgentError::Storage)?.as_ref().ok_or(AgentError::ModuleUnavailable)?.artifacts.clone();
        let metadata=std::fs::symlink_metadata(source).map_err(|_|AgentError::Storage)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {return Err(AgentError::Storage);}
        let mut input=File::open(source).map_err(|_|AgentError::Storage)?;
        let artifact=artifacts.publish_blob_reader(&mut input,Some("application/vnd.sqlite3".into())).map_err(|_|AgentError::Storage)?;
        let grant=artifacts.grant_blob(&artifact).map_err(|_|AgentError::Storage)?;
        self.call("owner.restore",json!({"sourceGrant":grant.descriptor.grant_id,"expectedEpoch":expected_epoch}),vec![grant],None)
    }
    fn call<T:DeserializeOwned>(&self,method:&str,mut args:Value,mut grants:Vec<ArtifactGrant>,context:Option<TurnContext>)->Result<T,AgentError> {
        let slot=if matches!(method,"get"|"has_nonempty") {self.inner.query.lock()}else{self.inner.remote.lock()}.map_err(|_|AgentError::Storage)?;
        let remote=slot.as_ref().ok_or(AgentError::ModuleUnavailable)?;
        let bytes=serde_json::to_vec(&args).map_err(|_|AgentError::InvalidRequest)?;
        if bytes.len() as u64>MAX_PRIVATE_BYTES {return Err(AgentError::TurnLimit);}
        if bytes.len()>MAX_WORKER_FRAME_BYTES/2 {
            let artifact=remote.artifacts.publish_blob_reader(&mut std::io::Cursor::new(bytes),Some("application/json".into())).map_err(|_|AgentError::Storage)?;
            let grant=remote.artifacts.grant_blob(&artifact).map_err(|_|AgentError::Storage)?;
            args=json!({"inputGrant":grant.descriptor.grant_id});grants.push(grant);
        }
        let request=WorkerRequest {id:Uuid::new_v4().to_string(),kind:WorkerRequestKind::Invoke,contract_id:Some(AGENT_CONTRACT_ID.into()),contract_version:Some(1),method:Some(method.into()),params:args,grants:grants.iter().map(|grant|grant.descriptor.clone()).collect()};
        let (send,receive)=mpsc::channel();
        remote.sender.send(CommandMessage::Invoke(Work {request,context,result:send})).map_err(|_|AgentError::ModuleUnavailable)?;
        let artifacts=remote.artifacts.clone();let workspace=remote.workspace.clone();drop(slot);
        let value=receive.recv().map_err(|_|AgentError::ModuleUnavailable)??;
        let result:WireResult=serde_json::from_value(value).map_err(|_|AgentError::InvalidResponse)?;
        match result {
            WireResult::Failure(error)=>Err(error),
            WireResult::Produced(descriptor)=> {
                if descriptor.size>MAX_PRIVATE_BYTES {return Err(AgentError::TurnLimit);}
                let grant=artifacts.grant_produced(&workspace,&descriptor).map_err(|_|AgentError::InvalidResponse)?;
                let value=serde_json::from_reader(grant.open_read().map_err(|_|AgentError::InvalidResponse)?).map_err(|_|AgentError::InvalidResponse);
                let _=std::fs::remove_file(&descriptor.path);value
            }
        }
    }
}
fn write_frame<T:Serialize>(output:&mut impl Write,value:&T)->Result<(),AgentError> {
    let bytes=serde_json::to_vec(value).map_err(|_|AgentError::InvalidRequest)?;
    if bytes.len()>MAX_WORKER_FRAME_BYTES {return Err(AgentError::TurnLimit);}
    output.write_all(&(bytes.len() as u32).to_be_bytes()).and_then(|_|output.write_all(&bytes)).and_then(|_|output.flush()).map_err(|_|AgentError::ModuleUnavailable)
}
fn read_frame<T:DeserializeOwned>(input:&mut impl Read)->Result<T,AgentError> {
    let mut prefix=[0;4];input.read_exact(&mut prefix).map_err(|_|AgentError::ModuleUnavailable)?;
    let size=u32::from_be_bytes(prefix) as usize;if size>MAX_WORKER_FRAME_BYTES {return Err(AgentError::InvalidResponse);}
    let mut bytes=vec![0;size];input.read_exact(&mut bytes).map_err(|_|AgentError::ModuleUnavailable)?;
    serde_json::from_slice(&bytes).map_err(|_|AgentError::InvalidResponse)
}
fn spawn_owner(artifacts:ArtifactStore,digest:&str,path:&Path,broker:NetworkBroker,gateway:Arc<dyn ApplicationGateway>,read_only:bool)->Result<Remote,AgentError> {
    let lease=artifacts.acquire_module(digest).map_err(|_|AgentError::ModuleUnavailable)?;
    let manifest=&lease.module().manifest;
    if manifest.id!="mintcat.feature.agent" || manifest.state.schema_version!=1 || !manifest.provides.iter().any(|p|p.contract_id==AGENT_CONTRACT_ID && p.version==1) {return Err(AgentError::ModuleUnavailable);}
    let component=manifest.components.iter().find(|c|c.kind=="service" && c.runtime=="worker" && c.state_model=="single-writer").ok_or(AgentError::ModuleUnavailable)?;
    let entry=lease.module().entry_path(&component.entry).map_err(|_|AgentError::ModuleUnavailable)?;
    let parent=path.parent().ok_or(AgentError::Storage)?;std::fs::create_dir_all(parent).map_err(|_|AgentError::Storage)?;
    let parent=std::fs::canonicalize(parent).map_err(|_|AgentError::Storage)?;let path=parent.join(path.file_name().ok_or(AgentError::Storage)?);
    let workspace=parent.join("agent-workers").join(Uuid::new_v4().to_string());std::fs::create_dir_all(&workspace).map_err(|_|AgentError::Storage)?;
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&workspace,std::fs::Permissions::from_mode(0o700)).map_err(|_|AgentError::Storage)?;}
    let instance=Uuid::new_v4().to_string();
    // No child stderr is retained: even malformed provider bodies cannot reach diagnostics.
    let mut child=Command::new(entry).env_clear().env(WORKER_INSTANCE_ID_ENV,&instance).env(WORKER_MODULE_ID_ENV,&manifest.id).env(WORKER_MODULE_VERSION_ENV,&manifest.version).env(WORKER_MODULE_DIGEST_ENV,digest).env(WORKER_PROVIDES_ENV,serde_json::to_string(&manifest.provides).map_err(|_|AgentError::InvalidRequest)?).env(WORKER_WORKSPACE_ENV,&workspace).env("MINTCAT_AGENT_DATABASE",path).env("MINTCAT_AGENT_READ_ONLY",if read_only {"1"}else{"0"}).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|_|AgentError::ModuleUnavailable)?;
    let input=child.stdin.take().ok_or(AgentError::ModuleUnavailable)?;let output=child.stdout.take().ok_or(AgentError::ModuleUnavailable)?;
    let expected=(instance,manifest.id.clone(),manifest.version.clone(),digest.to_string(),manifest.provides.clone());
    let (sender,receive)=mpsc::channel();let (ready_send,ready_receive)=mpsc::channel();let host_artifacts=artifacts.clone();let host_workspace=workspace.clone();
    let child=Arc::new(Mutex::new(child));let transport_child=child.clone();
    let join=match std::thread::Builder::new().name("agent-owner-transport".into()).spawn(move ||transport(transport_child,input,output,lease,receive,ready_send,expected,host_artifacts,host_workspace,broker,gateway)) {
        Ok(join)=>join,
        Err(_)=>{stop_child(&child);return Err(AgentError::ModuleUnavailable);}
    };
    match ready_receive.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(()))=>Ok(Remote {sender,join:Some(join),artifacts,workspace,digest:digest.into()}),
        _=>{stop_child(&child);let _=sender.send(CommandMessage::Stop);let _=join.join();Err(AgentError::ModuleUnavailable)}
    }
}
fn stop_child(child:&Mutex<Child>) {if let Ok(mut child)=child.lock() {let _=child.kill();let _=child.wait();}}
fn transport(child:Arc<Mutex<Child>>,mut input:ChildStdin,mut output:ChildStdout,_lease:ModuleLease,receive:mpsc::Receiver<CommandMessage>,ready:mpsc::Sender<Result<(),AgentError>>,expected:(String,String,String,String,Vec<ProvidedContract>),artifacts:ArtifactStore,workspace:PathBuf,broker:NetworkBroker,gateway:Arc<dyn ApplicationGateway>) {
    let handshake=(|| {let hello:WorkerHello=read_frame(&mut output)?;if hello.protocol_version!=WORKER_PROTOCOL_VERSION || (hello.instance_id,hello.module_id,hello.module_version,hello.module_digest,hello.provides)!=expected {return Err(AgentError::ModuleUnavailable);}write_frame(&mut input,&WorkerHostHello {protocol_version:WORKER_PROTOCOL_VERSION,accepted:true,generation_id:Uuid::new_v4().to_string(),error:None})})();
    let success=handshake.is_ok();let _=ready.send(handshake);
    if !success {stop_child(&child);return;}
    // HTTP connection tasks must keep running while this thread blocks on native
    // frames or commands, including closing sockets after provider cancellation.
    let runtime=match tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build(){Ok(runtime)=>runtime,Err(_)=>{stop_child(&child);return;}};
    for message in receive {
        let CommandMessage::Invoke(work)=message else {break;};
        let result=(|| {
            write_frame(&mut input,&work.request)?;
            let mut reply_grants=Vec::new();
            loop {match read_frame::<WorkerOutbound>(&mut output)? {
                WorkerOutbound::Response(response)=> {
                    if response.id!=work.request.id || !response.ok {return Err(AgentError::ModuleUnavailable);}
                    return response.result.ok_or(AgentError::InvalidResponse);
                },
                WorkerOutbound::Broker {broker:callback}=> {
                    let id=callback.id.clone();
                    let result=handle_callback(&runtime,&artifacts,&workspace,&broker,&gateway,&work,&callback);
                    let response=match result {
                        Ok(value)=> {
                            let bytes=serde_json::to_vec(&value).map_err(|_|AgentError::InvalidResponse)?;
                            if bytes.len() as u64>MAX_PRIVATE_BYTES {return Err(AgentError::TurnLimit);}
                            let artifact=artifacts.publish_blob_reader(&mut std::io::Cursor::new(bytes),Some("application/json".into())).map_err(|_|AgentError::Storage)?;
                            let grant=artifacts.grant_blob(&artifact).map_err(|_|AgentError::Storage)?;
                            let result=serde_json::to_value(&grant.descriptor).map_err(|_|AgentError::Storage)?;reply_grants.push(grant);
                            WorkerResponse {id,ok:true,result:Some(result),error:None}
                        },
                        Err(_)=>WorkerResponse {id:id.clone(),ok:false,result:None,error:Some(*worker_app_error(id,"PERMISSION_DENIED","agent.host.denied",Value::Null))}
                    };
                    write_frame(&mut input,&response)?;
                }
            }}
        })();
        let broken=result.is_err();
        // A transport failure is terminal only after the writer has actually exited.
        if broken {stop_child(&child);}
        let _=work.result.send(result);if broken {break;}
    }
    stop_child(&child);
}
fn handle_callback(runtime:&tokio::runtime::Runtime,artifacts:&ArtifactStore,workspace:&Path,broker:&NetworkBroker,gateway:&Arc<dyn ApplicationGateway>,work:&Work,callback:&WorkerBrokerRequest)->Result<Value,AgentError> {
    let context=work.context.as_ref().ok_or(AgentError::PermissionDenied)?;
    if work.request.method.as_deref()!=Some("run_turn") || callback.provider_digest!="host" || callback.contract_id!="agent.host" || callback.contract_version!=1 || !callback.grant_ids.is_empty() || !callback.params.is_null() || callback.produced.len()!=1 {return Err(AgentError::PermissionDenied);}
    let descriptor=&callback.produced[0];if descriptor.size>MAX_PRIVATE_BYTES {return Err(AgentError::TurnLimit);}
    let grant=artifacts.grant_produced(workspace,descriptor).map_err(|_|AgentError::InvalidResponse)?;
    let args:Value=serde_json::from_reader(grant.open_read().map_err(|_|AgentError::InvalidResponse)?).map_err(|_|AgentError::InvalidResponse)?;
    if context.cancellation.is_cancelled() {return Err(AgentError::Provider);}
    match callback.method.as_str() {
        "completion"=> {
            if args["model"].as_str()!=Some(context.session.configuration.model.as_str()) {return Err(AgentError::PermissionDenied);}
            let body=serde_json::to_vec(&args).map_err(|_|AgentError::InvalidRequest)?;
            runtime.block_on(complete_with_broker(broker,&context.session.configuration,body,&context.cancellation))
        },
        "tool"=> {
            let name=args["method"].as_str().ok_or(AgentError::InvalidTool)?;
            let key=args["idempotencyKey"].as_str().ok_or(AgentError::InvalidTool)?;
            let prefix=format!("{}:{}:",context.session.session_id,context.session.revision);
            if !key.starts_with(&prefix) {return Err(AgentError::PermissionDenied);}
            let arguments=serde_json::to_string(&args["params"]).map_err(|_|AgentError::InvalidTool)?;
            let request=decode_tool(name,&arguments,&context.session.target,&context.generation,key)?;
            let allowed=request.descriptor.error_codes;
            match runtime.block_on(gateway.execute(request)) {
                Ok(data)=>Ok(json!({"ok":true,"data":data})),
                Err(error)=>Ok(json!({"ok":false,"code":if allowed.contains(&error.code.as_str()){error.code.as_str()}else{"APPLICATION_REJECTED"}}))
            }
        },
        _=>Err(AgentError::PermissionDenied)
    }
}
