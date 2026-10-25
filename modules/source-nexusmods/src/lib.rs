use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::DateTime;
use mintcat_runtime_broker::{
    interaction_binding, BrokerError, BrowserDownloadRequest, CancellationToken, CredentialBinding,
    CredentialPlacement, CredentialRef, HttpMethod, InteractionClient, InteractionContext,
    NetworkBroker, NetworkRequest, NetworkResponse, Origin, ProxyPolicy,
};
use mintcat_sdk::{ActionRequired, ModRef};
use mintcat_source_api::{
    DownloadDescriptor, DownloadResolution, ReviewStatus, SearchPage, SearchRequest,
    SearchSortField, SortDirection, SourceDependency, SourceError, SourceGameBinding, SourceMod,
    SourceNetworkClient, SourceProvider, SourceVersion, SourceWarning,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use url::Url;

pub const SOURCE_ID: &str = "nexusmods";
const MAX_PAGE_SIZE: u32 = 100;
const SEARCH_QUERY: &str = r#"
query NexusModsSearch($filter: ModsFilter, $sort: [ModsSort!], $offset: Int, $count: Int) {
  mods(filter: $filter, sort: $sort, offset: $offset, count: $count) {
    nodes {
      modId
      name
      summary
      description
      author
      downloads
      endorsements
      pictureUrl
      thumbnailUrl
      thumbnailLargeUrl
      createdAt
      updatedAt
      game { domainName }
      uploader { name }
    }
    totalCount
    nodesCount
  }
}
"#;

#[derive(Debug, Clone)]
pub struct NexusConfig {
    pub api_base: Url,
    pub website_base: Url,
    pub credential: Option<CredentialRef>,
    pub proxy_policy: ProxyPolicy,
    pub navigation_origins: Vec<Origin>,
    pub download_origins: Vec<Origin>,
    pub application_name: String,
    pub application_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NexusDownloadOutcome {
    Ready(DownloadDescriptor),
    InteractionRequired(ActionRequired),
}

#[derive(Clone)]
pub struct NexusProvider {
    broker: SourceNetworkClient,
    interactions: Option<InteractionClient>,
    config: NexusConfig,
    api_origin: Origin,
    website_origin: Origin,
}

impl NexusProvider {
    pub fn new(
        broker: NetworkBroker,
        interactions: InteractionClient,
        config: NexusConfig,
    ) -> Result<Self, SourceError> {
        let mut provider = Self::with_network_client(broker.into(), config)?;
        provider.interactions = Some(interactions);
        Ok(provider)
    }

    pub fn with_network_client(broker: SourceNetworkClient, config: NexusConfig) -> Result<Self, SourceError> {
        let api_origin = Origin::from_url(&config.api_base)
            .map_err(|_| SourceError::InvalidArgument("invalid Nexus API origin configuration".into()))?;
        let website_origin = Origin::from_url(&config.website_base)
            .map_err(|_| SourceError::InvalidArgument("invalid Nexus website origin configuration".into()))?;
        if !config.navigation_origins.contains(&website_origin) {
            return Err(SourceError::InvalidArgument(
                "Nexus navigation origins must contain the website origin".to_owned(),
            ));
        }
        if config.application_name.trim().is_empty() || config.application_version.trim().is_empty()
        {
            return Err(SourceError::InvalidArgument(
                "Nexus application identity cannot be empty".to_owned(),
            ));
        }
        Ok(Self {
            broker,
            interactions: None,
            config,
            api_origin,
            website_origin,
        })
    }

    pub async fn resolve_download_or_request_interaction(
        &self,
        job_id: &str,
        context: &InteractionContext,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<NexusDownloadOutcome, SourceError> {
        match self.resolve_download(game, mod_ref, cancellation).await {
            Ok(descriptor) => Ok(NexusDownloadOutcome::Ready(descriptor)),
            Err(SourceError::InteractionRequired { target_url, .. }) => {
                let action = self.interactions.as_ref().ok_or_else(|| SourceError::Unavailable("browser interactions require the host broker".into()))?.request_browser_download(
                    job_id,
                    context,
                    BrowserDownloadRequest {
                        download_page_url: target_url,
                        allowed_navigation_origins: self.config.navigation_origins.clone(),
                        callback_origins: self.config.download_origins.clone(),
                        callback_schemes: vec!["nxm".to_owned()],
                    },
                )?;
                Ok(NexusDownloadOutcome::InteractionRequired(action))
            }
            Err(error) => Err(error),
        }
    }

    pub async fn resume_download(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let callback = self.interactions.as_ref()
            .ok_or_else(|| SourceError::Unavailable("browser interactions require the host broker".into()))?
            .consume_browser_callback(&action.action_id, &interaction_binding(action))?;
        self.resume_download_from_callback(action, game, mod_ref, callback, cancellation).await
    }

    pub async fn resume_download_from_callback(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        callback: Url,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        if action.kind != "browser-download" {
            return Err(SourceError::InvalidArgument("Nexus continuation requires a browser-download action".into()));
        }
        let files = self.files(game, mod_ref, cancellation).await?;
        let selected = select_exact_file(mod_ref, files)?;
        self.validate_action_target(action, game, mod_ref, &selected)?;
        if callback.scheme() == "nxm" {
            let parsed = parse_nexus_url(&callback, None)?.ok_or_else(|| {
                SourceError::InvalidResponse("Nexus callback is not a valid nxm URL".to_owned())
            })?;
            validate_callback_identity(&parsed, game, mod_ref, &selected)?;
            let key = parsed.key.ok_or_else(|| {
                SourceError::InvalidResponse("Nexus nxm callback has no download key".to_owned())
            })?;
            let expires = parsed.expires.ok_or_else(|| {
                SourceError::InvalidResponse("Nexus nxm callback has no expiry".to_owned())
            })?;
            if expires <= now_seconds()? {
                return Err(SourceError::InteractionRequired {
                    kind: "browser-download".to_owned(),
                    target_url: self.download_page_url(game, mod_ref, selected.file_id)?,
                });
            }
            return self
                .resolve_selected_download(
                    game,
                    mod_ref,
                    selected,
                    Some(DownloadCredentials { key, expires }),
                    cancellation,
                )
                .await;
        }

        let callback_origin = Origin::from_url(&callback)?;
        if !self.config.download_origins.contains(&callback_origin) {
            return Err(SourceError::InvalidResponse(
                "Nexus callback download origin is not trusted".to_owned(),
            ));
        }
        Ok(self.download_descriptor(mod_ref, selected, callback, None))
    }

    fn validate_action_target(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        file: &ApiFile,
    ) -> Result<(), SourceError> {
        let start_url = action
            .target
            .get("authorizationUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SourceError::InvalidResponse(
                    "Nexus browser action has no bound download page".to_owned(),
                )
            })?;
        let start_url = Url::parse(start_url).map_err(|_| {
            SourceError::InvalidResponse("Nexus browser action URL is invalid".to_owned())
        })?;
        let parsed = self.parse_url(game, &start_url)?.ok_or_else(|| {
            SourceError::InvalidResponse(
                "Nexus browser action is outside the expected game".to_owned(),
            )
        })?;
        if parsed.remote_mod_id != mod_ref.remote_mod_id
            || parsed.remote_file_id.as_deref() != Some(&file.file_id.to_string())
        {
            return Err(SourceError::InvalidArgument(
                "Nexus browser action does not match the requested file".to_owned(),
            ));
        }
        Ok(())
    }

    async fn raw_request(
        &self,
        url: Url,
        method: HttpMethod,
        body: Option<Vec<u8>>,
        cancellation: &CancellationToken,
    ) -> Result<NetworkResponse, SourceError> {
        let credential = self
            .config
            .credential
            .clone()
            .ok_or(SourceError::Unauthorized)?;
        let mut request =
            NetworkRequest::get("source.nexusmods", url, vec![self.api_origin.clone()]);
        request.method = method;
        request.proxy_policy = self.config.proxy_policy;
        request.accept_http_errors = true;
        request
            .headers
            .insert("accept".to_owned(), "application/json".to_owned());
        request.headers.insert(
            "application-name".to_owned(),
            self.config.application_name.clone(),
        );
        request.headers.insert(
            "application-version".to_owned(),
            self.config.application_version.clone(),
        );
        request.credential = Some(CredentialBinding {
            reference: credential,
            placement: CredentialPlacement::Header("apikey".to_owned()),
            allowed_origins: vec![self.api_origin.clone()],
        });
        if let Some(body) = body {
            request.method = HttpMethod::Post;
            request
                .headers
                .insert("content-type".to_owned(), "application/json".to_owned());
            request.body = Some(body.into());
        }
        self.broker
            .execute(&request, cancellation)
            .await
            .map_err(map_network_error)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<T, SourceError> {
        let url = self.endpoint(path)?;
        let response = self
            .raw_request(url, HttpMethod::Get, None, cancellation)
            .await?;
        parse_api_response(response, "Nexus resource")
    }

    fn endpoint(&self, path: &str) -> Result<Url, SourceError> {
        self.config
            .api_base
            .join(path.trim_start_matches('/'))
            .map_err(|_| SourceError::InvalidArgument("invalid Nexus endpoint".to_owned()))
    }

    async fn files(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ApiFile>, SourceError> {
        let mod_id = validate_ref(game, mod_ref)?;
        let response: ApiFilesResponse = self
            .get_json(
                &format!(
                    "v1/games/{}/mods/{mod_id}/files.json",
                    provider_domain(game)?
                ),
                cancellation,
            )
            .await?;
        let files = response.into_files();
        if files.iter().any(|file| file.file_id == 0) {
            return Err(SourceError::InvalidResponse(
                "Nexus file identity is missing".to_owned(),
            ));
        }
        Ok(files)
    }

    async fn detail(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<ApiMod, SourceError> {
        let mod_id = validate_ref(game, mod_ref)?;
        let detail: ApiMod = self
            .get_json(
                &format!("v1/games/{}/mods/{mod_id}.json", provider_domain(game)?),
                cancellation,
            )
            .await?;
        if detail.mod_id != mod_id {
            return Err(SourceError::InvalidResponse(
                "Nexus detail returned a different mod id".to_owned(),
            ));
        }
        if detail
            .domain_name
            .as_deref()
            .is_some_and(|domain| domain != game.provider_game_id)
        {
            return Err(SourceError::InvalidResponse(
                "Nexus detail escaped the bound game".to_owned(),
            ));
        }
        Ok(detail)
    }

    async fn resolve_selected_download(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        file: ApiFile,
        credentials: Option<DownloadCredentials>,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let mod_id = validate_ref(game, mod_ref)?;
        let mut url = self.endpoint(&format!(
            "v1/games/{}/mods/{mod_id}/files/{}/download_link.json",
            provider_domain(game)?,
            file.file_id
        ))?;
        if let Some(credentials) = &credentials {
            url.query_pairs_mut()
                .append_pair("key", &credentials.key)
                .append_pair("expires", &credentials.expires.to_string());
        }
        let response = self
            .raw_request(url, HttpMethod::Get, None, cancellation)
            .await?;
        if matches!(response.status, 401 | 403) {
            return Err(SourceError::InteractionRequired {
                kind: "browser-download".to_owned(),
                target_url: self.download_page_url(game, mod_ref, file.file_id)?,
            });
        }
        let links: Vec<ApiDownloadLink> = parse_api_response(response, "Nexus download link")?;
        if links.is_empty() {
            return Err(SourceError::InteractionRequired {
                kind: "browser-download".to_owned(),
                target_url: self.download_page_url(game, mod_ref, file.file_id)?,
            });
        }
        let mut rejected_origin = false;
        for link in links {
            let Some(raw_url) = link.uri else {
                continue;
            };
            let url = Url::parse(&raw_url).map_err(|_| {
                SourceError::InvalidResponse("Nexus download URL is invalid".to_owned())
            })?;
            let origin = Origin::from_url(&url)?;
            if self.config.download_origins.contains(&origin) {
                return Ok(self.download_descriptor(
                    mod_ref,
                    file,
                    url,
                    credentials.map(|value| value.expires),
                ));
            }
            rejected_origin = true;
        }
        Err(SourceError::InvalidResponse(if rejected_origin {
            "Nexus returned a download URL outside the configured origins".to_owned()
        } else {
            "Nexus returned no usable download URL".to_owned()
        }))
    }

    fn download_descriptor(
        &self,
        mod_ref: &ModRef,
        file: ApiFile,
        url: Url,
        expires_at: Option<i64>,
    ) -> DownloadDescriptor {
        DownloadDescriptor {
            mod_ref: exact_mod_ref(mod_ref, &file),
            url,
            allowed_origins: self.config.download_origins.clone(),
            headers: BTreeMap::new(),
            credential: None,
            expires_at,
            expected_size: file_size(&file),
            expected_md5: None,
        }
    }

    fn download_page_url(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        file_id: u64,
    ) -> Result<Url, SourceError> {
        let mut url = self.profile_url(game, validate_ref(game, mod_ref)?)?;
        url.query_pairs_mut()
            .append_pair("tab", "files")
            .append_pair("file_id", &file_id.to_string());
        Ok(url)
    }

    fn profile_url(&self, game: &SourceGameBinding, mod_id: u64) -> Result<Url, SourceError> {
        self.config
            .website_base
            .join(&format!("{}/mods/{mod_id}", provider_domain(game)?))
            .map_err(|_| SourceError::InvalidArgument("invalid Nexus profile URL".to_owned()))
    }
}

#[async_trait]
impl SourceProvider for NexusProvider {
    fn source_id(&self) -> &'static str {
        SOURCE_ID
    }

    fn parse_url(
        &self,
        game: &SourceGameBinding,
        url: &Url,
    ) -> Result<Option<ModRef>, SourceError> {
        let Some(parsed) = parse_nexus_url(url, Some(&self.website_origin))? else {
            return Ok(None);
        };
        if parsed.domain != game.provider_game_id {
            return Ok(None);
        }
        Ok(Some(ModRef {
            source_id: SOURCE_ID.to_owned(),
            remote_game_key: game.remote_game_key.clone(),
            remote_mod_id: parsed.mod_id.to_string(),
            version_id: parsed.file_id.map(|id| format!("file:{id}")),
            remote_file_id: parsed.file_id.map(|id| id.to_string()),
        }))
    }

    async fn search(
        &self,
        game: &SourceGameBinding,
        request: &SearchRequest,
        cancellation: &CancellationToken,
    ) -> Result<SearchPage, SourceError> {
        validate_search(request)?;
        let domain = provider_domain(game)?;
        let scope_hash = search_scope_hash(game, request)?;
        let offset = request
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, &scope_hash))
            .transpose()?
            .unwrap_or(0);
        let mut filters = vec![json!({
            "gameDomainName": [{"value": domain, "op": "EQUALS"}]
        })];
        if !request.query.trim().is_empty() {
            filters.push(search_query_filter(request.query.trim()));
        }
        let body = serde_json::to_vec(&json!({
            "query": SEARCH_QUERY,
            "variables": {
                "filter": {"op": "AND", "filter": filters},
                "sort": search_sort(request),
                "offset": offset,
                "count": request.page_size,
            }
        }))
        .map_err(|error| SourceError::InvalidResponse(error.to_string()))?;
        let response = self
            .raw_request(
                self.endpoint("v2/graphql")?,
                HttpMethod::Post,
                Some(body),
                cancellation,
            )
            .await?;
        let envelope: GraphqlEnvelope = parse_api_response(response, "Nexus search")?;
        let warnings = envelope
            .errors
            .iter()
            .map(|error| SourceWarning {
                code: "NEXUS_GRAPHQL_PARTIAL".to_owned(),
                message: error.message.clone(),
            })
            .collect::<Vec<_>>();
        let data = envelope.data.ok_or_else(|| {
            SourceError::InvalidResponse(if warnings.is_empty() {
                "Nexus GraphQL response has no data".to_owned()
            } else {
                "Nexus GraphQL search failed".to_owned()
            })
        })?;
        let mods = data.mods.ok_or_else(|| {
            SourceError::InvalidResponse("Nexus GraphQL response has no mods".to_owned())
        })?;
        let count = u64::try_from(mods.nodes.len())
            .map_err(|_| SourceError::InvalidResponse("Nexus page length overflow".to_owned()))?;
        let items = mods
            .nodes
            .into_iter()
            .map(|node| map_search_mod(self, game, node))
            .collect::<Result<Vec<_>, _>>()?;
        let next_offset = offset + count;
        let has_more = mods.total_count.is_some_and(|total| next_offset < total)
            || (mods.total_count.is_none() && count == u64::from(request.page_size));
        Ok(SearchPage {
            items,
            next_cursor: has_more
                .then(|| encode_cursor(&scope_hash, next_offset))
                .transpose()?,
            live_pagination: true,
            total: mods.total_count,
            warnings,
        })
    }

    async fn get_mod(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<SourceMod, SourceError> {
        let detail = self.detail(game, mod_ref, cancellation).await?;
        let files = self.files(game, mod_ref, cancellation).await?;
        map_detail(self, game, detail, select_default_file(files))
    }

    async fn list_versions(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceVersion>, SourceError> {
        self.files(game, mod_ref, cancellation)
            .await?
            .into_iter()
            .map(map_file)
            .collect()
    }

    async fn get_dependencies(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        _cancellation: &CancellationToken,
    ) -> Result<Vec<SourceDependency>, SourceError> {
        validate_ref(game, mod_ref)?;
        Err(SourceError::Unavailable(
            "Nexus does not expose dependency metadata through this API".to_owned(),
        ))
    }

    async fn resolve_download(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let files = self.files(game, mod_ref, cancellation).await?;
        let selected = select_exact_file(mod_ref, files)?;
        self.resolve_selected_download(game, mod_ref, selected, None, cancellation)
            .await
    }

    async fn begin_download(
        &self,
        job_id: &str,
        context: &InteractionContext,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadResolution, SourceError> {
        match NexusProvider::resolve_download_or_request_interaction(
            self,
            job_id,
            context,
            game,
            mod_ref,
            cancellation,
        )
        .await?
        {
            NexusDownloadOutcome::Ready(descriptor) => Ok(DownloadResolution::Ready(descriptor)),
            NexusDownloadOutcome::InteractionRequired(action) => {
                Ok(DownloadResolution::InteractionRequired(action))
            }
        }
    }

    async fn resume_download_action(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        NexusProvider::resume_download(self, action, game, mod_ref, cancellation).await
    }

    async fn resume_download_callback(
        &self,
        action: &ActionRequired,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        callback: Url,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        self.resume_download_from_callback(action, game, mod_ref, callback, cancellation).await
    }
}

#[derive(Debug)]
struct ParsedNexusUrl {
    domain: String,
    mod_id: u64,
    file_id: Option<u64>,
    key: Option<String>,
    expires: Option<i64>,
}

#[derive(Debug)]
struct DownloadCredentials {
    key: String,
    expires: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlEnvelope {
    data: Option<GraphqlData>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
    mods: Option<GraphqlMods>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlMods {
    #[serde(default)]
    nodes: Vec<GraphqlMod>,
    total_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlMod {
    mod_id: u64,
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    endorsements: u64,
    picture_url: Option<String>,
    thumbnail_url: Option<String>,
    thumbnail_large_url: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    game: Option<GraphqlGame>,
    uploader: Option<GraphqlUploader>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlGame {
    domain_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphqlUploader {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiMod {
    #[serde(alias = "id")]
    mod_id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    picture_url: Option<String>,
    #[serde(default)]
    mod_downloads: u64,
    #[serde(default)]
    endorsement_count: u64,
    #[serde(default)]
    author: String,
    #[serde(default)]
    uploaded_by: String,
    #[serde(default)]
    updated_time: i64,
    #[serde(default)]
    created_time: i64,
    domain_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiFile {
    #[serde(alias = "id")]
    file_id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    file_name: String,
    category_id: Option<i64>,
    category_name: Option<String>,
    #[serde(default)]
    is_primary: bool,
    #[serde(default)]
    uploaded_timestamp: i64,
    size: Option<u64>,
    size_kb: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ApiFilesResponse {
    Wrapped { files: Vec<ApiFile> },
    Bare(Vec<ApiFile>),
}

impl ApiFilesResponse {
    fn into_files(self) -> Vec<ApiFile> {
        match self {
            Self::Wrapped { files } | Self::Bare(files) => files,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ApiDownloadLink {
    #[serde(rename = "URI", alias = "uri")]
    uri: Option<String>,
}

fn parse_api_response<T: DeserializeOwned>(
    response: NetworkResponse,
    resource: &str,
) -> Result<T, SourceError> {
    match response.status {
        200..=299 => serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::InvalidResponse(error.to_string())),
        401 | 403 => Err(SourceError::Unauthorized),
        404 => Err(SourceError::NotFound(resource.to_owned())),
        429 => Err(SourceError::RateLimited),
        status => Err(SourceError::Unavailable(format!(
            "{resource} returned HTTP {status}"
        ))),
    }
}

fn validate_search(request: &SearchRequest) -> Result<(), SourceError> {
    if request.page_size == 0 || request.page_size > MAX_PAGE_SIZE {
        return Err(SourceError::InvalidArgument(format!(
            "Nexus pageSize must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    Ok(())
}

fn provider_domain(game: &SourceGameBinding) -> Result<&str, SourceError> {
    let domain = game.provider_game_id.trim();
    if domain.is_empty()
        || !domain
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(SourceError::InvalidArgument(
            "invalid Nexus game domain binding".to_owned(),
        ));
    }
    Ok(domain)
}

fn validate_ref(game: &SourceGameBinding, mod_ref: &ModRef) -> Result<u64, SourceError> {
    provider_domain(game)?;
    if mod_ref.source_id != SOURCE_ID || mod_ref.remote_game_key != game.remote_game_key {
        return Err(SourceError::InvalidArgument(
            "mod reference does not match the Nexus game binding".to_owned(),
        ));
    }
    let mod_id = mod_ref
        .remote_mod_id
        .parse::<u64>()
        .map_err(|_| SourceError::InvalidArgument("invalid Nexus mod id".to_owned()))?;
    if mod_id == 0 {
        return Err(SourceError::InvalidArgument(
            "invalid Nexus mod id".to_owned(),
        ));
    }
    Ok(mod_id)
}

fn parse_nexus_url(
    url: &Url,
    expected_website_origin: Option<&Origin>,
) -> Result<Option<ParsedNexusUrl>, SourceError> {
    if url.scheme() == "nxm" {
        let Some(domain) = url.host_str() else {
            return Ok(None);
        };
        let segments = url
            .path_segments()
            .map(|segments| segments.collect::<Vec<_>>())
            .unwrap_or_default();
        if segments.len() != 4 || segments[0] != "mods" || segments[2] != "files" {
            return Ok(None);
        }
        let mod_id = parse_url_id(segments[1])?;
        let file_id = parse_url_id(segments[3])?;
        let key = query_value(url, "key");
        let expires = query_value(url, "expires")
            .map(|value| {
                value.parse::<i64>().map_err(|_| {
                    SourceError::InvalidArgument("invalid Nexus callback expiry".to_owned())
                })
            })
            .transpose()?;
        return Ok(Some(ParsedNexusUrl {
            domain: domain.to_ascii_lowercase(),
            mod_id,
            file_id: Some(file_id),
            key,
            expires,
        }));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Ok(None);
    }
    let origin = Origin::from_url(url)?;
    let origin_allowed = expected_website_origin.is_some_and(|expected| expected == &origin)
        || (expected_website_origin.is_none()
            && matches!(url.host_str(), Some("nexusmods.com" | "www.nexusmods.com")));
    if !origin_allowed {
        return Ok(None);
    }
    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (domain, mod_segment, trailing_file) = match segments.as_slice() {
        ["games", domain, "mods", mod_id, rest @ ..] => (*domain, *mod_id, parse_path_file(rest)?),
        [domain, "mods", mod_id, rest @ ..] => (*domain, *mod_id, parse_path_file(rest)?),
        _ => return Ok(None),
    };
    let mod_id = parse_url_id(mod_segment)?;
    let query_file = query_value(url, "file_id")
        .map(|value| parse_url_id(&value))
        .transpose()?;
    Ok(Some(ParsedNexusUrl {
        domain: domain.to_ascii_lowercase(),
        mod_id,
        file_id: trailing_file.or(query_file),
        key: None,
        expires: None,
    }))
}

fn parse_path_file(segments: &[&str]) -> Result<Option<u64>, SourceError> {
    match segments {
        [] => Ok(None),
        ["files", file_id, ..] => parse_url_id(file_id).map(Some),
        _ => Ok(None),
    }
}

fn parse_url_id(value: &str) -> Result<u64, SourceError> {
    let id = value
        .parse::<u64>()
        .map_err(|_| SourceError::InvalidArgument("invalid Nexus URL identity".to_owned()))?;
    if id == 0 {
        return Err(SourceError::InvalidArgument(
            "invalid Nexus URL identity".to_owned(),
        ));
    }
    Ok(id)
}

fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
}

fn validate_callback_identity(
    callback: &ParsedNexusUrl,
    game: &SourceGameBinding,
    mod_ref: &ModRef,
    file: &ApiFile,
) -> Result<(), SourceError> {
    let mod_id = validate_ref(game, mod_ref)?;
    if callback.domain != game.provider_game_id
        || callback.mod_id != mod_id
        || callback.file_id != Some(file.file_id)
    {
        return Err(SourceError::InvalidArgument(
            "Nexus callback does not match the requested game, mod, and file".to_owned(),
        ));
    }
    Ok(())
}

fn map_search_mod(
    provider: &NexusProvider,
    game: &SourceGameBinding,
    node: GraphqlMod,
) -> Result<SourceMod, SourceError> {
    if node.mod_id == 0
        || node
            .game
            .as_ref()
            .and_then(|value| value.domain_name.as_deref())
            != Some(game.provider_game_id.as_str())
    {
        return Err(SourceError::InvalidResponse(
            "Nexus search result escaped the bound game".to_owned(),
        ));
    }
    let created_at = parse_optional_time(node.created_at.as_deref())?;
    let updated_at = parse_optional_time(node.updated_at.as_deref())?;
    let author = if node.author.trim().is_empty() {
        node.uploader
            .and_then(|uploader| uploader.name)
            .unwrap_or_default()
    } else {
        node.author
    };
    Ok(SourceMod {
        mod_ref: base_mod_ref(game, node.mod_id),
        name: node.name,
        summary: node.summary,
        description: node.description,
        author,
        profile_url: Some(provider.profile_url(game, node.mod_id)?.into()),
        logo_url: node
            .thumbnail_large_url
            .or(node.thumbnail_url)
            .or(node.picture_url),
        tags: Vec::new(),
        maturity_flags: 0,
        review_status: ReviewStatus::Accepted,
        submitted_at: created_at,
        updated_at,
        downloads: node.downloads,
        subscribers: 0,
        rating: node.endorsements as f64,
        latest_file: None,
    })
}

fn map_detail(
    provider: &NexusProvider,
    game: &SourceGameBinding,
    detail: ApiMod,
    latest: Option<ApiFile>,
) -> Result<SourceMod, SourceError> {
    let latest_file = latest.map(map_file).transpose()?;
    Ok(SourceMod {
        mod_ref: base_mod_ref(game, detail.mod_id),
        name: detail.name,
        summary: detail.summary,
        description: detail.description,
        author: if detail.author.trim().is_empty() {
            detail.uploaded_by
        } else {
            detail.author
        },
        profile_url: Some(provider.profile_url(game, detail.mod_id)?.into()),
        logo_url: detail.picture_url,
        tags: Vec::new(),
        maturity_flags: 0,
        review_status: ReviewStatus::Accepted,
        submitted_at: detail.created_time,
        updated_at: detail.updated_time,
        downloads: detail.mod_downloads,
        subscribers: 0,
        rating: detail.endorsement_count as f64,
        latest_file,
    })
}

fn map_file(file: ApiFile) -> Result<SourceVersion, SourceError> {
    if file.file_id == 0 {
        return Err(SourceError::InvalidResponse(
            "Nexus file identity is missing".to_owned(),
        ));
    }
    let display_version = if file.version.trim().is_empty() {
        file.name.clone()
    } else {
        file.version.clone()
    };
    let size = file_size(&file);
    let review_status = file_review_status(&file);
    Ok(SourceVersion {
        version_id: format!("file:{}", file.file_id),
        remote_file_id: file.file_id.to_string(),
        display_version,
        filename: file.file_name,
        file_size: size,
        md5: None,
        published_at: file.uploaded_timestamp,
        review_status,
    })
}

fn file_review_status(file: &ApiFile) -> ReviewStatus {
    let category = file
        .category_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(category.as_str(), "archived" | "removed") {
        ReviewStatus::Unavailable
    } else {
        ReviewStatus::Accepted
    }
}

fn select_exact_file(mod_ref: &ModRef, files: Vec<ApiFile>) -> Result<ApiFile, SourceError> {
    if let Some(remote_file_id) = &mod_ref.remote_file_id {
        let file_id = parse_url_id(remote_file_id)?;
        return files
            .into_iter()
            .find(|file| file.file_id == file_id)
            .ok_or_else(|| SourceError::NotFound(remote_file_id.clone()));
    }
    let version_id = mod_ref.version_id.as_deref().ok_or_else(|| {
        SourceError::InvalidArgument(
            "Nexus download resolution requires a versionId or remoteFileId".to_owned(),
        )
    })?;
    if let Some(file_id) = version_id.strip_prefix("file:") {
        let file_id = parse_url_id(file_id)?;
        return files
            .into_iter()
            .find(|file| file.file_id == file_id)
            .ok_or_else(|| SourceError::NotFound(version_id.to_owned()));
    }
    let mut matching = files.into_iter().filter(|file| file.version == version_id);
    let selected = matching
        .next()
        .ok_or_else(|| SourceError::NotFound(version_id.to_owned()))?;
    if matching.next().is_some() {
        return Err(SourceError::InvalidArgument(
            "Nexus display version is ambiguous; use the stable file version id".to_owned(),
        ));
    }
    Ok(selected)
}

fn select_default_file(files: Vec<ApiFile>) -> Option<ApiFile> {
    let mut downloadable = files
        .iter()
        .filter(|file| {
            !matches!(
                file.category_name
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "old_version" | "old versions" | "archived" | "removed"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    downloadable
        .iter()
        .find(|file| file.is_primary)
        .cloned()
        .or_else(|| {
            downloadable
                .iter()
                .find(|file| {
                    file.category_id == Some(1)
                        || file
                            .category_name
                            .as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case("main"))
                })
                .cloned()
        })
        .or_else(|| {
            downloadable.sort_by_key(|file| file.uploaded_timestamp);
            downloadable.pop()
        })
        .or_else(|| files.into_iter().next())
}

fn file_size(file: &ApiFile) -> u64 {
    let raw = file.size.or(file.size_kb).unwrap_or(0);
    if raw > 0 && raw < 1024 * 1024 {
        raw.saturating_mul(1024)
    } else {
        raw
    }
}

fn base_mod_ref(game: &SourceGameBinding, mod_id: u64) -> ModRef {
    ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: game.remote_game_key.clone(),
        remote_mod_id: mod_id.to_string(),
        version_id: None,
        remote_file_id: None,
    }
}

fn exact_mod_ref(mod_ref: &ModRef, file: &ApiFile) -> ModRef {
    ModRef {
        source_id: mod_ref.source_id.clone(),
        remote_game_key: mod_ref.remote_game_key.clone(),
        remote_mod_id: mod_ref.remote_mod_id.clone(),
        version_id: Some(format!("file:{}", file.file_id)),
        remote_file_id: Some(file.file_id.to_string()),
    }
}

fn parse_optional_time(value: Option<&str>) -> Result<i64, SourceError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|time| time.timestamp())
                .map_err(|_| SourceError::InvalidResponse("Nexus timestamp is invalid".to_owned()))
        })
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn search_sort(request: &SearchRequest) -> Vec<Value> {
    let direction = match request.sort_direction.unwrap_or(SortDirection::Descending) {
        SortDirection::Ascending => "ASC",
        SortDirection::Descending => "DESC",
    };
    let field = match request.sort_field {
        None if !request.query.trim().is_empty() => "relevance",
        Some(SearchSortField::Name) => "name",
        Some(SearchSortField::Downloads | SearchSortField::Subscribers) => "downloads",
        Some(SearchSortField::Rating) => "endorsements",
        Some(SearchSortField::Updated) | None => "updatedAt",
    };
    let mut object = serde_json::Map::new();
    object.insert(field.to_owned(), json!({"direction": direction}));
    vec![Value::Object(object)]
}

fn search_query_filter(query: &str) -> Value {
    json!({
        "op": "OR",
        "filter": [
            {"nameStemmed": [{"value": query, "op": "MATCHES"}]},
            {"name": [{"value": format!("*{query}*"), "op": "WILDCARD"}]},
            {"description": [{"value": query, "op": "MATCHES"}]},
            {"author": [{"value": query, "op": "MATCHES"}]},
            {"uploader": [{"value": query, "op": "MATCHES"}]}
        ]
    })
}

fn search_scope_hash(
    game: &SourceGameBinding,
    request: &SearchRequest,
) -> Result<String, SourceError> {
    let value = json!({
        "sourceId": SOURCE_ID,
        "gameId": game.game_id,
        "remoteGameKey": game.remote_game_key,
        "providerGameId": game.provider_game_id,
        "query": request.query.trim(),
        "pageSize": request.page_size,
        "sortField": request.sort_field,
        "sortDirection": request.sort_direction,
    });
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(
            serde_json::to_vec(&value)
                .map_err(|error| SourceError::InvalidResponse(error.to_string()))?
        )
    ))
}

fn encode_cursor(scope_hash: &str, offset: u64) -> Result<String, SourceError> {
    serde_json::to_vec(&json!({"scopeHash": scope_hash, "offset": offset}))
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|error| SourceError::InvalidResponse(error.to_string()))
}

fn decode_cursor(cursor: &str, expected_scope: &str) -> Result<u64, SourceError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Cursor {
        scope_hash: String,
        offset: u64,
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| SourceError::InvalidArgument("invalid Nexus cursor".to_owned()))?;
    let cursor: Cursor = serde_json::from_slice(&bytes)
        .map_err(|_| SourceError::InvalidArgument("invalid Nexus cursor".to_owned()))?;
    if cursor.scope_hash != expected_scope {
        return Err(SourceError::InvalidArgument(
            "Nexus cursor does not match the query".to_owned(),
        ));
    }
    Ok(cursor.offset)
}

fn now_seconds() -> Result<i64, SourceError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SourceError::InvalidResponse(error.to_string()))?;
    i64::try_from(elapsed.as_secs())
        .map_err(|_| SourceError::InvalidResponse("timestamp overflow".to_owned()))
}

fn map_network_error(error: BrokerError) -> SourceError {
    match error {
        BrokerError::CredentialUnavailable(_) => SourceError::Unauthorized,
        other => SourceError::Network(other),
    }
}

#[cfg(test)]
mod tests;
