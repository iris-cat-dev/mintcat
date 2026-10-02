use std::{future::Future, pin::Pin};
use mintcat_sdk::{ModuleManifest, ModuleComponent};
use mintcat_runtime_artifacts::ArtifactStore;
use crate::{OwnerBackup, ScopeGeneration, Result};

pub type OwnerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
/// Implemented by the actual single writer. Updates never opens or replaces its database.
/// `drain` closes admissions and waits for in-flight transactions; backup must be durable
/// and consistent. Migration and health run with writes closed. `restore` MUST independently
/// enforce its first-new-write fence, including after a process crash.
/// `Busy` from supports/drain means no owner admission/data change occurred: the prepared
/// candidate waits without quarantining its signed package. Other errors require recovery.
pub trait StateOwner: Send + Sync {
    fn owner_id(&self) -> &str;
    fn supports(&self, old: Option<&ModuleManifest>, new: &ModuleManifest) -> Result<()>;
    /// Exclusion from the generic supervisor requires actual native code loading and
    /// read-only health for this owner's exact signed generation.
    fn handles_native(&self, _: &ModuleManifest, _: &ModuleComponent) -> bool { false }
    fn prepare_code<'a>(&'a self, generation: &'a ScopeGeneration, _: ArtifactStore) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            crate::require(!generation.targets.signed.packages.values().filter(|p| p.manifest.id == self.owner_id()).any(|p| p.manifest.components.iter().any(|c| c.kind == "service")),
                format!("state owner {} has no actual signed native code activation adapter", self.owner_id()))
        })
    }
    fn rollback_code<'a>(&'a self, generation: &'a ScopeGeneration, artifacts: ArtifactStore) -> OwnerFuture<'a, ()> { self.prepare_code(generation, artifacts) }
    /// A failed first installation has no old code to select. Deselect and stop its
    /// actual native façade without deleting or initializing its user data.
    fn deactivate_code<'a>(&'a self, generation: &'a ScopeGeneration) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            crate::require(!generation.targets.signed.packages.values().filter(|p| p.manifest.id == self.owner_id()).any(|p| p.manifest.components.iter().any(|c| c.kind == "service")),
                format!("state owner {} has no actual native code deactivation adapter", self.owner_id()))
        })
    }
    fn drain(&self) -> OwnerFuture<'_, ()>;
    fn backup<'a>(&'a self, old_schema: u64, new_schema: u64) -> OwnerFuture<'a, OwnerBackup>;
    fn migrate<'a>(&'a self, backup: &'a OwnerBackup) -> OwnerFuture<'a, ()>;
    fn read_only_health(&self) -> OwnerFuture<'_, ()>;
    fn restore<'a>(&'a self, backup: &'a OwnerBackup) -> OwnerFuture<'a, ()>;
    fn open_writes(&self) -> OwnerFuture<'_, ()>;
}
