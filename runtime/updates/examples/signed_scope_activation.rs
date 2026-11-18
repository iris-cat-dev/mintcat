#[path = "../tests/common/mod.rs"] mod common;
use common::Fixture;
use mintcat_runtime_supervisor::Invocation;
use mintcat_runtime_updates::{LeaseKind, UpdateError};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let worker = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: signed_scope_activation <mintcat-update-game-fixture executable>")?);
    let fixture = Fixture::new().await;
    let scope = "game:drg";
    let (source, record) = fixture.package(scope, 1, b"TEST ONLY old data", Some(&worker), false);
    fixture.publish(scope, 1, record.clone(), source);
    let first = fixture.install(scope).await;
    let task = fixture.updates.acquire_lease(Some(scope), &first.generation.generation_id, "in-flight-install", LeaseKind::Task, None)?;
    let old_supervisor = fixture.updates.supervisor_for_generation(scope, &first.generation.generation_id)?.ok_or("old worker missing")?;
    let receipt = fixture.updates.retain_install_artifacts("saved-receipt", &[record.digest], false)?;
    let (source, record) = fixture.package(scope, 2, b"TEST ONLY new data", Some(&worker), false);
    fixture.publish(scope, 2, record, source);
    let candidate = fixture.stage(scope).await;
    if !matches!(fixture.updates.prepare(&candidate.operation_id, 100).await, Err(UpdateError::Busy(_))) { return Err("running game job bypassed safe point".into()); }
    fixture.updates.release_lease(&task.lease_id)?;
    fixture.updates.prepare(&candidate.operation_id, 100).await?;
    fixture.updates.activate(&candidate.operation_id, 100)?;
    fixture.updates.confirm(&candidate.operation_id, 100).await?;
    let old_result = old_supervisor.invoke(Invocation { scope_id: scope, contract_id: mintcat_game_api::GAME_ADAPTER_CONTRACT_ID, contract_version: mintcat_game_api::GAME_ADAPTER_CONTRACT_VERSION, method: "math.sum", params: json!({"values":[1,5]}), grants: &[], deadline: Duration::from_secs(5) }).await?;
    if old_result != json!({"sum":6.0}) { return Err("saved receipt lost exact old worker".into()); }
    println!("{}", serde_json::to_string_pretty(&json!({"signedRepository":fixture.repository.path(),"oldGeneration":first.generation.generation_id,"newGeneration":candidate.generation.generation_id,"receiptDigests":receipt.digests,"oldResult":old_result,"phase":"confirmed","safePoint":"running task finishes before activation; receipt retains exact old bytes"}))?);
    fixture.updates.shutdown().await?;
    Ok(())
}
