use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, NaiveDateTime};
use mintcat_runtime_broker::{
    BrokerError, CancellationToken, CredentialBinding, CredentialPlacement, CredentialRef,
    HttpMethod, NetworkRequest, Origin, ProxyPolicy,
};
use mintcat_sdk::ModRef;
use mintcat_source_api::{
    DownloadDescriptor, ReviewStatus, SearchPage, SearchRequest, SourceDependency, SourceError,
    SourceGameBinding, SourceMod, SourceNetworkClient, SourceProvider, SourceVersion,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use url::{form_urlencoded, Url};

pub const SOURCE_ID: &str = "modcat";
const MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone)]
pub struct ModcatConfig {
    pub api_base: Url,
    pub credential: Option<CredentialRef>,
    pub proxy_policy: ProxyPolicy,
    pub download_request_origin: Option<Url>,
}

#[derive(Clone)]
pub struct ModcatProvider {
    broker: SourceNetworkClient,
    config: ModcatConfig,
    api_origin: Origin,
}

impl ModcatProvider {
    pub fn new(broker: impl Into<SourceNetworkClient>, config: ModcatConfig) -> Result<Self, SourceError> {
        let api_origin = Origin::from_url(&config.api_base)?;
        if let Some(origin) = &config.download_request_origin {
            Origin::from_url(origin)?;
        }
        Ok(Self {
            broker: broker.into(),
            config,
            api_origin,
        })
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        cancellation: &CancellationToken,
    ) -> Result<T, SourceError> {
        let url = self
            .config
            .api_base
            .join(path.trim_start_matches('/'))
            .map_err(|_| SourceError::InvalidArgument("invalid ModCat endpoint".to_owned()))?;
        let mut request = NetworkRequest::get("source.modcat", url, vec![self.api_origin.clone()]);
        request.method = HttpMethod::Post;
        request
            .headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        request.body = Some(
            serde_json::to_vec(body)
                .map_err(|error| SourceError::InvalidResponse(error.to_string()))?
                .into(),
        );
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
        let envelope: ApiEnvelope<T> = serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::InvalidResponse(error.to_string()))?;
        match envelope.result_code {
            200 => envelope.result_data.ok_or_else(|| {
                SourceError::InvalidResponse("ModCat response has no ResultData".to_owned())
            }),
            401 | 403 => Err(SourceError::Unauthorized),
            404 => Err(SourceError::NotFound(
                envelope
                    .result_msg
                    .unwrap_or_else(|| "ModCat resource".to_owned()),
            )),
            429 => Err(SourceError::RateLimited),
            _ => Err(SourceError::Unavailable(
                envelope
                    .result_msg
                    .unwrap_or_else(|| format!("ModCat result code {}", envelope.result_code)),
            )),
        }
    }

    async fn detail(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<ApiMod, SourceError> {
        validate_ref(game, mod_ref)?;
        let detail: ApiMod = self
            .post(
                "api/Mod/ModDetail",
                &json!({"ModId": mod_ref.remote_mod_id}),
                cancellation,
            )
            .await?;
        validate_detail_game(game, &detail)?;
        if detail.mod_id.as_deref() != Some(mod_ref.remote_mod_id.as_str()) {
            return Err(SourceError::InvalidResponse(
                "ModCat detail returned a different ModId".to_owned(),
            ));
        }
        Ok(detail)
    }
}

#[async_trait]
impl SourceProvider for ModcatProvider {
    fn source_id(&self) -> &'static str {
        SOURCE_ID
    }

    fn parse_url(
        &self,
        game: &SourceGameBinding,
        url: &Url,
    ) -> Result<Option<ModRef>, SourceError> {
        if !matches!(url.host_str(), Some("modcat.top" | "www.modcat.top")) {
            return Ok(None);
        }
        let query_id = url
            .query_pairs()
            .find(|(name, _)| name.eq_ignore_ascii_case("ModId"))
            .map(|(_, value)| value.into_owned());
        let fragment_id = url.fragment().and_then(|fragment| {
            fragment
                .split_once('?')
                .map(|(_, query)| query)
                .and_then(|query| {
                    form_urlencoded::parse(query.as_bytes())
                        .find(|(name, _)| name.eq_ignore_ascii_case("ModId"))
                        .map(|(_, value)| value.into_owned())
                })
        });
        let Some(remote_mod_id) = query_id.or(fragment_id) else {
            return Ok(None);
        };
        if remote_mod_id.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(ModRef {
            source_id: SOURCE_ID.to_owned(),
            remote_game_key: game.remote_game_key.clone(),
            remote_mod_id,
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
        let response: Vec<ApiListMod> = self
            .post(
                "api/Mod/ModListPage",
                &json!({
                    "Skip": offset.to_string(),
                    "Take": request.page_size.to_string(),
                    "Search": request.query.trim(),
                    "GameId": game.provider_game_id,
                }),
                cancellation,
            )
            .await?;
        let count = u64::try_from(response.len())
            .map_err(|_| SourceError::InvalidResponse("ModCat page length overflow".to_owned()))?;
        let items = response
            .into_iter()
            .map(|item| map_list_mod(game, item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchPage {
            items,
            next_cursor: (count == u64::from(request.page_size))
                .then(|| encode_cursor(&scope_hash, offset + count))
                .transpose()?,
            live_pagination: true,
            total: None,
            warnings: Vec::new(),
        })
    }

    async fn get_mod(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<SourceMod, SourceError> {
        let detail = self.detail(game, mod_ref, cancellation).await?;
        if detail.soft_deleted {
            return Err(SourceError::Unavailable(mod_ref.remote_mod_id.clone()));
        }
        map_detail(game, detail)
    }

    async fn list_versions(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceVersion>, SourceError> {
        self.detail(game, mod_ref, cancellation)
            .await?
            .versions
            .into_iter()
            .map(map_version)
            .collect()
    }

    async fn get_dependencies(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SourceDependency>, SourceError> {
        self.detail(game, mod_ref, cancellation)
            .await?
            .dependencies
            .into_iter()
            .map(|dependency| map_dependency(game, dependency))
            .collect()
    }

    async fn resolve_download(
        &self,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
        cancellation: &CancellationToken,
    ) -> Result<DownloadDescriptor, SourceError> {
        let detail = self.detail(game, mod_ref, cancellation).await?;
        if detail.soft_deleted {
            return Err(SourceError::Unavailable(mod_ref.remote_mod_id.clone()));
        }
        let version = select_version(mod_ref, detail.versions)?;
        let mapped = map_version(version.clone())?;
        if mapped.review_status != ReviewStatus::Accepted {
            return Err(SourceError::Unavailable(mapped.version_id));
        }
        let file_id = version.files_id.ok_or_else(|| {
            SourceError::Unavailable(format!("version {} has no file", mapped.version_id))
        })?;
        let mut url = self
            .config
            .api_base
            .join("api/Files/DownloadFileGet")
            .map_err(|_| SourceError::InvalidResponse("invalid download endpoint".to_owned()))?;
        url.query_pairs_mut()
            .append_pair("FileId", &file_id)
            .append_pair("NoCount", "true");
        let mut headers = BTreeMap::new();
        if let Some(origin) = &self.config.download_request_origin {
            headers.insert("origin".to_owned(), origin.origin().ascii_serialization());
            headers.insert("referer".to_owned(), origin.as_str().to_owned());
        }
        let credential = self
            .config
            .credential
            .as_ref()
            .map(|reference| CredentialBinding {
                reference: reference.clone(),
                placement: CredentialPlacement::Bearer,
                allowed_origins: vec![self.api_origin.clone()],
            });
        Ok(DownloadDescriptor {
            mod_ref: ModRef {
                source_id: SOURCE_ID.to_owned(),
                remote_game_key: game.remote_game_key.clone(),
                remote_mod_id: mod_ref.remote_mod_id.clone(),
                version_id: Some(mapped.version_id),
                remote_file_id: Some(file_id),
            },
            url,
            allowed_origins: vec![self.api_origin.clone()],
            headers,
            credential,
            expires_at: None,
            expected_size: mapped.file_size,
            expected_md5: None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    #[serde(rename = "ResultCode")]
    result_code: i64,
    #[serde(rename = "ResultMsg", default)]
    result_msg: Option<String>,
    #[serde(rename = "ResultData")]
    result_data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ApiListMod {
    #[serde(rename = "ModId")]
    mod_id: Option<String>,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "PicUrl", default)]
    picture_url: Option<String>,
    #[serde(rename = "ModTypeEntities", default)]
    types: Vec<ApiListType>,
    #[serde(rename = "AVGPoint", default)]
    average_rating: Option<f64>,
    #[serde(rename = "DownloadCount", default)]
    downloads: u64,
    #[serde(rename = "CreatorNickName", default)]
    creator_name: String,
}

#[derive(Debug, Deserialize)]
struct ApiListType {
    #[serde(rename = "TypeName", default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct ApiMod {
    #[serde(rename = "ModId")]
    mod_id: Option<String>,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Description", default)]
    description: String,
    #[serde(rename = "CreatedAt", default)]
    created_at: Option<String>,
    #[serde(rename = "UpdatedAt", default)]
    updated_at: Option<String>,
    #[serde(rename = "DownloadCount", default)]
    downloads: u64,
    #[serde(rename = "SoftDeleted", default)]
    soft_deleted: bool,
    #[serde(rename = "PicUrl", default)]
    picture_url: Option<String>,
    #[serde(rename = "GameId", default)]
    game_id: Option<String>,
    #[serde(rename = "ModTypeEntities", default)]
    types: Vec<ApiModType>,
    #[serde(rename = "ModVersionEntities", default)]
    versions: Vec<ApiVersion>,
    #[serde(rename = "CreatorEntity", default)]
    creator: Option<ApiUser>,
    #[serde(rename = "ModDependenceEntities", default)]
    dependencies: Vec<ApiDependency>,
    #[serde(rename = "AVGPoint", default)]
    average_rating: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ApiModType {
    #[serde(rename = "Types", default)]
    item: Option<ApiType>,
}

#[derive(Debug, Deserialize)]
struct ApiType {
    #[serde(rename = "TypeName", default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct ApiUser {
    #[serde(rename = "NickName", default)]
    nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiVersion {
    #[serde(rename = "VersionId")]
    version_id: Option<String>,
    #[serde(rename = "ModId")]
    mod_id: Option<String>,
    #[serde(rename = "VersionNumber", default)]
    version_number: String,
    #[serde(rename = "FilesId")]
    files_id: Option<String>,
    #[serde(rename = "CreatedAt", default)]
    created_at: Option<String>,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "Files", default)]
    file: Option<ApiFile>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiFile {
    #[serde(rename = "FilesId")]
    files_id: Option<String>,
    #[serde(rename = "FilesName", default)]
    filename: String,
    #[serde(rename = "Size", default)]
    size: String,
    #[serde(rename = "SoftDeleted", default)]
    soft_deleted: bool,
}

#[derive(Debug, Deserialize)]
struct ApiDependency {
    #[serde(rename = "ModIOURL", default)]
    modio_url: Option<String>,
    #[serde(rename = "DependenceModVersion", default)]
    version: Option<ApiVersion>,
}

fn validate_ref(game: &SourceGameBinding, mod_ref: &ModRef) -> Result<(), SourceError> {
    if mod_ref.source_id != SOURCE_ID
        || mod_ref.remote_game_key != game.remote_game_key
        || mod_ref.remote_mod_id.trim().is_empty()
    {
        return Err(SourceError::InvalidArgument(
            "ModRef does not match the ModCat game binding".to_owned(),
        ));
    }
    Ok(())
}

fn validate_detail_game(game: &SourceGameBinding, detail: &ApiMod) -> Result<(), SourceError> {
    if detail.game_id.as_deref() != Some(game.provider_game_id.as_str()) {
        return Err(SourceError::InvalidResponse(
            "ModCat detail crossed the game binding".to_owned(),
        ));
    }
    Ok(())
}

fn validate_search(request: &SearchRequest) -> Result<(), SourceError> {
    if request.page_size == 0 || request.page_size > MAX_PAGE_SIZE {
        return Err(SourceError::InvalidArgument(format!(
            "ModCat page size must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    if request.sort_field.is_some() || request.sort_direction.is_some() {
        return Err(SourceError::InvalidArgument(
            "ModCat does not expose stable server-side sorting".to_owned(),
        ));
    }
    Ok(())
}

fn map_list_mod(game: &SourceGameBinding, item: ApiListMod) -> Result<SourceMod, SourceError> {
    let remote_mod_id = required(item.mod_id, "ModId")?;
    let mut tags = item
        .types
        .into_iter()
        .map(|item| item.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    deduplicate(&mut tags);
    Ok(SourceMod {
        mod_ref: base_mod_ref(game, remote_mod_id.clone()),
        name: item.name,
        summary: String::new(),
        description: String::new(),
        author: item.creator_name,
        profile_url: Some(format!(
            "https://modcat.top/#/modDetail?ModId={remote_mod_id}"
        )),
        logo_url: item.picture_url,
        tags,
        maturity_flags: 0,
        review_status: ReviewStatus::Accepted,
        submitted_at: 0,
        updated_at: 0,
        downloads: item.downloads,
        subscribers: 0,
        rating: item.average_rating.unwrap_or_default(),
        latest_file: None,
    })
}

fn map_detail(game: &SourceGameBinding, detail: ApiMod) -> Result<SourceMod, SourceError> {
    let remote_mod_id = required(detail.mod_id, "ModId")?;
    let latest_file = detail
        .versions
        .iter()
        .filter(|version| version_review(&version.status) == ReviewStatus::Accepted)
        .max_by_key(|version| {
            (
                parse_timestamp(version.created_at.as_deref()),
                version.version_id.as_deref().unwrap_or_default(),
            )
        })
        .cloned()
        .map(map_version)
        .transpose()?;
    let mut tags = detail
        .types
        .into_iter()
        .filter_map(|item| item.item)
        .map(|item| item.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    deduplicate(&mut tags);
    Ok(SourceMod {
        mod_ref: base_mod_ref(game, remote_mod_id.clone()),
        name: detail.name,
        summary: detail.description.clone(),
        description: detail.description,
        author: detail.creator.map(|user| user.nickname).unwrap_or_default(),
        profile_url: Some(format!(
            "https://modcat.top/#/modDetail?ModId={remote_mod_id}"
        )),
        logo_url: detail.picture_url,
        tags,
        maturity_flags: 0,
        review_status: ReviewStatus::Accepted,
        submitted_at: parse_timestamp(detail.created_at.as_deref()),
        updated_at: parse_timestamp(detail.updated_at.as_deref()),
        downloads: detail.downloads,
        subscribers: 0,
        rating: detail.average_rating.unwrap_or_default(),
        latest_file,
    })
}

fn map_version(version: ApiVersion) -> Result<SourceVersion, SourceError> {
    let version_id = required(version.version_id, "VersionId")?;
    let file_id = version
        .files_id
        .or_else(|| version.file.as_ref().and_then(|file| file.files_id.clone()))
        .ok_or_else(|| SourceError::InvalidResponse("ModCat FilesId is missing".to_owned()))?;
    let file = version.file.unwrap_or(ApiFile {
        files_id: Some(file_id.clone()),
        filename: String::new(),
        size: String::new(),
        soft_deleted: false,
    });
    let file_size = if file.size.trim().is_empty() {
        0
    } else {
        file.size
            .parse::<u64>()
            .map_err(|_| SourceError::InvalidResponse("ModCat file size is invalid".to_owned()))?
    };
    let review_status = if file.soft_deleted {
        ReviewStatus::Unavailable
    } else {
        version_review(&version.status)
    };
    Ok(SourceVersion {
        version_id,
        remote_file_id: file_id,
        display_version: version.version_number,
        filename: file.filename,
        file_size,
        md5: None,
        published_at: parse_timestamp(version.created_at.as_deref()),
        review_status,
    })
}

fn select_version(mod_ref: &ModRef, versions: Vec<ApiVersion>) -> Result<ApiVersion, SourceError> {
    if let Some(file_id) = &mod_ref.remote_file_id {
        return versions
            .into_iter()
            .find(|version| {
                version.files_id.as_deref() == Some(file_id)
                    || version
                        .file
                        .as_ref()
                        .and_then(|file| file.files_id.as_deref())
                        == Some(file_id)
            })
            .ok_or_else(|| SourceError::NotFound(file_id.clone()));
    }
    let version_id = mod_ref.version_id.as_deref().ok_or_else(|| {
        SourceError::InvalidArgument(
            "ModCat download resolution requires versionId or remoteFileId".to_owned(),
        )
    })?;
    versions
        .into_iter()
        .find(|version| version.version_id.as_deref() == Some(version_id))
        .ok_or_else(|| SourceError::NotFound(version_id.to_owned()))
}

fn map_dependency(
    game: &SourceGameBinding,
    dependency: ApiDependency,
) -> Result<SourceDependency, SourceError> {
    if let Some(raw_url) = dependency.modio_url.filter(|url| !url.trim().is_empty()) {
        let url = Url::parse(&raw_url).map_err(|_| {
            SourceError::InvalidResponse("ModCat dependency URL is invalid".to_owned())
        })?;
        let segments = url
            .path_segments()
            .map(Iterator::collect::<Vec<_>>)
            .unwrap_or_default();
        if !matches!(url.host_str(), Some("mod.io" | "www.mod.io"))
            || segments.len() != 4
            || segments[0] != "g"
            || segments[1] != game.remote_game_key
            || segments[2] != "m"
            || segments[3].is_empty()
        {
            return Err(SourceError::InvalidResponse(
                "ModCat mod.io dependency crossed the game binding".to_owned(),
            ));
        }
        return Ok(SourceDependency {
            mod_ref: ModRef {
                source_id: "modio".to_owned(),
                remote_game_key: game.remote_game_key.clone(),
                remote_mod_id: segments[3].to_owned(),
                version_id: None,
                remote_file_id: None,
            },
            required: true,
        });
    }
    let version = dependency.version.ok_or_else(|| {
        SourceError::InvalidResponse("ModCat dependency identity is missing".to_owned())
    })?;
    let remote_mod_id = required(version.mod_id.clone(), "dependency ModId")?;
    let mapped = map_version(version)?;
    Ok(SourceDependency {
        mod_ref: ModRef {
            source_id: SOURCE_ID.to_owned(),
            remote_game_key: game.remote_game_key.clone(),
            remote_mod_id,
            version_id: Some(mapped.version_id),
            remote_file_id: Some(mapped.remote_file_id),
        },
        required: true,
    })
}

fn base_mod_ref(game: &SourceGameBinding, remote_mod_id: String) -> ModRef {
    ModRef {
        source_id: SOURCE_ID.to_owned(),
        remote_game_key: game.remote_game_key.clone(),
        remote_mod_id,
        version_id: None,
        remote_file_id: None,
    }
}

fn required(value: Option<String>, field: &str) -> Result<String, SourceError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| SourceError::InvalidResponse(format!("ModCat {field} is missing")))
}

fn version_review(status: &str) -> ReviewStatus {
    match status.to_ascii_lowercase().as_str() {
        "approved" | "accepted" => ReviewStatus::Accepted,
        "pending" | "review" | "reviewing" => ReviewStatus::Pending,
        "rejected" | "denied" => ReviewStatus::Rejected,
        _ => ReviewStatus::Unavailable,
    }
}

fn parse_timestamp(value: Option<&str>) -> i64 {
    let Some(value) = value else {
        return 0;
    };
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.timestamp())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|date| date.and_utc().timestamp())
        })
        .unwrap_or_default()
}

fn deduplicate(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(value.to_ascii_lowercase()));
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
        .map_err(|_| SourceError::InvalidArgument("invalid ModCat cursor".to_owned()))?;
    let cursor: Cursor = serde_json::from_slice(&bytes)
        .map_err(|_| SourceError::InvalidArgument("invalid ModCat cursor".to_owned()))?;
    if cursor.scope_hash != expected_scope {
        return Err(SourceError::InvalidArgument(
            "ModCat cursor does not match the query".to_owned(),
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
            SourceError::NotFound("ModCat resource".to_owned())
        }
        BrokerError::Http { status: 429, .. } => SourceError::RateLimited,
        other => SourceError::Network(other),
    }
}

#[cfg(test)]
mod tests;
