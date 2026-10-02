use mintcat_runtime_broker::{CancellationToken, CredentialRef, Origin};
use mintcat_sdk::{worker_app_error, ModRef, ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState, PermissionRequest, ProvidedContract, RequiredContract, WorkerRequest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;
use crate::{SourceError, SourceGameBinding, SourceProvider};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SourceWorkerConfig {
    pub api_base: Url,
    #[serde(default)] pub website_base: Option<Url>,
    #[serde(default)] pub credential_ref: Option<CredentialRef>,
    pub download_origins: Vec<String>,
    #[serde(default)] pub navigation_origins: Vec<String>,
    #[serde(default)] pub download_request_origin: Option<Url>,
    #[serde(default)] pub application_name: Option<String>,
    #[serde(default)] pub application_version: Option<String>,
}
impl SourceWorkerConfig {
    pub fn download_origins(&self)->Result<Vec<Origin>,SourceError> { self.download_origins.iter().map(|origin|Origin::parse(origin).map_err(Into::into)).collect() }
    pub fn navigation_origins(&self)->Result<Vec<Origin>,SourceError> { self.navigation_origins.iter().map(|origin|Origin::parse(origin).map_err(Into::into)).collect() }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SourceWorkerParams { pub game: SourceGameBinding, pub input: Value, pub config: SourceWorkerConfig }
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct NativeDownload {
    pub reference: ModRef,
    pub url: String,
    pub expected_size: u64,
    pub expected_md5: Option<String>,
    pub expires_at: Option<i64>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag="state",rename_all="kebab-case",rename_all_fields="camelCase",deny_unknown_fields)]
pub enum NativeDownloadResolution {
    Ready { download: NativeDownload },
    InteractionRequired { kind: String, target_url: Url },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SourceBrowserCallback { pub action: mintcat_sdk::ActionRequired, pub reference: ModRef, pub callback: Url }
pub async fn execute_source_worker(request:&WorkerRequest, game:&SourceGameBinding, input:&Value, provider:&dyn SourceProvider)->Result<Value,Box<mintcat_sdk::AppError>> {
    let result=execute(request,game,input,provider).await;
    result.map_err(|error|source_worker_error(request,error))
}
async fn execute(request:&WorkerRequest,game:&SourceGameBinding,input:&Value,provider:&dyn SourceProvider)->Result<Value,SourceError> {
    if request.contract_id.as_deref().and_then(|id|id.strip_prefix("source."))!=Some(provider.source_id()) || request.contract_version!=Some(1) || !request.grants.is_empty() { return Err(SourceError::Unauthorized); }
    let cancellation=CancellationToken::default();
    let value=match request.method.as_deref() {
        Some("search")=>serde_json::to_value(provider.search(game,&decode(input)?,&cancellation).await?),
        Some("getMod")=>serde_json::to_value(provider.get_mod(game,&reference(input)?,&cancellation).await?),
        Some("listVersions")=>serde_json::to_value(provider.list_versions(game,&reference(input)?,&cancellation).await?),
        Some("getDependencies")=>serde_json::to_value(provider.get_dependencies(game,&reference(input)?,&cancellation).await?),
        Some("resolveDownload")=> {
            let resolution = match provider.resolve_download(game,&reference(input)?,&cancellation).await {
                Ok(descriptor)=>NativeDownloadResolution::Ready { download:wire_download(descriptor) },
                Err(SourceError::InteractionRequired {kind,target_url})=>NativeDownloadResolution::InteractionRequired {kind,target_url},
                Err(error)=>return Err(error),
            };
            serde_json::to_value(resolution)
        },
        Some("resumeDownloadCallback")=> {
            let input:SourceBrowserCallback=decode(input)?;
            let resolution = match provider.resume_download_callback(&input.action,game,&input.reference,input.callback,&cancellation).await {
                Ok(descriptor)=>NativeDownloadResolution::Ready {download:wire_download(descriptor)},
                Err(SourceError::InteractionRequired {kind,target_url})=>NativeDownloadResolution::InteractionRequired {kind,target_url},
                Err(error)=>return Err(error),
            };
            serde_json::to_value(resolution)
        },
        _=>return Err(SourceError::Unavailable("unsupported source native method".into())),
    };
    value.map_err(|error|SourceError::InvalidResponse(error.to_string()))
}
fn wire_download(descriptor:crate::DownloadDescriptor)->NativeDownload {
    // Bindings are never worker authority. Only download metadata crosses the
    // native boundary; the host reconstructs bindings from its approved config.
    NativeDownload {reference:descriptor.mod_ref,url:descriptor.url.to_string(),expected_size:descriptor.expected_size,expected_md5:descriptor.expected_md5,expires_at:descriptor.expires_at}
}
fn decode<T:serde::de::DeserializeOwned>(input:&Value)->Result<T,SourceError> { T::deserialize(input).map_err(|error|SourceError::InvalidArgument(error.to_string())) }
fn reference(input:&Value)->Result<ModRef,SourceError> { decode(input) }
pub fn source_worker_error(request:&WorkerRequest,error:SourceError)->Box<mintcat_sdk::AppError> {
    let code=match error {SourceError::Unauthorized=>"SOURCE_UNAUTHORIZED",SourceError::NotFound(_)=>"SOURCE_NOT_FOUND",SourceError::RateLimited=>"SOURCE_RATE_LIMITED",SourceError::InvalidArgument(_)=>"INVALID_ARGUMENT",SourceError::InvalidResponse(_)=>"SOURCE_INVALID_RESPONSE",SourceError::InteractionRequired {..}=>"SOURCE_INTERACTION_REQUIRED",_=>"SOURCE_UNAVAILABLE"};
    worker_app_error(&request.id,code,"source.worker.failed",json!({"message":error.to_string()}))
}
pub fn source_module_manifest(source_id:&str,target:&str,host_api:&str,worker:ModuleFile)->ModuleManifest {
    ModuleManifest {
        schema_version: 1,
        id: format!("source-{source_id}"),
        version: "1.0.0".into(),
        publisher: "mintcat.official".into(),
        target: target.into(),
        host_api: host_api.into(),
        components: vec![ModuleComponent {kind:"service".into(),entry:worker.path.clone(),runtime:"worker".into(),state_model:"stateless".into()}],
        provides: vec![ProvidedContract {contract_id:format!("source.{source_id}"),version:1,cardinality:Some("multi".into())}],
        requires: vec![RequiredContract {contract_id:"source.network".into(),version_range:"=1".into(),optional:false,binding_scope:Some("host".into())}],
        permissions: vec![PermissionRequest {capability:"network".into(),scope:format!("source.{source_id}")}],
        contributes: json!({}),
        state: ModuleState {schema_version:1,readable_range:"1".into(),writable_range:"1".into(),rollback_before_write_only:Some(true)},
        activation: ModuleActivation {trigger:"on-demand".into(),drain_policy:"finish-requests".into()},
        files: vec![worker],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mintcat_runtime_broker::{CredentialBinding, CredentialPlacement};
    use std::collections::BTreeMap;

    #[test]
    fn native_download_cannot_transport_worker_selected_authority() {
        let url = Url::parse("https://api.example/api/Files/DownloadFileGet?FileId=file-1&NoCount=true").unwrap();
        let reference = ModRef {
            source_id:"modcat".into(),remote_game_key:"game".into(),remote_mod_id:"mod-1".into(),
            version_id:Some("version-1".into()),remote_file_id:Some("file-1".into()),
        };
        let descriptor = crate::DownloadDescriptor {
            mod_ref:reference,url:url.clone(),allowed_origins:vec![Origin::from_url(&url).unwrap()],
            headers:BTreeMap::from([("origin".into(),"http://tauri.localhost".into()),("referer".into(),"http://tauri.localhost/".into()),("authorization".into(),"Bearer worker-selected".into())]),
            credential:Some(CredentialBinding {
                reference:serde_json::from_value(json!("credential:worker-selected")).unwrap(),
                placement:CredentialPlacement::Bearer,allowed_origins:vec![Origin::from_url(&url).unwrap()],
            }),
            expected_size:42,expected_md5:None,expires_at:None,
        };
        let value = serde_json::to_value(wire_download(descriptor)).unwrap();
        serde_json::from_value::<NativeDownload>(value.clone()).unwrap();
        assert!(value.get("headers").is_none());
        assert!(value.get("credential").is_none());
        for (field,binding) in [
            ("headers",json!({"authorization":"Bearer worker-selected"})),
            ("credential",json!("credential:worker-selected")),
            ("allowedOrigins",json!(["https://attacker.example"])),
        ] {
            let mut injected = value.clone();
            injected[field] = binding;
            assert!(serde_json::from_value::<NativeDownload>(injected).is_err(),"accepted worker authority field {field}");
        }
    }
}
