use std::io;

use mintcat_runtime_jobs::JobError;
use mintcat_runtime_kernel::KernelDatabaseError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkFailureKind {
    Cancelled,
    Timeout,
    Transport,
    Http,
    Redirect,
    Policy,
    ResponseTooLarge,
}

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("network request {request_id} was cancelled")]
    Cancelled { request_id: String },
    #[error("network request {request_id} timed out")]
    Timeout { request_id: String },
    #[error("network request {request_id} failed at {endpoint}")]
    Transport {
        request_id: String,
        endpoint: String,
    },
    #[error("network request {request_id} returned HTTP {status} at {endpoint}")]
    Http {
        request_id: String,
        status: u16,
        endpoint: String,
    },
    #[error("network request {request_id} exceeded its response limit")]
    ResponseTooLarge { request_id: String },
    #[error("origin is outside the declared source scope: {0}")]
    DisallowedOrigin(String),
    #[error("redirect is invalid: {0}")]
    InvalidRedirect(String),
    #[error("network request is invalid: {0}")]
    InvalidRequest(String),
    #[error("credential is unavailable: {0}")]
    CredentialUnavailable(String),
    #[error("credential operation failed")]
    CredentialStore,
    #[error("controlled interaction does not match its action: {0}")]
    InteractionMismatch(String),
    #[error("controlled interaction grant is unavailable: {0}")]
    GrantUnavailable(String),
    #[error("selected input exceeds its declared resource limit: {0}")]
    InputLimitExceeded(String),
    #[error("selected input changed while its immutable snapshot was created")]
    InputChanged,
    #[error(transparent)]
    Kernel(#[from] KernelDatabaseError),
    #[error(transparent)]
    Jobs(#[from] JobError),
    #[error("broker database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("broker JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("broker file operation failed: {0}")]
    Io(#[from] io::Error),
}

impl BrokerError {
    pub fn network_kind(&self) -> Option<NetworkFailureKind> {
        match self {
            Self::Cancelled { .. } => Some(NetworkFailureKind::Cancelled),
            Self::Timeout { .. } => Some(NetworkFailureKind::Timeout),
            Self::Transport { .. } => Some(NetworkFailureKind::Transport),
            Self::Http { .. } => Some(NetworkFailureKind::Http),
            Self::InvalidRedirect(_) => Some(NetworkFailureKind::Redirect),
            Self::DisallowedOrigin(_) | Self::InvalidRequest(_) => Some(NetworkFailureKind::Policy),
            Self::ResponseTooLarge { .. } => Some(NetworkFailureKind::ResponseTooLarge),
            _ => None,
        }
    }
}
