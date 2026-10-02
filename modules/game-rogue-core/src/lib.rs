mod builder;
mod descriptor;
mod installation;
mod integrator;
mod plan;
pub mod worker_execution;

use std::io;

use mintcat_engine_unreal_pak::UnrealEngineError;
use mintcat_game_api::GameContractError;
use mintcat_runtime_artifacts::ArtifactError;
use mintcat_unreal_pak::PakAlgorithmError;
use thiserror::Error;

pub use builder::{
    RogueCoreBuildInput, RogueCoreBuildLimits, RogueCoreBuildManifest, RogueCoreBuildRequest,
    RogueCoreBuildResult, RogueCoreBuilder, RogueCorePatchPackage,
};
pub use descriptor::{descriptor, module_manifest};
pub use installation::{
    discover_roots, discover_steam, ensure_not_running, validate_installation,
    validate_main_pak_path, RogueCoreVariant,
};
pub use plan::{
    create_build_plan, create_deploy_plan, detect_legacy_install, RogueCoreBuildPlanRequest,
    RogueCoreCompression, RogueCoreSettings,
};

pub const ROGUE_CORE_GAME_ID: &str = "game:rogue-core";
pub const ROGUE_CORE_MODULE_ID: &str = "mintcat.game.rogue-core";
pub const ROGUE_CORE_MODULE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Error)]
pub enum RogueCoreError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Contract(#[from] GameContractError),
    #[error(transparent)]
    Engine(#[from] UnrealEngineError),
    #[error(transparent)]
    Pak(#[from] PakAlgorithmError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Rogue Core I/O failed while {scope}: {source}")]
    Io {
        scope: String,
        #[source]
        source: io::Error,
    },
    #[error("invalid Rogue Core request: {0}")]
    InvalidRequest(String),
    #[error("unsupported Rogue Core installation layout: {0}")]
    UnsupportedLayout(String),
    #[error("unsupported Rogue Core build/version: {0}")]
    UnsupportedVersion(String),
    #[error("Rogue Core is running: {0}")]
    GameRunning(String),
    #[error("Rogue Core input {input_id} is invalid: {reason}")]
    InvalidInput { input_id: String, reason: String },
    #[error("Rogue Core archive {input_id} is invalid: {reason}")]
    Archive { input_id: String, reason: String },
    #[error("Rogue Core compatibility builder failed: {0}")]
    CompatibilityBuild(String),
    #[error("Rogue Core output verification failed: {0}")]
    Verification(String),
}

#[cfg(test)]
mod tests;
