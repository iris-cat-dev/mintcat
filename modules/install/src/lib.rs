mod coordinator;
mod model;
mod profile;
mod service;
mod store;
mod validation;
mod worker_adapter;

use std::{collections::BTreeMap, path::Path, sync::{Arc, RwLock}};

use mintcat_game_api::{LegacyInstallReport, ValidatedGameInstallation};
use mintcat_runtime_kernel::KernelDatabaseError;
use thiserror::Error;

pub use model::*;
pub use coordinator::{install_app_error, GameCatalog, InstallCoordinator};
pub use service::InstallService;
pub use worker_adapter::WorkerGameAdapter;

pub trait GameInstallAdapter: Send + Sync {
    fn game_id(&self) -> &str;

    fn validate_installation(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<ValidatedGameInstallation, String>;

    fn ensure_stopped(&self, root: &Path) -> Result<(), String>;

    fn detect_legacy_install(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<LegacyInstallReport, String>;

    fn build(&self, request: AdapterBuildRequest<'_>) -> Result<AdapterBuildProduct, String>;
}

type AdapterBindings = BTreeMap<String, BTreeMap<String, Arc<dyn GameInstallAdapter>>>;

#[derive(Clone, Default)]
pub struct GameAdapterRegistry {
    adapters: Arc<RwLock<AdapterBindings>>,
}

impl GameAdapterRegistry {
    pub fn new(
        adapters: impl IntoIterator<Item = (ModuleClosure, Arc<dyn GameInstallAdapter>)>,
    ) -> Result<Self, InstallError> {
        let registry = Self::default();
        for (closure, adapter) in adapters {
            registry.register(&closure, adapter)?;
        }
        Ok(registry)
    }

    pub fn register(
        &self,
        closure: &ModuleClosure,
        adapter: Arc<dyn GameInstallAdapter>,
    ) -> Result<(), InstallError> {
        closure.verify()?;
        let game_id = adapter.game_id().trim();
        if game_id.is_empty() {
            return Err(InstallError::InvalidRequest("game id is required".to_owned()));
        }
        let mut adapters = self.adapters.write().map_err(|_| {
            InstallError::InvalidRequest("game adapter registry is poisoned".to_owned())
        })?;
        let versions = adapters.entry(game_id.to_owned()).or_default();
        if versions.contains_key(&closure.digest) {
            return Err(InstallError::InvalidRequest(
                "game adapter closure is already registered".to_owned(),
            ));
        }
        versions.insert(closure.digest.clone(), adapter);
        Ok(())
    }

    pub fn get(
        &self,
        game_id: &str,
        closure_digest: &str,
    ) -> Result<Arc<dyn GameInstallAdapter>, InstallError> {
        self.adapters.read().map_err(|_| {
            InstallError::InvalidRequest("game adapter registry is poisoned".to_owned())
        })?
            .get(game_id).and_then(|versions| versions.get(closure_digest))
            .cloned()
            .ok_or_else(|| InstallError::AdapterUnavailable(
                format!("{game_id} in closure {closure_digest}"),
            ))
    }
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error(transparent)]
    Library(#[from] mintcat_library_service::LibraryError),
    #[error(transparent)]
    Kernel(#[from] KernelDatabaseError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Artifact(#[from] mintcat_runtime_artifacts::ArtifactError),
    #[error(transparent)]
    FileTransaction(#[from] mintcat_runtime_file_transactions::FileTransactionError),
    #[error(transparent)]
    GameContract(#[from] mintcat_game_api::GameContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Job(#[from] mintcat_runtime_jobs::JobError),
    #[error("invalid install request: {0}")]
    InvalidRequest(String),
    #[error("game adapter is unavailable for {0}")]
    AdapterUnavailable(String),
    #[error("game adapter {game_id} failed: {message}")]
    Adapter { game_id: String, message: String },
    #[error(
        "Profile changed: expected revisions {expected_revision}/{expected_content_revision}, actual {actual_revision}/{actual_content_revision}"
    )]
    ProfileChanged {
        expected_revision: u64,
        actual_revision: u64,
        expected_content_revision: u64,
        actual_content_revision: u64,
    },
    #[error("immutable install plan was not found: {0}")]
    PlanNotFound(String),
    #[error("immutable install plan {plan_id} is in state {state}")]
    PlanState { plan_id: String, state: String },
    #[error("immutable install plan expired: {0}")]
    PlanExpired(String),
    #[error("approval does not authorize this immutable install plan: {0}")]
    Approval(String),
}

#[cfg(test)]
mod tests;
