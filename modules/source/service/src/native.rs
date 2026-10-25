use std::{collections::BTreeMap, sync::Arc, time::Duration};
use async_trait::async_trait;
use mintcat_game_api::GameDescriptor;
use mintcat_runtime_artifacts::ArtifactStore;
use mintcat_runtime_broker::{interaction_binding, BrowserDownloadRequest, CancellationToken, CredentialBinding, CredentialPlacement, InteractionClient, InteractionContext, Origin};
use mintcat_runtime_downloads::DownloadCatalog;
use mintcat_runtime_supervisor::{Invocation, ModuleSupervisor, SupervisorError};
use mintcat_sdk::{ActionRequired, ModRef, ScopedProviderBinding};
use mintcat_source_api::{DownloadDescriptor, DownloadResolution, NativeDownload, NativeDownloadResolution, SearchPage, SearchRequest, SourceDependency, SourceError, SourceGameBinding, SourceMod, SourceProvider, SourceVersion, SourceWorkerConfig};
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

/// Host-approved immutable active source package, its module lock, and signed
/// game descriptor. The host obtains the descriptor by invoking the active game
/// contract; it is not a public request parameter.
pub struct SourcePackageBinding {
    pub provider: ScopedProviderBinding,
    pub source_id: String,
    pub module_lock_digest: String,
    pub game_descriptor: GameDescriptor,
}
#[derive(Clone)]
pub struct NativeSourceProvider {
    supervisor: ModuleSupervisor,
    scope_id: String,
    contract_id: String,
    source_id: &'static str,
    config: SourceWorkerConfig,
    interactions: InteractionClient,
}
impl NativeSourceProvider {
    pub async fn register(catalog: &DownloadCatalog, supervisor: ModuleSupervisor, artifacts: &ArtifactStore, package: SourcePackageBinding, config: serde_json::Value, interactions: InteractionClient) -> Result<(), SourceError> {
        let source_id = match package.source_id.as_str() { "modio"=>"modio","modcat"=>"modcat","nexusmods"=>"nexusmods",_=>return Err(SourceError::Unavailable("source has no native contract implementation".into())) };
        let contract = format!("source.{source_id}");
        if package.provider.contract_id != contract || package.provider.contract_version != 1 || package.provider.provider_module_id != format!("source-{source_id}") { return Err(SourceError::InvalidArgument("approved source package identity/contract differs".into())); }
        let lease = artifacts.acquire_module(&package.provider.provider_digest).map_err(|error|SourceError::Unavailable(error.to_string()))?;
        if lease.module().manifest.id != package.provider.provider_module_id { return Err(SourceError::Unauthorized); }
        let config: SourceWorkerConfig = serde_json::from_value(config).map_err(|error| SourceError::InvalidArgument(error.to_string()))?;
        package.game_descriptor.validate().map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        let signed = package.game_descriptor.source_bindings.iter().find(|binding|binding.source_id == source_id).ok_or_else(||SourceError::InvalidArgument("signed game has no exact source binding".into()))?;
        supervisor.activate(package.provider.clone()).await.map_err(|error|SourceError::Unavailable(error.to_string()))?;
        let existing = catalog.provider_module_lock(source_id).map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        if existing.as_deref().is_some_and(|lock| lock != package.module_lock_digest) { return Err(SourceError::InvalidArgument("source catalog belongs to another module lock; rebuild the generation".into())); }
        if existing.is_none() {
            let provider = Arc::new(Self { supervisor,scope_id:package.provider.scope_id,contract_id:contract,source_id,config,interactions });
            catalog.register_provider(provider,package.module_lock_digest).map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        }
        catalog.register_game(source_id,SourceGameBinding { game_id:package.game_descriptor.game_id.clone(),remote_game_key:signed.remote_game_key.clone(),provider_game_id:signed.provider_game_id.clone() }).map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        Ok(())
    }
    async fn call<T: DeserializeOwned>(&self, method: &str, game: &SourceGameBinding, input: serde_json::Value, cancellation: &CancellationToken) -> Result<T,SourceError> {
        let value = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(SourceError::Unavailable("source request cancelled".into())),
            value = self.supervisor.invoke(Invocation { scope_id:&self.scope_id,contract_id:&self.contract_id,contract_version:1,method,params:json!({"game":game,"input":input,"config":self.config}),grants:&[],deadline:Duration::from_secs(60) }) => value.map_err(map_supervisor_error)?,
        };
        serde_json::from_value(value).map_err(|error|SourceError::InvalidResponse(error.to_string()))
    }
}
#[async_trait]
impl SourceProvider for NativeSourceProvider {
    fn source_id(&self)-> &'static str { self.source_id }
    fn parse_url(&self,_game:&SourceGameBinding,_url:&Url)->Result<Option<ModRef>,SourceError> { Err(SourceError::Unavailable("native URL parsing requires the asynchronous source contract".into())) }
    async fn search(&self,game:&SourceGameBinding,request:&SearchRequest,cancellation:&CancellationToken)->Result<SearchPage,SourceError> { self.call("search",game,serde_json::to_value(request).map_err(|e|SourceError::InvalidArgument(e.to_string()))?,cancellation).await }
    async fn get_mod(&self,game:&SourceGameBinding,reference:&ModRef,cancellation:&CancellationToken)->Result<SourceMod,SourceError> { self.call("getMod",game,json!(reference),cancellation).await }
    async fn list_versions(&self,game:&SourceGameBinding,reference:&ModRef,cancellation:&CancellationToken)->Result<Vec<SourceVersion>,SourceError> { self.call("listVersions",game,json!(reference),cancellation).await }
    async fn get_dependencies(&self,game:&SourceGameBinding,reference:&ModRef,cancellation:&CancellationToken)->Result<Vec<SourceDependency>,SourceError> { self.call("getDependencies",game,json!(reference),cancellation).await }
    async fn resolve_download(&self,game:&SourceGameBinding,reference:&ModRef,cancellation:&CancellationToken)->Result<DownloadDescriptor,SourceError> {
        match self.call("resolveDownload",game,json!(reference),cancellation).await? {
            NativeDownloadResolution::Ready {download}=>verify_download(self.source_id,&self.config,reference,download),
            NativeDownloadResolution::InteractionRequired {kind,target_url}=>Err(SourceError::InteractionRequired {kind,target_url}),
        }
    }

    async fn begin_download(&self, job_id:&str, context:&InteractionContext, game:&SourceGameBinding, reference:&ModRef, cancellation:&CancellationToken)->Result<DownloadResolution,SourceError> {
        match self.resolve_download(game,reference,cancellation).await {
            Ok(descriptor)=>Ok(DownloadResolution::Ready(descriptor)),
            Err(SourceError::InteractionRequired {kind,target_url}) if kind=="browser-download" && self.source_id=="nexusmods"=>{
                let navigation=self.config.navigation_origins()?;
                if !navigation.contains(&Origin::from_url(&target_url)?) { return Err(SourceError::InvalidResponse("native source browser target lies outside approved navigation origins".into())); }
                let action=self.interactions.request_browser_download(job_id,context,BrowserDownloadRequest {download_page_url:target_url,allowed_navigation_origins:navigation,callback_origins:self.config.download_origins()?,callback_schemes:vec!["nxm".into()]})?;
                Ok(DownloadResolution::InteractionRequired(action))
            },
            Err(error)=>Err(error),
        }
    }

    async fn resume_download_action(&self, action:&ActionRequired, game:&SourceGameBinding, reference:&ModRef, cancellation:&CancellationToken)->Result<DownloadDescriptor,SourceError> {
        if self.source_id!="nexusmods" || action.kind!="browser-download" {return Err(SourceError::InvalidArgument("source does not own this browser download action".into()));}
        let callback=self.interactions.consume_browser_callback(&action.action_id,&interaction_binding(action))?;
        let input=json!({"action":action,"reference":reference,"callback":callback});
        match self.call("resumeDownloadCallback",game,input,cancellation).await? {
            NativeDownloadResolution::Ready {download}=>verify_download(self.source_id,&self.config,reference,download),
            NativeDownloadResolution::InteractionRequired {kind,target_url}=>Err(SourceError::InteractionRequired {kind,target_url}),
        }
    }
}

fn verify_download(source_id:&str, config:&SourceWorkerConfig, reference:&ModRef, result:NativeDownload)->Result<DownloadDescriptor,SourceError> {
    let url = Url::parse(&result.url).map_err(|error|SourceError::InvalidResponse(error.to_string()))?;
    let origin = Origin::from_url(&url)?;
    let mut allowed = config.download_origins()?;
    let identity_matches = reference.source_id == source_id
        && result.reference.source_id == reference.source_id
        && result.reference.remote_game_key == reference.remote_game_key
        && result.reference.remote_mod_id == reference.remote_mod_id
        && reference.version_id.as_ref().is_none_or(|version|result.reference.version_id.as_ref()==Some(version))
        && reference.remote_file_id.as_ref().is_none_or(|file|result.reference.remote_file_id.as_ref()==Some(file));
    if !allowed.contains(&origin) || !identity_matches || !url.username().is_empty() || url.password().is_some() {
        return Err(SourceError::InvalidResponse("native download lies outside configured authority or requested identity".into()));
    }
    let mut headers = BTreeMap::new();
    let mut credential = None;
    if source_id == "modcat" {
        let file = result.reference.remote_file_id.as_deref().filter(|file|!file.is_empty())
            .ok_or_else(||SourceError::InvalidResponse("native ModCat download has no file identity".into()))?;
        if result.reference.version_id.as_deref().is_none_or(str::is_empty) {
            return Err(SourceError::InvalidResponse("native ModCat download has no version identity".into()));
        }
        let api_origin = Origin::from_url(&config.api_base)?;
        let mut approved_url = config.api_base.join("api/Files/DownloadFileGet")
            .map_err(|error|SourceError::InvalidResponse(error.to_string()))?;
        approved_url.query_pairs_mut().append_pair("FileId",file).append_pair("NoCount","true");
        if origin != api_origin || url != approved_url {
            return Err(SourceError::InvalidResponse("native ModCat download differs from the approved API file endpoint".into()));
        }
        // Match the ModCat provider's endpoint and bindings, but derive every
        // binding here from immutable host config, never native response data.
        allowed = vec![api_origin.clone()];
        if let Some(request_origin) = &config.download_request_origin {
            Origin::from_url(request_origin)?;
            headers.insert("origin".into(),request_origin.origin().ascii_serialization());
            headers.insert("referer".into(),request_origin.as_str().into());
        }
        credential = config.credential_ref.as_ref().map(|reference|CredentialBinding {
            reference:reference.clone(),placement:CredentialPlacement::Bearer,allowed_origins:vec![api_origin],
        });
    }
    // Mod.io and Nexus binary downloads never carry their API credentials.
    Ok(DownloadDescriptor { mod_ref:result.reference,url,allowed_origins:allowed,headers,credential,expires_at:result.expires_at,expected_size:result.expected_size,expected_md5:result.expected_md5 })
}

fn map_supervisor_error(error: SupervisorError) -> SourceError {
    match error {
        SupervisorError::WorkerApplication { code, details, .. } => {
            // Worker strings and URLs are not trusted diagnostics. Retain the
            // source protocol's safe reason vocabulary, including older workers
            // which supplied only a code/messageKey.
            let reason = details.get("reason").and_then(serde_json::Value::as_str)
                .filter(|reason| matches!(*reason,
                    "credential_missing_or_invalid" | "source_configuration" |
                    "source_not_found" | "source_rate_limited" | "source_response" |
                    "source_interaction_required" | "source_unavailable" | "source_network"))
                .map(str::to_owned);
            match code.as_str() {
                "SOURCE_UNAUTHORIZED" => SourceError::Unauthorized,
                "SOURCE_NOT_FOUND" => SourceError::NotFound(reason.unwrap_or_else(||"source_not_found".into())),
                "SOURCE_RATE_LIMITED" => SourceError::RateLimited,
                "INVALID_ARGUMENT" => SourceError::InvalidArgument(reason.unwrap_or_else(||"source_configuration".into())),
                "SOURCE_INVALID_RESPONSE" => SourceError::InvalidResponse(reason.unwrap_or_else(||"source_response".into())),
                _ => SourceError::Unavailable(reason.unwrap_or_else(||"source_unavailable".into())),
            }
        },
        other => SourceError::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mintcat_runtime_broker::{CredentialVault, MemoryCredentialStore, NetworkBroker, NetworkRequest, SecretValue};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

    #[test]
    fn native_worker_errors_preserve_categories_and_safe_reasons() {
        for (code,reason) in [
            ("INVALID_ARGUMENT","source_configuration"),
            ("SOURCE_UNAUTHORIZED","credential_missing_or_invalid"),
            ("SOURCE_NOT_FOUND","source_not_found"),
            ("SOURCE_RATE_LIMITED","source_rate_limited"),
            ("SOURCE_INVALID_RESPONSE","source_response"),
            ("SOURCE_UNAVAILABLE","source_network"),
        ] {
            let error = map_supervisor_error(SupervisorError::WorkerApplication {
                code:code.into(),message_key:"source.worker.failed".into(),
                details:json!({"reason":reason,"message":"https://example.invalid/?token=private-value"}),
            });
            assert_eq!(error.app_code(),code);
            assert_eq!(error.diagnostic_reason(),reason);
            assert!(!error.to_string().contains("private-value"));
        }
        let legacy = map_supervisor_error(SupervisorError::WorkerApplication {
            code:"INVALID_ARGUMENT".into(),message_key:"source.worker.failed".into(),
            details:json!({"message":"legacy worker diagnostic"}),
        });
        assert!(matches!(legacy,SourceError::InvalidArgument(_)));
        let untrusted = map_supervisor_error(SupervisorError::WorkerApplication {
            code:"INVALID_ARGUMENT".into(),message_key:"source.worker.failed".into(),
            details:json!({"reason":"https://example.invalid/?token=private-value"}),
        });
        assert_eq!(untrusted.diagnostic_reason(),"source_configuration");
        assert!(!untrusted.to_string().contains("private-value"));
    }

    #[test]
    fn host_rejects_every_binary_download_with_empty_origin_scope() {
        for source in ["modio","nexusmods"] {
            let mut config = config(Url::parse("https://api.example/v1/").unwrap());
            config.download_origins.clear();
            let requested = reference(source);
            let result = download(requested.clone(),Url::parse("https://downloads.example/file.zip").unwrap());
            assert!(matches!(verify_download(source,&config,&requested,result),Err(SourceError::InvalidResponse(_))));
        }
    }

    fn reference(source:&str)->ModRef {
        ModRef {source_id:source.into(),remote_game_key:"game".into(),remote_mod_id:"mod-1".into(),version_id:Some("version-1".into()),remote_file_id:Some("file-1".into())}
    }

    fn config(api_base:Url)->SourceWorkerConfig {
        SourceWorkerConfig {
            download_origins:vec![api_base.origin().ascii_serialization()],api_base,
            website_base:None,credential_ref:None,navigation_origins:vec![],
            download_request_origin:None,application_name:None,application_version:None,
        }
    }

    fn download(reference:ModRef, url:Url)->NativeDownload {
        NativeDownload {reference,url:url.to_string(),expected_size:7,expected_md5:None,expires_at:None}
    }

    fn modcat_url(config:&SourceWorkerConfig)->Url {
        let mut url = config.api_base.join("api/Files/DownloadFileGet").unwrap();
        url.query_pairs_mut().append_pair("FileId","file-1").append_pair("NoCount","true");
        url
    }

    #[test]
    fn modcat_authority_requires_exact_requested_identity_and_file_endpoint() {
        let mut config = config(Url::parse("https://api.example/vendor/").unwrap());
        config.credential_ref = Some(serde_json::from_value(json!("credential:host-approved")).unwrap());
        config.download_request_origin = Some(Url::parse("http://tauri.localhost/").unwrap());
        config.download_origins.push("https://cdn.example".into());
        let requested = reference("modcat");
        let url = modcat_url(&config);
        let mismatches = [
            ModRef {source_id:"nexusmods".into(),..requested.clone()},
            ModRef {remote_game_key:"another-game".into(),..requested.clone()},
            ModRef {remote_mod_id:"another-mod".into(),..requested.clone()},
            ModRef {version_id:Some("another-version".into()),..requested.clone()},
            ModRef {remote_file_id:Some("another-file".into()),..requested.clone()},
            ModRef {version_id:None,..requested.clone()},
            ModRef {remote_file_id:None,..requested.clone()},
        ];
        for returned in mismatches {
            assert!(matches!(verify_download("modcat",&config,&requested,download(returned,url.clone())),Err(SourceError::InvalidResponse(_))));
        }
        for unapproved in [
            "https://outside.example/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=true",
            "https://cdn.example/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=true",
            "https://api.example/api/Files/DownloadFileGet?FileId=file-1&NoCount=true",
            "https://api.example/vendor/api/Files/DownloadFileGet?FileId=another-file&NoCount=true",
            "https://api.example/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=false",
            "https://api.example/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=true&FileId=another-file",
            "https://api.example/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=true#unapproved",
            "https://attacker@api.example/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=true",
        ] {
            assert!(matches!(verify_download("modcat",&config,&requested,download(requested.clone(),Url::parse(unapproved).unwrap())),Err(SourceError::InvalidResponse(_))),"accepted {unapproved}");
        }
        let other_source = reference("modio");
        assert!(matches!(verify_download("modcat",&config,&other_source,download(other_source.clone(),url.clone())),Err(SourceError::InvalidResponse(_))));
        let unresolved = ModRef {version_id:None,remote_file_id:None,..requested.clone()};
        assert_eq!(verify_download("modcat",&config,&unresolved,download(requested.clone(),url)).unwrap().mod_ref,requested);
    }

    async fn request_download(descriptor:DownloadDescriptor, broker:&NetworkBroker)->Vec<u8> {
        let mut request = NetworkRequest::get("source.download",descriptor.url,descriptor.allowed_origins);
        request.headers = descriptor.headers;
        request.credential = descriptor.credential;
        request.timeout = Duration::from_secs(3);
        broker.execute(&request,&CancellationToken::default()).await.unwrap().body
    }

    async fn server(
        listener:TcpListener,
        expected_path:String,
        expected_origin:Option<&'static str>,
        expected_referer:Option<&'static str>,
        expected_authorization:Option<&'static str>,
    ) {
        let (mut socket,_) = tokio::time::timeout(Duration::from_secs(3),listener.accept()).await.unwrap().unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8;1024];
        loop {
            let count = tokio::time::timeout(Duration::from_secs(3),socket.read(&mut buffer)).await.unwrap().unwrap();
            assert!(count>0);
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len()<=8192);
            if bytes.windows(4).any(|window|window==b"\r\n\r\n") { break; }
        }
        let request = String::from_utf8(bytes).unwrap();
        let header = |wanted:&str|request.lines().filter_map(|line|line.split_once(':'))
            .find_map(|(name,value)|name.eq_ignore_ascii_case(wanted).then(||value.trim()));
        let approved = request.lines().next()==Some(format!("GET {expected_path} HTTP/1.1").as_str())
            && header("origin")==expected_origin && header("referer")==expected_referer
            && header("authorization")==expected_authorization && header("apikey").is_none();
        let response = if approved {
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\narchive"
        } else {
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        };
        socket.write_all(response.as_bytes()).await.unwrap();
        assert!(approved,"unexpected download authority: {request}");
    }

    #[tokio::test]
    async fn modcat_download_http_uses_only_configured_bindings() {
        for (use_origin,use_credential) in [(true,true),(false,false),(true,false),(false,true)] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut config = config(Url::parse(&format!("http://{}/vendor/",listener.local_addr().unwrap())).unwrap());
            let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
            if use_origin { config.download_request_origin = Some(Url::parse("http://tauri.localhost/").unwrap()); }
            if use_credential { config.credential_ref = Some(vault.store(SecretValue::new("host-secret").unwrap()).unwrap()); }
            let url = modcat_url(&config);
            let requested = reference("modcat");
            let descriptor = verify_download("modcat",&config,&requested,download(requested.clone(),url)).unwrap();
            let server = tokio::spawn(server(listener,"/vendor/api/Files/DownloadFileGet?FileId=file-1&NoCount=true".into(),
                use_origin.then_some("http://tauri.localhost"),use_origin.then_some("http://tauri.localhost/"),
                use_credential.then_some("Bearer host-secret")));
            assert_eq!(request_download(descriptor,&NetworkBroker::new(&vault,None).unwrap()).await,b"archive");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn modio_and_nexus_download_http_never_attach_api_authority() {
        for source in ["modio","nexusmods"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let binary_url = Url::parse(&format!("http://{}/archive",listener.local_addr().unwrap())).unwrap();
            let mut config = config(Url::parse("https://api.example/v1/").unwrap());
            config.download_origins = vec![binary_url.origin().ascii_serialization()];
            let vault = CredentialVault::new(Arc::new(MemoryCredentialStore::default()));
            config.credential_ref = Some(vault.store(SecretValue::new("api-only-secret").unwrap()).unwrap());
            config.download_request_origin = Some(Url::parse("http://tauri.localhost/").unwrap());
            let requested = reference(source);
            let descriptor = verify_download(source,&config,&requested,download(requested.clone(),binary_url)).unwrap();
            let server = tokio::spawn(server(listener,"/archive".into(),None,None,None));
            assert_eq!(request_download(descriptor,&NetworkBroker::new(&vault,None).unwrap()).await,b"archive");
            server.await.unwrap();
        }
    }
}
