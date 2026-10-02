mod credential;
mod error;
mod interaction;
mod network;

pub use credential::{
    CredentialRef, CredentialStore, CredentialVault, MemoryCredentialStore, OsCredentialStore,
    SecretValue,
};
pub use error::{BrokerError, NetworkFailureKind};
pub use interaction::{
    interaction_binding, interaction_plan_hash, open_interaction_broker, AuthorizedDirectory,
    AuthorizedFile, BrowserAuthorizationRequest, BrowserDownloadRequest, BrowserNavigationDecision,
    DirectorySelectionRequest, DirectorySnapshot, DirectorySnapshotEntry, FileSelectionRequest,
    InputSnapshotLimits, InteractionClient, InteractionContext, InteractionHost,
};
pub use network::{
    CancellationToken, CredentialBinding, CredentialPlacement, ForwardProxy, HttpMethod,
    NetworkBroker, NetworkRequest, NetworkResponse, NetworkStreamResponse, Origin, ProxyPolicy,
    RouteKind,
};

#[cfg(test)]
mod tests;
