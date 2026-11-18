use mintcat_runtime_broker::{CredentialRef, Origin};
use serde::{Deserialize, Serialize};
use url::Url;
use crate::SourceError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SourceConfig {
    pub api_base: Url,
    #[serde(default)] pub website_base: Option<Url>,
    #[serde(default)] pub credential_ref: Option<CredentialRef>,
    pub download_origins: Vec<String>,
    #[serde(default)] pub navigation_origins: Vec<String>,
    #[serde(default)] pub download_request_origin: Option<Url>,
    #[serde(default)] pub application_name: Option<String>,
    #[serde(default)] pub application_version: Option<String>,
}
impl SourceConfig {
    pub fn download_origins(&self)->Result<Vec<Origin>,SourceError> { self.download_origins.iter().map(|origin|Origin::parse(origin).map_err(|_|SourceError::InvalidArgument("invalid download origin configuration".into()))).collect() }
    pub fn navigation_origins(&self)->Result<Vec<Origin>,SourceError> { self.navigation_origins.iter().map(|origin|Origin::parse(origin).map_err(|_|SourceError::InvalidArgument("invalid navigation origin configuration".into()))).collect() }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_origin_configuration_is_not_a_network_failure() {
        let mut config = SourceConfig {
            api_base:Url::parse("https://api.example/v1/").unwrap(),
            website_base:None,credential_ref:None,download_origins:vec![],
            navigation_origins:vec![],download_request_origin:None,
            application_name:None,application_version:None,
        };
        assert!(config.download_origins().unwrap().is_empty());
        config.download_origins.push("not an origin".into());
        assert!(matches!(config.download_origins(),Err(SourceError::InvalidArgument(_))));
        config.navigation_origins.push("not an origin".into());
        assert!(matches!(config.navigation_origins(),Err(SourceError::InvalidArgument(_))));
    }
}
