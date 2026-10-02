use std::{collections::BTreeMap, path::Path};
use mintcat_runtime_broker::{CancellationToken, CredentialBinding, CredentialPlacement, CredentialRef, HttpMethod, NetworkBroker, NetworkRequest, NetworkResponse, Origin};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;
use crate::{atomic_write, digest, BackupError, BackupService, ModuleVersion};

/// Returns a daemon-owned vault reference. Only the existing NetworkBroker resolves tokens.
pub trait CloudCredentialProvider: Send + Sync {
    fn mintcat_credential_ref(&self) -> Result<CredentialRef, BackupError>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudMetadata {
    pub created_at: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudBackupRecord {
    pub id: String,
    #[serde(flatten)] pub metadata: CloudMetadata,
}
pub struct CloudBackupTransport { origin: Url, network: NetworkBroker, allowed_origins: Vec<Origin> }
impl CloudBackupTransport {
    pub fn new(origin: &str, network: NetworkBroker, download_origins: Vec<Origin>) -> Result<Self, BackupError> {
        let origin = Url::parse(origin).map_err(|_| BackupError::Cloud("invalid cloud origin".into()))?;
        validate_url(&origin)?;
        if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() { return Err(BackupError::Cloud("cloud endpoint must be an origin".into())); }
        let mut allowed_origins = download_origins;
        allowed_origins.push(Origin::from_url(&origin).map_err(|_| BackupError::Cloud("invalid cloud origin".into()))?);
        Ok(Self { origin, network, allowed_origins })
    }
    fn endpoint(&self, id: Option<&str>, download: bool) -> Result<Url, BackupError> {
        let mut url = self.origin.clone();
        let mut path = url.path_segments_mut().map_err(|_| BackupError::Cloud("invalid endpoint".into()))?;
        path.clear().push("v1").push("backups");
        if let Some(id) = id { if id.is_empty() { return Err(BackupError::Invalid("backup id empty".into())); } path.push(id); }
        if download { path.push("download"); }
        drop(path);
        Ok(url)
    }
    pub async fn list(&self, credentials: &dyn CloudCredentialProvider) -> Result<Vec<CloudBackupRecord>, BackupError> {
        let response = self.request(HttpMethod::Get, self.endpoint(None, false)?, BTreeMap::new(), None, Some(credentials)).await?;
        let bytes = checked_bytes(response)?;
        #[derive(Deserialize)] #[serde(untagged)] enum List { Array(Vec<CloudBackupRecord>), Items { items: Vec<CloudBackupRecord> } }
        Ok(match serde_json::from_slice::<List>(&bytes)? { List::Array(items) | List::Items { items } => items })
    }
    pub async fn upload(&self, bytes: Vec<u8>, mut metadata: CloudMetadata, credentials: &dyn CloudCredentialProvider) -> Result<CloudBackupRecord, BackupError> {
        let archive: crate::BackupArchive = serde_json::from_slice(&bytes)?;
        BackupService::validate_bytes(&bytes, &archive.manifest.module_versions)?;
        metadata.size = bytes.len() as u64;
        metadata.checksum = Some(digest(&bytes)[7..].to_owned());
        let boundary = format!("mintcat-{}", Uuid::new_v4());
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"mintcat-backup.json\"\r\nContent-Type: application/octet-stream\r\n\r\n", serde_json::to_string(&metadata)?).into_bytes();
        body.extend_from_slice(&bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let response = self.request(HttpMethod::Post, self.endpoint(None, false)?, BTreeMap::from([("Content-Type".into(), format!("multipart/form-data; boundary={boundary}"))]), Some(body), Some(credentials)).await?;
        let bytes = checked_bytes(response)?;
        #[derive(Deserialize)] #[serde(untagged)] enum Upload { Backup { backup: CloudBackupRecord }, Data { data: CloudBackupRecord }, Record(CloudBackupRecord) }
        Ok(match serde_json::from_slice::<Upload>(&bytes)? { Upload::Backup { backup } => backup, Upload::Data { data } => data, Upload::Record(record) => record })
    }
    pub async fn download(&self, id: &str, credentials: &dyn CloudCredentialProvider) -> Result<Vec<u8>, BackupError> {
        let response = self.request(HttpMethod::Get, self.endpoint(Some(id), true)?, BTreeMap::from([("Accept".into(), "application/octet-stream".into())]), None, Some(credentials)).await?;
        let is_json = response.headers.get("content-type").is_some_and(|value| value.contains("application/json"));
        let bytes = checked_bytes(response)?;
        // JSON may be a backup archive or the existing server's signed-download URL envelope.
        if is_json {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            if let Some(url) = value.get("downloadUrl").and_then(|value| value.as_str()) {
                let url = Url::parse(url).map_err(|_| BackupError::Cloud("invalid download URL".into()))?;
                validate_url(&url)?;
                // Never forward a Mintcat bearer token to a storage/signature URL.
                return checked_bytes(self.request(HttpMethod::Get, url, BTreeMap::new(), None, None).await?);
            }
        }
        Ok(bytes)
    }
    pub async fn download_to(&self, id: &str, target: &Path, modules: &BTreeMap<String, ModuleVersion>, credentials: &dyn CloudCredentialProvider) -> Result<crate::BackupResult, BackupError> {
        let bytes = self.download(id, credentials).await?;
        let result = BackupService::validate_bytes(&bytes, modules)?;
        atomic_write(target, &bytes, false)?;
        Ok(result)
    }
    pub async fn delete(&self, id: &str, credentials: &dyn CloudCredentialProvider) -> Result<(), BackupError> {
        let response = self.request(HttpMethod::Delete, self.endpoint(Some(id), false)?, BTreeMap::new(), None, Some(credentials)).await?;
        checked_bytes(response)?;
        Ok(())
    }
    async fn request(&self, method: HttpMethod, url: Url, headers: BTreeMap<String, String>, body: Option<Vec<u8>>, credentials: Option<&dyn CloudCredentialProvider>) -> Result<NetworkResponse, BackupError> {
        validate_url(&url)?;
        let mut request = NetworkRequest::get("mintcat.feature.backup.cloud", url, self.allowed_origins.clone());
        request.method = method;
        request.headers = headers;
        if let Some(body) = body { request = request.with_body(body); }
        if let Some(credentials) = credentials {
            let reference = credentials.mintcat_credential_ref()?;
            if reference.as_str().strip_prefix("credential:").and_then(|id| Uuid::parse_str(id).ok()).is_none() {
                return Err(BackupError::Cloud("Mintcat OAuth vault reference unavailable; sign in before cloud backup".into()));
            }
            request.credential = Some(CredentialBinding {
                reference, placement: CredentialPlacement::Bearer,
                allowed_origins: vec![Origin::from_url(&self.origin).map_err(|_| BackupError::Cloud("invalid credential origin".into()))?],
            });
        }
        self.network.execute(&request, &CancellationToken::default()).await.map_err(|error| BackupError::Cloud(error.to_string()))
    }
}
fn validate_url(url: &Url) -> Result<(), BackupError> {
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() { return Err(BackupError::Cloud("cloud transport requires HTTPS without embedded credentials".into())); }
    Ok(())
}
fn checked_bytes(response: NetworkResponse) -> Result<Vec<u8>, BackupError> {
    if !(200..300).contains(&response.status) { return Err(BackupError::Cloud(format!("HTTP {}", response.status))); }
    Ok(response.body)
}
