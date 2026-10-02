use mintcat_sdk::{run_worker_with_broker, WorkerIdentity};
use mintcat_runtime_broker::ProxyPolicy;
use mintcat_source_api::{execute_source_worker, source_worker_error, SourceError, SourceWorkerParams};
use mintcat_source_nexusmods::{NexusConfig, NexusProvider};

#[tokio::main]
async fn main()->Result<(),Box<dyn std::error::Error>> {
    run_worker_with_broker(WorkerIdentity::from_env()?, |mut request,broker| async move {
        let params:SourceWorkerParams=serde_json::from_value(std::mem::take(&mut request.params)).map_err(|error|source_worker_error(&request,SourceError::InvalidArgument(error.to_string())))?;
        let missing=|name:&str|source_worker_error(&request,SourceError::InvalidArgument(format!("Nexus config requires {name}")));
        let navigation_origins=params.config.navigation_origins().map_err(|error|source_worker_error(&request,error))?;
        let download_origins=params.config.download_origins().map_err(|error|source_worker_error(&request,error))?;
        let config=NexusConfig {
            api_base:params.config.api_base,
            website_base:params.config.website_base.ok_or_else(||missing("websiteBase"))?,
            credential:params.config.credential_ref,
            proxy_policy:ProxyPolicy::Direct,
            navigation_origins,
            download_origins,
            application_name:params.config.application_name.ok_or_else(||missing("applicationName"))?,
            application_version:params.config.application_version.ok_or_else(||missing("applicationVersion"))?,
        };
        let provider=NexusProvider::with_network_client(broker.into(),config).map_err(|error|source_worker_error(&request,error))?;
        execute_source_worker(&request,&params.game,&params.input,&provider).await
    }).await?;
    Ok(())
}
