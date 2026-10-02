use mintcat_feature_backup::{execute_codec, worker_workspace};
use mintcat_sdk::{run_worker_with_broker, worker_app_error, WorkerIdentity};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = worker_workspace()?;
    run_worker_with_broker(WorkerIdentity::from_env()?, move |request, _broker| {
        let workspace = workspace.clone();
        async move {
            let id = request.id.clone();
            tokio::task::spawn_blocking(move || execute_codec(&request,&workspace)).await
                .map_err(|e| worker_app_error(&id,"BACKUP_WORKER_FAILED","backup.worker.failed",json!({"reason":e.to_string()})))?
                .map_err(|e| worker_app_error(&id,"BACKUP_INVALID","backup.worker.invalid",json!({"reason":e.to_string()})))
        }
    }).await?;
    Ok(())
}
