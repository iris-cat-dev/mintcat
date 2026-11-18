use std::{collections::BTreeMap, sync::Arc};

use mintcat_game_api::GameDescriptor;
use mintcat_runtime_broker::{InteractionClient, NetworkBroker, Origin, ProxyPolicy};
use mintcat_runtime_downloads::DownloadCatalog;
use mintcat_source_api::{SourceConfig, SourceError, SourceGameBinding, SourceProvider};
use mintcat_source_modcat::{ModcatConfig, ModcatProvider};
use mintcat_source_modio::{ModioConfig, ModioProvider};
use mintcat_source_nexusmods::{NexusConfig, NexusProvider};

/// Built-in providers share the host's credential and network broker. Local
/// input bindings are separate: selecting bytes is not a catalog operation.
pub fn register_builtin_sources(
    catalog: &DownloadCatalog,
    configuration: &BTreeMap<String, SourceConfig>,
    games: &[GameDescriptor],
    broker: &NetworkBroker,
    interactions: &InteractionClient,
    lock_digest: &str,
) -> Result<BTreeMap<String, SourceGameBinding>, SourceError> {
    for (source_id, config) in configuration {
        for url in std::iter::once(&config.api_base).chain(config.website_base.iter()).chain(config.download_request_origin.iter()) {
            if !url.username().is_empty() || url.password().is_some() {
                return Err(SourceError::InvalidArgument("source configuration must use credential references, not URL credentials".into()));
            }
            Origin::from_url(url)?;
        }
        let provider: Arc<dyn SourceProvider> = match source_id.as_str() {
            "modio" => Arc::new(ModioProvider::new(broker.clone(), ModioConfig {
                api_base: config.api_base.clone(), credential: config.credential_ref.clone(),
                proxy_policy: ProxyPolicy::Direct, download_origins: config.download_origins()?,
            })?),
            "modcat" => Arc::new(ModcatProvider::new(broker.clone(), ModcatConfig {
                api_base: config.api_base.clone(), credential: config.credential_ref.clone(),
                proxy_policy: ProxyPolicy::Direct, download_request_origin: config.download_request_origin.clone(),
            })?),
            "nexusmods" => Arc::new(NexusProvider::new(broker.clone(), interactions.clone(), NexusConfig {
                api_base: config.api_base.clone(),
                website_base: config.website_base.clone().ok_or_else(|| SourceError::InvalidArgument("Nexus configuration requires websiteBase".into()))?,
                credential: config.credential_ref.clone(), proxy_policy: ProxyPolicy::Direct,
                navigation_origins: config.navigation_origins()?, download_origins: config.download_origins()?,
                application_name: config.application_name.clone().ok_or_else(|| SourceError::InvalidArgument("Nexus configuration requires applicationName".into()))?,
                application_version: config.application_version.clone().ok_or_else(|| SourceError::InvalidArgument("Nexus configuration requires applicationVersion".into()))?,
            })?),
            _ => return Err(SourceError::InvalidArgument(format!("unknown built-in source: {source_id}"))),
        };
        catalog.register_provider(provider, lock_digest).map_err(|error| SourceError::InvalidArgument(error.to_string()))?;
    }
    let mut local_games = BTreeMap::new();
    for game in games {
        game.validate().map_err(|error| SourceError::InvalidArgument(error.to_string()))?;
        for source in &game.source_bindings {
            let binding = SourceGameBinding { game_id: game.game_id.clone(), remote_game_key: source.remote_game_key.clone(), provider_game_id: source.provider_game_id.clone() };
            if source.source_id == "local" {
                if local_games.insert(game.game_id.clone(), binding).is_some() {
                    return Err(SourceError::InvalidArgument("game has multiple local input bindings".into()));
                }
            } else if configuration.contains_key(&source.source_id) {
                catalog.register_game(&source.source_id, binding).map_err(|error| SourceError::InvalidArgument(error.to_string()))?;
            }
        }
    }
    Ok(local_games)
}
