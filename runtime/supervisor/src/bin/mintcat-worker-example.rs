use anyhow::Result;
use mintcat_sdk::{
    run_worker_with_broker, worker_app_error, ArtifactGrantAccess, WorkerIdentity, WorkerRequest,
    WorkerBroker, WorkerBrokerRequest, ArtifactGrantDescriptor, WORKER_WORKSPACE_ENV,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<()> {
    let identity = WorkerIdentity::from_env()?;
    eprintln!(
        "worker {} starting module {}@{}",
        identity.instance_id, identity.module_id, identity.module_version
    );
    run_worker_with_broker(identity, handle).await?;
    Ok(())
}

async fn handle(request: WorkerRequest, broker: WorkerBroker) -> Result<Value, Box<mintcat_sdk::AppError>> {
    let method = request.method.as_deref().unwrap_or_default();
    match method {
        "fixture.callback" => {
            let callback: WorkerBrokerRequest = serde_json::from_value(request.params.clone())
                .map_err(|error| worker_app_error(&request.id, "INVALID_ARGUMENT", "fixture.callback.invalid", json!({"message": error.to_string()})))?;
            broker.invoke(callback).await.map_err(|error|
                worker_app_error(&request.id, "BROKER_REJECTED", "fixture.callback.rejected", json!({"message": error.to_string()})))
        }
        "build" => {
            // Controlled routing fixture, not a game delivery implementation.
            let grant_id = request.params["grantId"].as_str().unwrap_or_default();
            let grant = request.grants.iter().find(|grant| grant.grant_id == grant_id)
                .ok_or_else(|| worker_app_error(&request.id, "PERMISSION_DENIED", "fixture.grant.required", json!({})))?;
            let mut bytes = tokio::fs::read(&grant.path).await
                .map_err(|error| worker_app_error(&request.id, "IO", "fixture.read.failed", json!({"message": error.to_string()})))?;
            let version = std::env::var("MINTCAT_WORKER_MODULE_VERSION").unwrap();
            bytes.extend_from_slice(format!("-engine-{version}").as_bytes());
            let path = std::path::PathBuf::from(std::env::var(WORKER_WORKSPACE_ENV).unwrap()).join(format!("{}.out", request.id));
            tokio::fs::write(&path, &bytes).await
                .map_err(|error| worker_app_error(&request.id, "IO", "fixture.write.failed", json!({"message": error.to_string()})))?;
            let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
            Ok(json!({
                "grant": ArtifactGrantDescriptor {
                    grant_id: request.id.clone(), digest: digest.clone(), size: bytes.len() as u64,
                    path: path.to_string_lossy().into_owned(), access: ArtifactGrantAccess::Read,
                },
                "digest": digest,
            }))
        }
        "math.sum" => {
            let values = request
                .params
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    worker_app_error(
                        &request.id,
                        "INVALID_ARGUMENT",
                        "worker.math.values_required",
                        json!({ "field": "values" }),
                    )
                })?;
            let mut sum = 0_f64;
            for (index, value) in values.iter().enumerate() {
                sum += value.as_f64().ok_or_else(|| {
                    worker_app_error(
                        &request.id,
                        "INVALID_ARGUMENT",
                        "worker.math.number_required",
                        json!({ "index": index }),
                    )
                })?;
            }
            Ok(json!({ "sum": sum }))
        }
        "artifact.sha256" => hash_granted_artifact(&request).await,
        "process.crash" => std::process::exit(41),
        _ => Err(worker_app_error(
            &request.id,
            "UNKNOWN_METHOD",
            "worker.method_unknown",
            json!({ "method": method }),
        )),
    }
}

async fn hash_granted_artifact(
    request: &WorkerRequest,
) -> Result<Value, Box<mintcat_sdk::AppError>> {
    let grant_id = request
        .params
        .get("grantId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let grant = request
        .grants
        .iter()
        .find(|grant| grant.grant_id == grant_id && grant.access == ArtifactGrantAccess::Read)
        .ok_or_else(|| {
            worker_app_error(
                &request.id,
                "PERMISSION_DENIED",
                "worker.artifact.grant_required",
                json!({ "grantId": grant_id }),
            )
        })?;
    let mut file = tokio::fs::File::open(&grant.path).await.map_err(|error| {
        worker_app_error(
            &request.id,
            "ARTIFACT_UNAVAILABLE",
            "worker.artifact.open_failed",
            json!({ "grantId": grant_id, "message": error.to_string() }),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).await.map_err(|error| {
            worker_app_error(
                &request.id,
                "ARTIFACT_UNAVAILABLE",
                "worker.artifact.read_failed",
                json!({ "grantId": grant_id, "message": error.to_string() }),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size += count as u64;
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    if digest != grant.digest || size != grant.size {
        return Err(worker_app_error(
            &request.id,
            "ARTIFACT_CORRUPT",
            "worker.artifact.digest_mismatch",
            json!({ "grantId": grant_id }),
        ));
    }
    Ok(json!({ "digest": digest, "size": size }))
}
