use std::collections::HashSet;

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use mintcat_runtime_broker::{
    BrokerError, CancellationToken, CredentialBinding, CredentialPlacement, CredentialRef,
    NetworkRequest, Origin, ProxyPolicy,
};
use mintcat_sdk::ModRef;
use mintcat_source_api::{
    DownloadDescriptor, ReviewStatus, SearchPage, SearchRequest, SearchSortField, SortDirection,
    SourceDependency, SourceError, SourceGameBinding, SourceMod, SourceNetworkClient, SourceProvider, SourceVersion,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use url::Url;

pub const SOURCE_ID: &str = "modio";
const MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone)]
pub struct ModioConfig {
    pub api_base: Url,
    pub credential: Option<CredentialRef>,
    pub proxy_policy: ProxyPolicy,
    pub download_origins: Vec<Origin>,
}

#[derive(Clone)]
pub struct ModioProvider {
    broker: SourceNetworkClient,
    config: ModioConfig,
    api_origin: Origin,
}

impl ModioProvider {
    pub fn new(broker: impl Into<SourceNetworkClient>, config: ModioConfig) -> Result<Self, SourceError> {
        let api_origin = Origin::from_url(&config.api_base)?;
        if config.download_origins.is_empty() {
            return Err(SourceError::InvalidArgument(
                "mod.io download origin scope cannot be empty".to_owned(),
            ));
        }
        Ok(Self {
            broker: broker.into(),
            config,
            api_origin,
        })
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        url: Url,
        cancellation: &CancellationToken,
    ) -> Result<T, SourceError> {
        let mut request = NetworkRequest::get("source.modio", url, vec![self.api_origin.clone()]);
        request.proxy_policy = self.config.proxy_policy;
        if let Some(reference) = &self.config.credential {
            request.credential = Some(CredentialBinding {
                reference: reference.clone(),
                placement: CredentialPlacement::Bearer,
                allowed_origins: vec![self.api_origin.clone()],
            });
        }
        let response = self
            .broker
            .execute(&request, cancellation)
            .await
            .map_err(map_network_error)?;
        serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::InvalidResponse(error.to_string()))
    }

    fn endpoint(&self, path: &str) -> Result<Url, SourceError> {
        self.config
            .api_base
            .join(path)
            .map_err(|_| SourceError::InvalidArgument("invalid mod.io endpoint".to_owned()))
    }

    async fn resolve_mod_id(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<u64, SourceError> {
        validate_ref(game, mod_ref)?;
        if let Ok(id) = mod_ref.remote_mod_id.parse::<u64>() {
            return Ok(id);
        }
        let mut endpoint = self.endpoint(&format!("games/{}/mods", provider_game_id(game)?))?;
        endpoint
            .query_pairs_mut()
            .append_pair("name_id", &mod_ref.remote_mod_id)
            .append_pair("_limit", "2");
        let response: ApiPage<ApiMod> = self.request_json(endpoint, cancellation).await?;
        let mut matches = response
            .data
            .into_iter()
            .filter(|item| item.name_id == mod_ref.remote_mod_id);
        let item = matches
            .next()
            .ok_or_else(|| SourceError::NotFound(mod_ref.remote_mod_id.clone()))?;
        if matches.next().is_some() {
            return Err(SourceError::InvalidResponse(
                "mod.io name identity is not unique".to_owned(),
            ));
        }
        Ok(item.id)
    }

    async fn files(
        &self,
        game: &SourceGameBinding,
        mod_id: u64,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ApiFile>, SourceError> {
        let endpoint = self.endpoint(&format!(
            "games/{}/mods/{mod_id}/files?_limit=100",
            provider_game_id(game)?
        ))?;
        let response: ApiPage<ApiFile> = self.request_json(endpoint, cancellation).await?;
        if response.data.iter().any(|file| file.mod_id != mod_id) {
            return Err(SourceError::InvalidResponse("mod.io files crossed the requested mod identity".into()));
        }
        Ok(response.data)
    }
}

#[async_trait]
impl SourceProvider for ModioProvider {
    fn source_id(&self) -> &'static str {
        SOURCE_ID
    }

    fn parse_url(
        &self,
        game: &SourceGameBinding,
        url: &Url,
    ) -> Result<Option<ModRef>, SourceError> {
        if !matches!(url.host_str(), Some("mod.io" | "www.mod.io")) {
            return Ok(None);
        }
        let segments = url
            .path_segments()
            .map(Iterator::collect::<Vec<_>>)
            .unwrap_or_default();
        if segments.len() != 4
            || segments[0] != "g"
            || segments[1] != game.remote_game_key
            || segments[2] != "m"
            || segments[3].is_empty()
        {
            return Ok(None);
        }
        Ok(Some(ModRef {
            source_id: SOURCE_ID.to_owned(),
            remote_game_key: game.remote_game_key.clone(),
            remote_mod_id: segments[3].to_owned(),
            version_id: None,
            remote_file_id: None,
        }))
    }

    async fn search(
        &self,
        game: &SourceGameBinding,
        request: &SearchRequest,
        cancellation: &CancellationToken,
    ) -> Result<SearchPage, SourceError> {
        validate_search(request)?;
        let scope_hash = search_scope_hash(game, request)?;
        let offset = request
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, &scope_hash))
            .transpose()?
            .unwrap_or(0);
        let provider_game_id = provider_game_id(game)?;
        let mut endpoint = self.endpoint(&format!("games/{provider_game_id}/mods"))?;
        {
            let mut query = endpoint.query_pairs_mut();
            query
                .append_pair("_limit", &request.page_size.to_string())
                .append_pair("_offset", &offset.to_string());
            if !request.query.trim().is_empty() {
                query.append_pair("name-lk", &format!("*{}*", request.query.trim()));
            }
            if let Some(sort) = modio_sort(request)? {
                query.append_pair("_sort", sort);
            }
        }
        let response: ApiPage<ApiMod> = self.request_json(endpoint, cancellation).await?;
        let count = u64::try_from(response.data.len())
            .map_err(|_| SourceError::InvalidResponse("mod.io page length overflow".to_owned()))?;
        let items = response
            .data
            .into_iter()
            .map(|item| map_mod(game, item))
            .collect::<Result<Vec<_>, _>>()?;
        let next_offset = offset + count;
        let has_more = response
            .result_total
            .is_some_and(|total| next_offset < total)
            || (response.result_total.is_none() && count == u64::from(request.page_size));
        Ok(SearchPage {
            items,
            next_cursor: has_more
                .then(|| encode_cursor(&scope_hash, next_offset))
                .transpose()?,
            live_pagination: true,
            total: response.result_total,
            warnings: Vec::new(),
        })
    }

    async fn get_mod(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<SourceMod, SourceError> {
        let mod_id = self.resolve_mod_id(game, mod_ref, cancellation).await?;
        let endpoint =
            self.endpoint(&format!("games/{}/mods/{mod_id}", provider_game_id(game)?))?;
        let item: ApiMod = self.request_json(endpoint, cancellation).await?;
        if item.id != mod_id {
            return Err(SourceError::InvalidResponse("mod.io metadata crossed the requested mod identity".into()));
        }
        let mapped = map_mod(game, item)?;
        if mapped.review_status != ReviewStatus::Accepted {
            return Err(SourceError::Unavailable(mapped.mod_ref.remote_mod_id));
        }
        Ok(mapped)
    }

    async fn list_versions(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceVersion>, SourceError> {
        let mod_id = self.resolve_mod_id(game, mod_ref, cancellation).await?;
        self.files(game, mod_id, cancellation)
            .await?
            .into_iter()
            .map(map_file)
            .collect()
    }

    async fn get_dependencies(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceDependency>, SourceError> {
        let mod_id = self.resolve_mod_id(game, mod_ref, cancellation).await?;
        let endpoint = self.endpoint(&format!(
            "games/{}/mods/{mod_id}/dependencies?_limit=100",
            provider_game_id(game)?
        ))?;
        let response: ApiPage<ApiMod> = self.request_json(endpoint, cancellation).await?;
        response
            .data
            .into_iter()
            .map(|dependency| {
                if dependency.game_id.to_string() != game.provider_game_id {
                    return Err(SourceError::InvalidResponse(
                        "mod.io dependency crossed the game binding".to_owned(),
                    ));
                }
                Ok(SourceDependency {
                    mod_ref: base_mod_ref(game, dependency.id),
                    required: true,
                })
            })
            .collect()
    }

    async fn resolve_download(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let mod_id = self.resolve_mod_id(game, mod_ref, cancellation).await?;
        let files = self.files(game, mod_id, cancellation).await?;
        let selected = select_file(mod_ref, files)?;
        let version = map_file(selected.clone())?;
        if version.review_status != ReviewStatus::Accepted {
            return Err(SourceError::Unavailable(version.remote_file_id));
        }
        let download = selected.download.ok_or_else(|| {
            SourceError::Unavailable(format!("file {} has no download", selected.id))
        })?;
        let url = Url::parse(&download.binary_url)
            .map_err(|_| SourceError::InvalidResponse("invalid download URL".to_owned()))?;
        let download_origin = Origin::from_url(&url)?;
        if !self.config.download_origins.contains(&download_origin) {
            return Err(SourceError::InvalidResponse(
                "download URL escaped the configured origin scope".to_owned(),
            ));
        }
        Ok(DownloadDescriptor {
            mod_ref: ModRef {
                source_id: SOURCE_ID.to_owned(),
                remote_game_key: game.remote_game_key.clone(),
                remote_mod_id: mod_id.to_string(),
                version_id: Some(version.version_id),
                remote_file_id: Some(version.remote_file_id),
            },
            url,
            headers: Default::default(),
            credential: None,
            allowed_origins: self.config.download_origins.clone(),
            expires_at: download.date_expires,
            expected_size: version.file_size,
            expected_md5: version.md5,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ApiPage<T> {
    data: Vec<T>,
    #[serde(default)]
    result_total: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiMod {
    id: u64,
    game_id: u64,
    #[serde(default)]
    status: i64,
    #[serde(default)]
    visible: i64,
    #[serde(default)]
    date_added: i64,
    #[serde(default)]
    date_updated: i64,
    #[serde(default)]
    maturity_option: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    name_id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description_plaintext: String,
    #[serde(default)]
    profile_url: Option<String>,
    #[serde(default)]
    submitted_by: ApiUser,
    #[serde(default)]
    logo: ApiLogo,
    #[serde(default)]
    modfile: Option<ApiFile>,
    #[serde(default)]
    tags: Vec<ApiTag>,
    #[serde(default)]
    stats: ApiStats,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ApiUser {
    #[serde(default)]
    username: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ApiLogo {
    #[serde(default)]
    thumb_640x360: Option<String>,
    #[serde(default)]
    original: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiFile {
    id: u64,
    mod_id: u64,
    #[serde(default)]
    date_added: i64,
    #[serde(default)]
    virus_status: i64,
    #[serde(default)]
    virus_positive: i64,
    #[serde(default)]
    filesize: u64,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    filehash: ApiFileHash,
    #[serde(default)]
    download: Option<ApiDownload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ApiFileHash {
    #[serde(default)]
    md5: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiDownload {
    binary_url: String,
    #[serde(default)]
    date_expires: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiTag {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ApiStats {
    #[serde(default)]
    downloads_total: u64,
    #[serde(default)]
    subscribers_total: u64,
    #[serde(default)]
    ratings_weighted_aggregate: f64,
}

fn provider_game_id(game: &SourceGameBinding) -> Result<u64, SourceError> {
    game.provider_game_id.parse::<u64>().map_err(|_| {
        SourceError::InvalidArgument("mod.io provider game id must be numeric".to_owned())
    })
}

fn validate_ref(game: &SourceGameBinding, mod_ref: &ModRef) -> Result<(), SourceError> {
    if mod_ref.source_id != SOURCE_ID || mod_ref.remote_game_key != game.remote_game_key {
        return Err(SourceError::InvalidArgument(
            "ModRef does not match the mod.io game binding".to_owned(),
        ));
    }
    if mod_ref.remote_mod_id.trim().is_empty() {
        return Err(SourceError::InvalidArgument(
            "mod.io remote mod id cannot be empty".to_owned(),
        ));
    }
    Ok(())
}

fn validate_search(request: &SearchRequest) -> Result<(), SourceError> {
    if request.page_size == 0 || request.page_size > MAX_PAGE_SIZE {
        return Err(SourceError::InvalidArgument(format!(
            "mod.io page size must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    if request.sort_field.is_some() != request.sort_direction.is_some() {
        return Err(SourceError::InvalidArgument(
            "sort field and direction must be provided together".to_owned(),
        ));
    }
    Ok(())
}

fn modio_sort(request: &SearchRequest) -> Result<Option<&'static str>, SourceError> {
    let Some(field) = request.sort_field else {
        return Ok(None);
    };
    let descending = request.sort_direction == Some(SortDirection::Descending);
    Ok(Some(match (field, descending) {
        (SearchSortField::Updated, true) => "-date_updated",
        (SearchSortField::Updated, false) => "date_updated",
        (SearchSortField::Downloads, true) => "-downloads_total",
        (SearchSortField::Downloads, false) => "downloads_total",
        (SearchSortField::Subscribers, true) => "-subscribers_total",
        (SearchSortField::Subscribers, false) => "subscribers_total",
        (SearchSortField::Rating, true) => "-ratings_weighted_aggregate",
        (SearchSortField::Rating, false) => "ratings_weighted_aggregate",
        (SearchSortField::Name, true) => "-name",
        (SearchSortField::Name, false) => "name",
    }))
}

fn map_mod(game: &SourceGameBinding, item: ApiMod) -> Result<SourceMod, SourceError> {
    if item.game_id.to_string() != game.provider_game_id {
        return Err(SourceError::InvalidResponse(
            "mod.io response crossed the game binding".to_owned(),
        ));
    }
    if item.modfile.as_ref().is_some_and(|file| file.mod_id != item.id) {
        return Err(SourceError::InvalidResponse("mod.io latest file crossed the requested mod identity".into()));
    }
    let latest_file = item.modfile.map(map_file).transpose()?;
    let mut tags = item
        .tags
        .into_iter()
        .map(|tag| tag.name.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    tags.retain(|tag| seen.insert(tag.to_ascii_lowercase()));
    Ok(SourceMod {
        mod_ref: base_mod_ref(game, item.id),
        name: item.name,
        summary: item.summary,
        description: item.description_plaintext,
        author: item.submitted_by.username,
        profile_url: item.profile_url,
        logo_url: item.logo.thumb_640x360.or(item.logo.original),
        tags,
        maturity_flags: item.maturity_option,
        review_status: map_mod_status(item.status, item.visible),
        submitted_at: item.date_added,
        updated_at: item.date_updated,
        downloads: item.stats.downloads_total,
        subscribers: item.stats.subscribers_total,
        rating: item.stats.ratings_weighted_aggregate,
        latest_file,
    })
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

fn map_file(file: ApiFile) -> Result<SourceVersion, SourceError> {
    if file.mod_id == 0 || file.id == 0 {
        return Err(SourceError::InvalidResponse(
            "mod.io file identity is missing".to_owned(),
        ));
    }
    Ok(SourceVersion {
        version_id: format!("file:{}", file.id),
        remote_file_id: file.id.to_string(),
        display_version: file.version,
        filename: file.filename,
        file_size: file.filesize,
        md5: file.filehash.md5.filter(|value| !value.trim().is_empty()),
        published_at: file.date_added,
        review_status: map_file_status(file.virus_status, file.virus_positive),
    })
}

fn map_mod_status(status: i64, visible: i64) -> ReviewStatus {
    match (status, visible) {
        (1, 1) => ReviewStatus::Accepted,
        (0, _) => ReviewStatus::Pending,
        (value, _) if value < 0 => ReviewStatus::Rejected,
        _ => ReviewStatus::Unavailable,
    }
}

fn map_file_status(virus_status: i64, virus_positive: i64) -> ReviewStatus {
    if virus_positive > 0 {
        ReviewStatus::Rejected
    } else {
        match virus_status {
            1 => ReviewStatus::Accepted,
            0 | 2 => ReviewStatus::Pending,
            _ => ReviewStatus::Unavailable,
        }
    }
}

fn select_file(mod_ref: &ModRef, files: Vec<ApiFile>) -> Result<ApiFile, SourceError> {
    if let Some(remote_file_id) = &mod_ref.remote_file_id {
        return files
            .into_iter()
            .find(|file| file.id.to_string() == *remote_file_id)
            .ok_or_else(|| SourceError::NotFound(remote_file_id.clone()));
    }
    let version_id = mod_ref.version_id.as_deref().ok_or_else(|| {
        SourceError::InvalidArgument(
            "download resolution requires a versionId or remoteFileId".to_owned(),
        )
    })?;
    if let Some(file_id) = version_id.strip_prefix("file:") {
        return files
            .into_iter()
            .find(|file| file.id.to_string() == file_id)
            .ok_or_else(|| SourceError::NotFound(version_id.to_owned()));
    }
    let mut matching = files.into_iter().filter(|file| file.version == version_id);
    let selected = matching
        .next()
        .ok_or_else(|| SourceError::NotFound(version_id.to_owned()))?;
    if matching.next().is_some() {
        return Err(SourceError::InvalidArgument(
            "display version is ambiguous; use the stable file version id".to_owned(),
        ));
    }
    Ok(selected)
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
    let value = json!({"scopeHash": scope_hash, "offset": offset});
    serde_json::to_vec(&value)
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
        .map_err(|_| SourceError::InvalidArgument("invalid mod.io cursor".to_owned()))?;
    let cursor: Cursor = serde_json::from_slice(&bytes)
        .map_err(|_| SourceError::InvalidArgument("invalid mod.io cursor".to_owned()))?;
    if cursor.scope_hash != expected_scope {
        return Err(SourceError::InvalidArgument(
            "mod.io cursor does not match the query".to_owned(),
        ));
    }
    Ok(cursor.offset)
}

fn map_network_error(error: BrokerError) -> SourceError {
    match error {
        BrokerError::Http {
            status: 401 | 403, ..
        }
        | BrokerError::CredentialUnavailable(_) => SourceError::Unauthorized,
        BrokerError::Http { status: 404, .. } => {
            SourceError::NotFound("mod.io resource".to_owned())
        }
        BrokerError::Http { status: 429, .. } => SourceError::RateLimited,
        other => SourceError::Network(other),
    }
}

#[cfg(test)]
mod tests;
