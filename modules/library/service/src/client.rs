use std::{collections::BTreeSet, fs::File, io::{Read, Write}, path::{Path, PathBuf}, process::{Child, ChildStdin, ChildStdout, Command, Stdio}, sync::{Arc, Mutex, mpsc}, time::Duration};
use mintcat_runtime_artifacts::{ArtifactStore, ArtifactGrant, ModuleLease};
use mintcat_runtime_kernel::RuntimeLayout;
use mintcat_sdk::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{json, Value};
use crate::*;
use crate::LegacyImportReport;
use crate::native::{HostCall, WireResult};

struct Work { request: WorkerRequest, events: mpsc::Sender<Event> }
enum Event { Complete(Result<Value, LibraryError>), Credential(WorkerBrokerRequest, mpsc::Sender<Result<Value, String>>) }
struct Remote { sender: mpsc::Sender<Work>, artifacts: ArtifactStore, workspace: PathBuf, digest: String, child: Arc<Mutex<Child>> }
impl Drop for Remote {
    fn drop(&mut self) { terminate(&self.child); }
}
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeOwnerIdentity {
    pub instance_id: String,
    pub module_version: String,
    pub module_digest: String,
    pub process_id: u32,
}
/// Host facade. Production has no SQLite connection; all calls go to a verified native owner.
#[derive(Clone)]
pub struct LibraryStore { remote: Arc<Mutex<Option<Remote>>>, path: PathBuf, layout: RuntimeLayout, #[cfg(any(test, feature = "fixtures"))] local: Option<crate::OwnerStore> }
impl std::fmt::Debug for LibraryStore { fn fmt(&self, f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.debug_struct("LibraryStore").field("path", &self.path).finish_non_exhaustive() } }
impl LibraryStore {
    pub fn connect(artifacts: ArtifactStore, digest: &str, layout: &RuntimeLayout) -> Result<Self, LibraryError> {
        let remote = spawn_owner(artifacts, digest, layout)?;
        Ok(Self { remote: Arc::new(Mutex::new(Some(remote))), path: layout.state_dir.join("mintcat.library/library.sqlite"), layout: layout.clone(), #[cfg(any(test, feature = "fixtures"))] local: None })
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub fn open(layout: &RuntimeLayout) -> Result<Self, LibraryError> { Ok(Self { remote: Arc::new(Mutex::new(None)), path: layout.state_dir.join("mintcat.library/library.sqlite"), layout: layout.clone(), local: Some(crate::OwnerStore::open(layout)?) }) }
    pub fn database_path(&self) -> &Path { &self.path }
    pub fn owner_digest(&self) -> Result<String, LibraryError> { self.remote.lock().map_err(|_| LibraryError::Poisoned)?.as_ref().map(|r|r.digest.clone()).ok_or_else(|| unavailable("owner not connected")) }
    pub fn write_epoch(&self) -> Result<u64, LibraryError> { self.call("owner.write_epoch", None, json!({}), &[], None) }
    pub fn native_identity(&self) -> Result<NativeOwnerIdentity, LibraryError> { self.call("owner.identity", None, json!({}), &[], None) }
    pub fn unavailable(layout: &RuntimeLayout) -> Self {
        Self { remote: Arc::new(Mutex::new(None)), path: layout.state_dir.join("mintcat.library/library.sqlite"), layout: layout.clone(), #[cfg(any(test, feature = "fixtures"))] local: None }
    }
    pub fn pause(&self) -> Result<(), LibraryError> { self.call("owner.pause", None, json!({}), &[], None) }
    pub fn read_only_health(&self) -> Result<(), LibraryError> { self.call("owner.health", None, json!({}), &[], None) }
    pub fn open_writes(&self) -> Result<(), LibraryError> { self.call("owner.open_writes", None, json!({}), &[], None) }
    /// Unload the selected native process after lifecycle drain, without deleting state.
    pub fn disconnect(&self) -> Result<(), LibraryError> {
        let mut slot=self.remote.lock().map_err(|_|LibraryError::Poisoned)?;
        *slot=None;
        Ok(())
    }
    /// Drain the old backend before selecting a different signed native executable.
    pub fn replace_native(&self, artifacts: ArtifactStore, digest: &str) -> Result<(), LibraryError> {
        let previous = self.remote.lock().map_err(|_| LibraryError::Poisoned)?.as_ref().map(|r| r.digest.clone());
        if previous.as_deref().is_some_and(|old| old != digest) { self.pause()?; }
        let mut slot = self.remote.lock().map_err(|_| LibraryError::Poisoned)?;
        if let Some(old) = slot.as_ref() { if old.digest == digest { return Ok(()); } }
        *slot = None;
        *slot = Some(spawn_owner(artifacts, digest, &self.layout)?);
        Ok(())
    }
    fn artifacts(&self) -> Result<ArtifactStore, LibraryError> { self.remote.lock().map_err(|_| LibraryError::Poisoned)?.as_ref().map(|r| r.artifacts.clone()).ok_or_else(|| unavailable("owner not connected")) }
    fn call<T: DeserializeOwned>(&self, method:&str, authorization:Option<&LibraryAuthorization>, args:Value, grants:&[&ArtifactGrant], vault:Option<&dyn CredentialVault>) -> Result<T, LibraryError> {
        let slot = self.remote.lock().map_err(|_| LibraryError::Poisoned)?;
        let remote = slot.as_ref().ok_or_else(|| unavailable("owner not connected"))?;
        // Authorization is created by trusted host consumers, never merged with application JSON.
        let call = HostCall { authorization: authorization.cloned(), args };
        let mut params = serde_json::to_value(call)?;
        let mut descriptors = Vec::new();
        for grant in grants { grant.open_read().map_err(|_| LibraryError::ArtifactNotVerified(grant.descriptor.digest.clone()))?; descriptors.push(grant.descriptor.clone()); }
        let mut payload_grant = None;
        if serde_json::to_vec(&params)?.len() > MAX_WORKER_FRAME_BYTES / 2 {
            let mut bytes = std::io::Cursor::new(serde_json::to_vec(&params)?);
            let artifact = remote.artifacts.publish_blob_reader(&mut bytes, Some("application/json".into())).map_err(|e| unavailable(e.to_string()))?;
            let grant = remote.artifacts.grant_blob(&artifact).map_err(|e| unavailable(e.to_string()))?;
            params = json!({"inputGrant":grant.descriptor.grant_id}); descriptors.push(grant.descriptor.clone()); payload_grant = Some(grant);
        }
        let request = WorkerRequest { id: uuid::Uuid::new_v4().to_string(), kind:WorkerRequestKind::Invoke, contract_id:Some(LIBRARY_CONTRACT_ID.into()), contract_version:Some(LIBRARY_CONTRACT_VERSION), method:Some(method.into()), params, grants:descriptors };
        let (send, receive) = mpsc::channel();
        remote.sender.send(Work { request, events:send }).map_err(|_| unavailable("owner transport closed"))?;
        let artifacts = remote.artifacts.clone(); let workspace = remote.workspace.clone();
        drop(slot);
        let result = loop { match receive.recv().map_err(|_| unavailable("owner transport exited"))? {
            Event::Complete(result) => break result.map_err(|error| unavailable(error.to_string()))?,
            Event::Credential(request, reply) => {
                let result = if method != "import_legacy" || vault.is_none() { Err("credential import not authorized".into()) } else {
                    let vault = vault.unwrap();
                    match request.method.as_str() {
                        "store" => { let p=&request.params; vault.store(LegacyCredential { owner_scope:p["ownerScope"].as_str().unwrap_or(""), provider:p["provider"].as_str().unwrap_or(""), secret:p["secret"].as_str().unwrap_or("") }).map(Value::String).map_err(|_| "credential vault rejected import".into()) },
                        "delete" => vault.delete(request.params["credentialRef"].as_str().unwrap_or("")).map(|_| Value::Null).map_err(|_| "credential vault rejected cleanup".into()),
                        _ => Err("credential method denied".into()),
                    }
                }; let _=reply.send(result);
            }
        }};
        drop(payload_grant);
        let wire: WireResult = serde_json::from_value(result)?;
        match wire {
            WireResult::Failure(error) => Err(error.into_error()),
            WireResult::Produced(descriptor) => {
                let grant = artifacts.grant_produced(&workspace, &descriptor).map_err(|_| LibraryError::ArtifactNotVerified(descriptor.digest.clone()))?;
                let value = serde_json::from_reader(grant.open_read().map_err(|e| unavailable(e.to_string()))?)?;
                std::fs::remove_file(&descriptor.path)?;
                Ok(value)
            }
        }
    }
    pub fn import_legacy(&self, source:&Path, vault:&dyn CredentialVault) -> Result<LegacyImportReport, LibraryError> {
        self.import_legacy_bound(source, None, vault)
    }
    pub fn import_legacy_checked(&self, source:&Path, expected_source_digest:&str, vault:&dyn CredentialVault) -> Result<LegacyImportReport, LibraryError> {
        self.import_legacy_bound(source, Some(expected_source_digest), vault)
    }
    fn import_legacy_bound(&self, source:&Path, expected_source_digest:Option<&str>, vault:&dyn CredentialVault) -> Result<LegacyImportReport, LibraryError> {
        #[cfg(any(test, feature = "fixtures"))] if let Some(local)=&self.local {
            return match expected_source_digest {
                Some(expected)=>local.import_legacy_checked(source,expected,vault),
                None=>local.import_legacy(source,vault),
            };
        }
        let artifacts=self.artifacts()?;
        let copy = tempfile::tempdir()?;
        let copy_path = copy.path().join("approved-legacy.sqlite");
        Self::snapshot_legacy_source(source, &copy_path)?;
        let artifact=artifacts.publish_blob_file(&copy_path,Some("application/vnd.sqlite3".into())).map_err(|e| unavailable(e.to_string()))?;
        let grant=artifacts.grant_blob(&artifact).map_err(|e| unavailable(e.to_string()))?;
        self.call("import_legacy",None,json!({"sourceGrant":grant.descriptor.grant_id,"expectedSourceDigest":expected_source_digest}),&[&grant],Some(vault))
    }
    /// Freeze an approved legacy source, including committed WAL, without writing the source.
    pub fn snapshot_legacy_source(source: &Path, target: &Path) -> Result<(), LibraryError> {
        if target.exists() { return Err(LibraryError::BackupExists(target.into())); }
        let input = rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let reserve = std::fs::OpenOptions::new().write(true).create_new(true).open(target)?;
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; reserve.set_permissions(std::fs::Permissions::from_mode(0o600))?; }
        drop(reserve);
        let mut output = rusqlite::Connection::open(target)?;
        let backup = rusqlite::backup::Backup::new(&input, &mut output)?;
        backup.run_to_completion(128, Duration::from_millis(10), None)?;
        drop(backup); drop(output); File::open(target)?.sync_all()?;
        Ok(())
    }
    pub fn legacy_source_digest(source: &Path) -> Result<String, LibraryError> {
        crate::legacy::legacy_source_digest(source)
    }
    pub fn snapshot_digest(snapshot: &LibrarySnapshot) -> Result<String, LibraryError> {
        crate::digest::serialized_digest(snapshot)
    }
    pub fn snapshot_file_digest(source: &Path) -> Result<String, LibraryError> {
        let mut writer=crate::digest::DigestWriter::new(std::io::sink());
        std::io::copy(&mut File::open(source)?, &mut writer)?;
        Ok(writer.finish().1)
    }
    pub fn export_to(&self,target:&Path)->Result<LibraryBackup,LibraryError> {
        if target.exists() { return Err(LibraryError::BackupExists(target.into())); }
        let backup=LibraryBackup { format:LIBRARY_BACKUP_FORMAT.into(),exported_at_ms:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64,snapshot:self.snapshot()? };
        let parent=target.parent().ok_or_else(|| LibraryError::InvalidRecord("backup path has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        let temporary=parent.join(format!(".library-backup-{}",uuid::Uuid::new_v4()));
        let result=(|| -> Result<(),LibraryError> {
            let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
            #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
            let mut file=options.open(&temporary)?;
            serde_json::to_writer(&mut file,&backup)?;file.sync_all()?;drop(file);
            match std::fs::hard_link(&temporary,target) {
                Ok(())=>{},
                Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>return Err(LibraryError::BackupExists(target.into())),
                Err(error)=>return Err(error.into()),
            }
            File::open(parent)?.sync_all()?;Ok(())
        })();
        let _=std::fs::remove_file(&temporary);result?;Ok(backup)
    }
    pub fn restore_from(&self,source:&Path)->Result<LibrarySnapshot,LibraryError> {
        let backup:LibraryBackup=serde_json::from_reader(File::open(source)?)?;
        if backup.format!=LIBRARY_BACKUP_FORMAT { return Err(LibraryError::UnsupportedBackup(backup.format)); }
        self.import_empty_snapshot(&backup.snapshot)?;Ok(backup.snapshot)
    }
}
fn unavailable(message:impl Into<String>)->LibraryError { LibraryError::OwnerUnavailable(message.into()) }
fn write_frame<T:Serialize>(out:&mut impl Write,value:&T)->Result<(),LibraryError> { let bytes=serde_json::to_vec(value)?;if bytes.len()>MAX_WORKER_FRAME_BYTES {return Err(unavailable("frame exceeds limit"));}out.write_all(&(bytes.len() as u32).to_be_bytes())?;out.write_all(&bytes)?;out.flush()?;Ok(()) }
fn read_frame<T:DeserializeOwned>(input:&mut impl Read)->Result<T,LibraryError> { let mut prefix=[0;4];input.read_exact(&mut prefix)?;let len=u32::from_be_bytes(prefix) as usize;if len>MAX_WORKER_FRAME_BYTES {return Err(unavailable("frame exceeds limit"));}let mut bytes=vec![0;len];input.read_exact(&mut bytes)?;Ok(serde_json::from_slice(&bytes)?) }
fn spawn_owner(artifacts:ArtifactStore,digest:&str,layout:&RuntimeLayout)->Result<Remote,LibraryError> {
    if !layout.initialized {return Err(LibraryError::UninitializedLayout);}
    let lease=artifacts.acquire_module(digest).map_err(|e| unavailable(e.to_string()))?;
    let manifest=&lease.module().manifest;
    if manifest.id!="mintcat.library" || manifest.state.schema_version!=5 || !manifest.provides.iter().any(|p|p.contract_id==LIBRARY_CONTRACT_ID && p.version==LIBRARY_CONTRACT_VERSION) {return Err(unavailable("signed package does not implement Library schema5 owner"));}
    let component=manifest.components.iter().find(|c|c.kind=="service" && c.runtime=="worker" && c.state_model=="single-writer").ok_or_else(|| unavailable("native Library component missing"))?;
    let entry=lease.module().entry_path(&component.entry).map_err(|e| unavailable(e.to_string()))?;
    let workspace=layout.runtime_dir.join("library-workers").join(uuid::Uuid::new_v4().to_string());std::fs::create_dir_all(&workspace)?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&workspace,std::fs::Permissions::from_mode(0o700))?; }
    let instance=uuid::Uuid::new_v4().to_string();
    let log=std::fs::OpenOptions::new().create_new(true).write(true).open(workspace.join("worker.log"))?;
    let mut child=Command::new(entry).env(WORKER_INSTANCE_ID_ENV,&instance).env(WORKER_MODULE_ID_ENV,&manifest.id).env(WORKER_MODULE_VERSION_ENV,&manifest.version).env(WORKER_MODULE_DIGEST_ENV,digest).env(WORKER_PROVIDES_ENV,serde_json::to_string(&manifest.provides)?).env(WORKER_WORKSPACE_ENV,&workspace).env("MINTCAT_LIBRARY_DATA_DIR",&layout.data_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(log).spawn().map_err(|error|unavailable(error.to_string()))?;
    let input=child.stdin.take().ok_or_else(|| unavailable("stdin missing"))?;let output=child.stdout.take().ok_or_else(|| unavailable("stdout missing"))?;
    let child=Arc::new(Mutex::new(child));
    let transport_child=child.clone();
    let (send,receive)=mpsc::channel();let (ready_send,ready_receive)=mpsc::channel();let expected=(instance,manifest.id.clone(),manifest.version.clone(),digest.to_string(),manifest.provides.clone());
    std::thread::Builder::new().name("library-owner-transport".into()).spawn(move || {
        transport(transport_child,input,output,lease,receive,ready_send,expected);
    })?;
    match ready_receive.recv_timeout(Duration::from_secs(10)) {
        Ok(result)=>result.map_err(|error|unavailable(error.to_string()))?,
        Err(_)=>{terminate(&child);return Err(unavailable("native handshake timed out"));},
    }
    Ok(Remote {sender:send,artifacts,workspace,digest:digest.into(),child})
}
fn transport(child:Arc<Mutex<Child>>,mut input:ChildStdin,mut output:ChildStdout,_lease:ModuleLease,receive:mpsc::Receiver<Work>,ready:mpsc::Sender<Result<(),LibraryError>>,expected:(String,String,String,String,Vec<ProvidedContract>)) {
    let handshake=(||->Result<(),LibraryError>{let hello:WorkerHello=read_frame(&mut output)?;if hello.protocol_version!=WORKER_PROTOCOL_VERSION || (hello.instance_id,hello.module_id,hello.module_version,hello.module_digest,hello.provides)!=expected {return Err(unavailable("native identity differs from signed package"));}write_frame(&mut input,&WorkerHostHello {protocol_version:WORKER_PROTOCOL_VERSION,accepted:true,generation_id:uuid::Uuid::new_v4().to_string(),error:None})?;Ok(())})();
    let ok=handshake.is_ok();let _=ready.send(handshake);if !ok {terminate(&child);return;}
    for work in receive {
        let result=(||->Result<Value,LibraryError>{
            write_frame(&mut input,&work.request)?;
            loop { match read_frame::<WorkerOutbound>(&mut output)? {
                WorkerOutbound::Response(response) => {if response.id!=work.request.id {return Err(unavailable("response id mismatch"));}return if response.ok {response.result.ok_or_else(|| unavailable("missing result"))} else {Err(unavailable("native request rejected"))};},
                WorkerOutbound::Broker {broker} => {
                    if work.request.method.as_deref()!=Some("import_legacy") || broker.contract_id!="library.credentials" || broker.contract_version!=1 || broker.provider_digest!="host" || !broker.produced.is_empty() || broker.grant_ids.len()!=1 || work.request.params["args"]["sourceGrant"].as_str()!=broker.grant_ids.first().map(String::as_str) {return Err(LibraryError::PermissionDenied("credential callback denied".into()));}
                    let id=broker.id.clone();let (reply,answer)=mpsc::channel();work.events.send(Event::Credential(broker,reply)).map_err(|_| unavailable("caller exited"))?;
                    let result=answer.recv().map_err(|_| unavailable("credential host exited"))?;
                    let response=match result {Ok(value)=>WorkerResponse{id,ok:true,result:Some(value),error:None},Err(_)=>WorkerResponse{id:id.clone(),ok:false,result:None,error:Some(*worker_app_error(id,"PERMISSION_DENIED","library.credentials.denied",Value::Null))}};
                    write_frame(&mut input,&response)?;
                }
            }}
        })();let broken=result.is_err();let _=work.events.send(Event::Complete(result));if broken {break;}
    }
    terminate(&child);
}
fn terminate(child:&Arc<Mutex<Child>>) { if let Ok(mut child)=child.lock() {let _=child.kill();let _=child.wait();} }
include!("client_methods.rs");
