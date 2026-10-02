#[path = "../tests/common/mod.rs"] mod common;
use common::Fixture;
use mintcat_runtime_supervisor::Invocation;
use mintcat_runtime_updates::{LeaseKind, UiAck, UiStage};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build mintcat-runtime-supervisor's real SDK worker first, then pass its executable.
    let worker = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: signed_scope_activation <mintcat-worker-example executable>")?);
    let fixture = Fixture::new().await;
    let (source, record) = fixture.package("drg", 1, b"", Some(&worker), false); fixture.publish("drg", 1, record, source);
    let first = fixture.install("drg").await;
    let task = fixture.updates.acquire_lease(Some("drg"), &first.generation.generation_id, "in-flight-install", LeaseKind::Task, None)?;
    let old_environment = fixture.updates.environment()?;
    fixture.updates.register_ui_session("visible-workbench", &old_environment.generation_id)?;
    let old_supervisor = fixture.updates.supervisor_for_generation("drg", &first.generation.generation_id)?.ok_or("old worker missing")?;
    let before = old_supervisor.invoke(Invocation { scope_id: "drg", contract_id: "fixture.compute", contract_version: 1, method: "math.sum", params: json!({"values":[2,3,4]}), grants: &[], deadline: Duration::from_secs(5) }).await?;
    if before != json!({"sum":9.0}) { return Err("real old worker computed wrong result".into()); }
    let (source, record) = fixture.package("drg", 2, b"", Some(&worker), false); fixture.publish("drg", 2, record, source);
    let candidate = fixture.stage("drg").await;
    let prepared = fixture.updates.prepare(&candidate.operation_id, 100).await?; let barrier = prepared.barrier.ok_or("barrier missing")?;
    if fixture.updates.activate(&candidate.operation_id, 100).is_ok() { return Err("activation bypassed visible UI dispose barrier".into()); }
    fixture.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id.clone(), session_id: "visible-workbench".into(), stage: UiStage::Disposed, state: Some(json!({"route":"/fixture.drg","draft":"retained"})), error: None, rebuilt: false }).await?;
    fixture.updates.activate(&candidate.operation_id, 100)?;
    let new_supervisor = fixture.updates.supervisor_for_generation("drg", &candidate.generation.generation_id)?.ok_or("new worker missing")?;
    let result = new_supervisor.invoke(Invocation { scope_id: "drg", contract_id: "fixture.compute", contract_version: 1, method: "math.sum", params: json!({"values":[10,20]}), grants: &[], deadline: Duration::from_secs(5) }).await?;
    if result != json!({"sum":30.0}) { return Err("real new worker computed wrong result".into()); }
    fixture.updates.acknowledge_ui(UiAck { barrier_id: barrier.barrier_id, session_id: "visible-workbench".into(), stage: UiStage::Ready, state: None, error: None, rebuilt: false }).await?;
    fixture.updates.confirm(&candidate.operation_id, 100).await?;
    let old_result = old_supervisor.invoke(Invocation { scope_id: "drg", contract_id: "fixture.compute", contract_version: 1, method: "math.sum", params: json!({"values":[1,5]}), grants: &[], deadline: Duration::from_secs(5) }).await?;
    if old_result != json!({"sum":6.0}) { return Err("in-flight old task lost its worker closure".into()); }
    println!("{}", serde_json::to_string_pretty(&json!({"signedRepository":fixture.repository.path(),"oldGeneration":first.generation.generation_id,"newGeneration":candidate.generation.generation_id,"oldLeaseDigests":task.digests,"oldResult":old_result,"newResult":result,"phase":"confirmed","barrierProtocol":"disposed -> probation -> ready (protocol exercise, not visual WebView proof)"}))?);
    fixture.updates.close_ui_session("visible-workbench")?; fixture.updates.release_lease(&task.lease_id)?;
    new_supervisor.shutdown_all().await?; old_supervisor.shutdown_all().await?;
    Ok(())
}
