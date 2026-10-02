use mintcat_sdk::{run_worker_with_broker, WorkerIdentity};
use mintcat_runtime_broker::ProxyPolicy;
use mintcat_source_api::{execute_source_worker, source_worker_error, SourceError, SourceWorkerParams};
use mintcat_source_modio::{ModioConfig, ModioProvider};

#[tokio::main]
async fn main()->Result<(),Box<dyn std::error::Error>> {
    run_worker_with_broker(WorkerIdentity::from_env()?, |mut request,broker| async move {
        let params:SourceWorkerParams=serde_json::from_value(std::mem::take(&mut request.params)).map_err(|error|source_worker_error(&request,SourceError::InvalidArgument(error.to_string())))?;
        let download_origins=params.config.download_origins().map_err(|error|source_worker_error(&request,error))?;
        let config=ModioConfig {api_base:params.config.api_base,credential:params.config.credential_ref,proxy_policy:ProxyPolicy::Direct,download_origins};
        let provider=ModioProvider::new(broker,config).map_err(|error|source_worker_error(&request,error))?;
        execute_source_worker(&request,&params.game,&params.input,&provider).await
    }).await?;
    Ok(())
}
