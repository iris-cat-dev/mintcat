use std::{fs::{self, File, OpenOptions}, path::PathBuf};
use mintcat_library_service::{LibrarySnapshot, LibraryStore, LIBRARY_CONTRACT_ID, LIBRARY_CONTRACT_VERSION};
use mintcat_runtime_artifacts::ArtifactStore;
use mintcat_sdk::{ModuleComponent, ModuleManifest};
use serde_json::json;
use crate::{OwnerBackup, OwnerFuture, Result, ScopeGeneration, StateOwner, UpdateError, require};

/// Owns actual signed native code selection and its single SQLite writer. The shared facade
/// is retained by all consumers; replacing native code changes their backend, not their data.
pub struct LibraryStateOwner { library:LibraryStore, backups:PathBuf }
impl LibraryStateOwner {
    pub fn new(library:LibraryStore,layout:&mintcat_runtime_kernel::RuntimeLayout)->Result<Self> {
        let backups=layout.state_dir.join("backups/mintcat.library");fs::create_dir_all(&backups)?;
        Ok(Self {library,backups})
    }
    fn schema()->u64 {5}
    fn owner_error(error:mintcat_library_service::LibraryError)->UpdateError {match error {
        mintcat_library_service::LibraryError::ResourceBusy {owner_operation_id}=>UpdateError::Busy(format!("Library operation {owner_operation_id} must drain before maintenance")),
        other=>UpdateError::Blocked(format!("Library native owner: {other}")),
    }}
    fn native_digest(generation:&ScopeGeneration)->Result<&str> {
        let mut owners=generation.targets.signed.packages.iter().filter(|(_,p)|p.manifest.id=="mintcat.library");
        let (digest,_)=owners.next().ok_or_else(||UpdateError::Blocked("generation has no signed Library native owner".into()))?;
        require(owners.next().is_none(),"generation has multiple Library native owners")?;Ok(digest)
    }
    fn snapshot_name<'a>(&self,backup:&'a OwnerBackup)->Result<&'a str> {
        require(backup.owner_id=="mintcat.library" && backup.old_schema==5 && backup.new_schema==5,"Library backup belongs to another owner/schema")?;
        let name=backup.reference["snapshotName"].as_str().ok_or_else(||UpdateError::Blocked("Library backup reference missing".into()))?;
        require(name.len()==41 && name.strip_suffix(".json").is_some_and(|stem|stem.parse::<uuid::Uuid>().is_ok()),"Library backup name invalid")?;Ok(name)
    }
}
impl StateOwner for LibraryStateOwner {
    fn owner_id(&self)->&str {"mintcat.library"}
    fn supports(&self,old:Option<&ModuleManifest>,new:&ModuleManifest)->Result<()> {
        require(new.id==self.owner_id() && new.state.schema_version==Self::schema() && old.is_none_or(|m|m.state.schema_version==Self::schema()),"Library native owner implements schema5 only; a signed actual schema migration provider is required for other schemas")?;
        require(new.provides.iter().any(|p|p.contract_id==LIBRARY_CONTRACT_ID && p.version==LIBRARY_CONTRACT_VERSION) && new.components.iter().any(|c|self.handles_native(new,c)),"Library update must carry its real native owner executable")
    }
    fn handles_native(&self,manifest:&ModuleManifest,component:&ModuleComponent)->bool {
        manifest.id==self.owner_id() && manifest.state.schema_version==5 && component.kind=="service" && component.runtime=="worker" && component.state_model=="single-writer" && manifest.provides.iter().any(|p|p.contract_id==LIBRARY_CONTRACT_ID && p.version==LIBRARY_CONTRACT_VERSION)
    }
    fn prepare_code<'a>(&'a self,generation:&'a ScopeGeneration,artifacts:ArtifactStore)->OwnerFuture<'a,()> {Box::pin(async move {
        self.library.replace_native(artifacts,Self::native_digest(generation)?).map_err(Self::owner_error)
    })}
    fn rollback_code<'a>(&'a self,generation:&'a ScopeGeneration,artifacts:ArtifactStore)->OwnerFuture<'a,()> {Box::pin(async move {
        self.library.replace_native(artifacts,Self::native_digest(generation)?).map_err(Self::owner_error)
    })}
    fn deactivate_code<'a>(&'a self,_generation:&'a ScopeGeneration)->OwnerFuture<'a,()> {Box::pin(async {
        self.library.disconnect().map_err(Self::owner_error)
    })}
    fn drain(&self)->OwnerFuture<'_,()> {Box::pin(async {
        if self.library.owner_digest().is_err() && !self.library.database_path().exists() {return Ok(());}
        self.library.pause().map_err(Self::owner_error)
    })}
    fn backup<'a>(&'a self,old_schema:u64,new_schema:u64)->OwnerFuture<'a,OwnerBackup> {Box::pin(async move {
        require(old_schema==5 && new_schema==5,"Library cross-schema backup needs an implemented signed provider")?;
        let (snapshot,epoch)=if self.library.owner_digest().is_err() && !self.library.database_path().exists() {(LibrarySnapshot::default(),0)} else {(self.library.backup_snapshot().map_err(Self::owner_error)?,self.library.write_epoch().map_err(Self::owner_error)?)};
        let name=format!("{}.json",uuid::Uuid::new_v4());let path=self.backups.join(&name);
        let mut options=OpenOptions::new();options.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
        let mut file=options.open(&path)?;serde_json::to_writer(&mut file,&snapshot)?;file.sync_all()?;drop(file);File::open(&self.backups)?.sync_all()?;
        let digest=LibraryStore::snapshot_file_digest(&path).map_err(Self::owner_error)?;
        Ok(OwnerBackup {owner_id:self.owner_id().into(),old_schema,new_schema,reference:json!({"snapshotName":name,"sha256":digest,"schemaVersion":5,"writeEpoch":epoch})})
    })}
    fn migrate<'a>(&'a self,_:&'a OwnerBackup)->OwnerFuture<'a,()> {Box::pin(async {Err(UpdateError::Blocked("Library schema5 provider has no cross-schema migration; same-schema native code activation is not a data migration".into()))})}
    fn read_only_health(&self)->OwnerFuture<'_,()> {Box::pin(async {self.library.read_only_health().map_err(Self::owner_error)})}
    fn restore<'a>(&'a self,backup:&'a OwnerBackup)->OwnerFuture<'a,()> {Box::pin(async move {
        let name=self.snapshot_name(backup)?;
        let epoch=backup.reference["writeEpoch"].as_u64().ok_or_else(||UpdateError::Blocked("Library backup lacks durable write fence".into()))?;
        require(self.library.write_epoch().map_err(Self::owner_error)?==epoch,"Library rollback forbidden after first new owner write; preserve live data")?;
        let path=self.backups.join(name);
        let digest=LibraryStore::snapshot_file_digest(&path).map_err(Self::owner_error)?;
        require(backup.reference["sha256"].as_str()==Some(digest.as_str()),"Library backup digest mismatch")?;
        let snapshot:LibrarySnapshot=serde_json::from_reader(File::open(path)?)?;
        require(self.library.backup_snapshot().map_err(Self::owner_error)?==snapshot,"Library live data changed; snapshot overwrite is prohibited")?;
        // Same-schema rollback selects the original signed executable. It never overwrites data.
        self.library.read_only_health().map_err(Self::owner_error)
    })}
    fn open_writes(&self)->OwnerFuture<'_,()> {Box::pin(async {self.library.open_writes().map_err(Self::owner_error)})}
}
