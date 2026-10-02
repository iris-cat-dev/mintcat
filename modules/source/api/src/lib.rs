mod worker;
pub use worker::{execute_source_worker, source_module_manifest, source_worker_error, NativeDownload, NativeDownloadResolution, SourceBrowserCallback, SourceWorkerConfig, SourceWorkerParams};
mod network;
pub use network::{SourceNetworkClient, SourceNetworkOutcome, SourceNetworkRequest, SourceNetworkResponse, HOST_PROVIDER_DIGEST, SOURCE_NETWORK_CONTRACT};

use std::collections::BTreeMap;

use async_trait::async_trait;
use mintcat_runtime_broker::{
    BrokerError, CancellationToken, CredentialBinding, InteractionContext, Origin,
};
use mintcat_sdk::{ActionRequired, ModRef};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceGameBinding {
    pub game_id: String,
    pub remote_game_key: String,
    pub provider_game_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SearchSortField {
    Updated,
    Downloads,
    Subscribers,
    Rating,
    Name,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    pub cursor: Option<String>,
    pub page_size: u32,
    pub sort_field: Option<SearchSortField>,
    pub sort_direction: Option<SortDirection>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStatus {
    Accepted,
    Pending,
    Rejected,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceMod {
    pub mod_ref: ModRef,
    pub name: String,
    pub summary: String,
    pub description: String,
    pub author: String,
    pub profile_url: Option<String>,
    pub logo_url: Option<String>,
    pub tags: Vec<String>,
    pub maturity_flags: u32,
    pub review_status: ReviewStatus,
    pub submitted_at: i64,
    pub updated_at: i64,
    pub downloads: u64,
    pub subscribers: u64,
    pub rating: f64,
    pub latest_file: Option<SourceVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceVersion {
    pub version_id: String,
    pub remote_file_id: String,
    pub display_version: String,
    pub filename: String,
    pub file_size: u64,
    pub md5: Option<String>,
    pub published_at: i64,
    pub review_status: ReviewStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDependency {
    pub mod_ref: ModRef,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchPage {
    pub items: Vec<SourceMod>,
    pub next_cursor: Option<String>,
    pub live_pagination: bool,
    pub total: Option<u64>,
    #[serde(default)]
    pub warnings: Vec<SourceWarning>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadDescriptor {
    pub mod_ref: ModRef,
    pub url: Url,
    pub allowed_origins: Vec<Origin>,
    pub headers: BTreeMap<String, String>,
    pub credential: Option<CredentialBinding>,
    pub expires_at: Option<i64>,
    pub expected_size: u64,
    pub expected_md5: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadResolution {
    Ready(DownloadDescriptor),
    InteractionRequired(ActionRequired),
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("source request is invalid: {0}")]
    InvalidArgument(String),
    #[error("source authorization is missing, expired, or invalid")]
    Unauthorized,
    #[error("source resource was not found: {0}")]
    NotFound(String),
    #[error("source resource is unavailable: {0}")]
    Unavailable(String),
    #[error("source interaction is required: {kind}")]
    InteractionRequired { kind: String, target_url: Url },
    #[error("source rate limit was exceeded")]
    RateLimited,
    #[error("source response violated its contract: {0}")]
    InvalidResponse(String),
    #[error(transparent)]
    Network(#[from] BrokerError),
}

#[async_trait]
pub trait SourceProvider: Send + Sync {
    fn source_id(&self) -> &'static str;

    fn parse_url(&self, game: &SourceGameBinding, url: &Url)
        -> Result<Option<ModRef>, SourceError>;

    async fn search(
        &self,
        game: &SourceGameBinding,
        request: &SearchRequest,
        cancellation: &CancellationToken,
    ) -> Result<SearchPage, SourceError>;

    async fn get_mod(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<SourceMod, SourceError>;

    async fn list_versions(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceVersion>, SourceError>;

    async fn get_dependencies(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceDependency>, SourceError>;

    async fn resolve_download(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError>;

    async fn begin_download(
        &self,
        job_id: &str,
        context: &InteractionContext,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadResolution, SourceError> {
        let _ = (job_id, context);
        self.resolve_download(game, mod_ref, cancellation)
            .await
            .map(DownloadResolution::Ready)
    }

    async fn resume_download_action(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let _ = (action, game, mod_ref, cancellation);
        Err(SourceError::InvalidArgument(format!(
            "source {} does not support download interaction continuation",
            self.source_id()
        )))
    }

    /// Callback URL has already been consumed from the authenticated host
    /// interaction grant; the provider still verifies source/file identity.
    async fn resume_download_callback(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        callback: Url,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let _ = (action, game, mod_ref, callback, cancellation);
        Err(SourceError::Unavailable(format!("source {} has no browser callback contract", self.source_id())))
    }
}

