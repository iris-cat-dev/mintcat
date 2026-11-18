//! TEST-ONLY host for smoke-games.mjs. Uses the production supervisor/SDK and signed
//! package grants, but only isolated synthetic PAKs and non-installable marker ZIPs.
//! Built in the runner's temporary workspace; not a product binary or a game worker.
use std::{fs, io::Cursor, path::Path, time::{Duration, SystemTime, UNIX_EPOCH}};

use anyhow::{ensure, Context, Result};
use mintcat_game_api::{
    GameBuildPlan, GameDescriptor, WorkerGameBuildRequest, WorkerGamePreparationResponse,
    WorkerInputGrant, GAME_ADAPTER_CONTRACT_ID, GAME_ADAPTER_CONTRACT_VERSION,
};
use mintcat_integrator_core::uasset_utils::asset_registry::{AssetRegistry, Writable};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_runtime_release::{load_trust_root, read_json, verify_package, VerificationPolicy};
use mintcat_runtime_supervisor::{Invocation, ModuleSupervisor, SupervisorError};
use mintcat_sdk::ScopedProviderBinding;
use mintcat_unreal_pak::{PakBuildTarget, PakBundleWriter};
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let proof = fs::canonicalize(args.next().context("expected one isolated proof directory")?)?;
    ensure!(args.next().is_none(), "expected one isolated proof directory");
    let marker = fs::read_to_string(proof.parent().context("proof parent is absent")?.join("TEST-ONLY"))?;
    ensure!(marker.starts_with("TEST-ONLY MintCat game worker smoke;"), "refusing a non-fixture directory");
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let root = load_trust_root(&proof.join("root.json"), now)?;
    let policy: VerificationPolicy = read_json(&proof.join("policy.json"))?;
    let record = verify_package(&proof.join("package"), &root, &policy)?;
    let scope = record.manifest.contributes["scopeId"].as_str().context("signed game scope absent")?;
    ensure!(matches!(scope, "game:drg" | "game:rogue-core"), "unexpected fixture game scope");
    let trust = root.artifact_trust_store()?.restrict_to_modules([(&record.manifest, &record.signature)])?;
    let runtime = proof.join("worker-runtime-test");
    fs::create_dir(&runtime)?;
    let layout = RuntimePaths::new(runtime.join("state"))?.initialize(RuntimeEnvironment::Test)?;
    let store = ArtifactStore::from_layout(&layout, RuntimeEnvironment::Test,
        &policy.target, &policy.host_api, trust)?;
    let installed = store.publish_module(&proof.join("package/payload"), &record.manifest, &record.signature)?;
    ensure!(installed.digest == record.digest, "installed signed identity differs from checked package");
    let supervisor = ModuleSupervisor::new(store.clone(), "TEST-ONLY-game-worker-proof", &runtime)?;
    let instance = supervisor.activate(ScopedProviderBinding {
        scope_id: scope.into(), contract_id: GAME_ADAPTER_CONTRACT_ID.into(),
        contract_version: GAME_ADAPTER_CONTRACT_VERSION,
        provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone(),
    }).await?;
    let result = exercise(&supervisor, &store, scope, &record.digest, &record.manifest.contributes["games"][0], &proof).await;
    // Always drain our own worker, including after a failed assertion.
    let drained = supervisor.drain_binding(scope, GAME_ADAPTER_CONTRACT_ID, GAME_ADAPTER_CONTRACT_VERSION).await;
    let mut evidence = result?;
    drained?;
    evidence["instanceId"] = json!(instance);
    evidence["shutdown"] = json!("PASS supervisor drain");
    fs::write(proof.join("worker-proof.json"), serde_json::to_vec_pretty(&evidence)?)?;
    println!("PASS {scope}: actual worker handshake/describe/discovery/validation/planning/preparation, scoped binding and signed-resource authorization; marker-only patch rejected");
    Ok(())
}

async fn invoke(supervisor: &ModuleSupervisor, scope: &str, method: &str, params: Value,
    grants: &[ArtifactGrant]) -> std::result::Result<Value, SupervisorError> {
    supervisor.invoke(Invocation {
        scope_id: scope, contract_id: GAME_ADAPTER_CONTRACT_ID,
        contract_version: GAME_ADAPTER_CONTRACT_VERSION, method, params, grants,
        deadline: Duration::from_secs(30),
    }).await
}

fn rejected(result: std::result::Result<Value, SupervisorError>, code: &str, message: &str) -> Result<Value> {
    match result {
        Err(SupervisorError::WorkerApplication { code: actual, message_key, details }) => {
            ensure!(actual == code && details["message"].as_str().is_some_and(|detail| detail.contains(message)),
                "unexpected rejection: {actual} {message_key} {details}; expected {code}: {message}");
            Ok(json!({"status":"PASS", "code":actual, "messageKey":message_key, "details":details}))
        }
        other => anyhow::bail!("expected worker application rejection {code}: {message}; got {other:?}"),
    }
}

fn pak(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut writer = PakBundleWriter::new(Cursor::new(Vec::new()), &[], &PakBuildTarget::default())?;
    writer.write_file(path, bytes)?;
    Ok(writer.finish()?.into_inner())
}

async fn exercise(supervisor: &ModuleSupervisor, store: &ArtifactStore, scope: &str,
    digest: &str, signed_descriptor: &Value, proof: &Path) -> Result<Value> {
    let described = invoke(supervisor, scope, "describe", json!({}), &[]).await?;
    ensure!(&described == signed_descriptor, "actual worker descriptor differs from its signed game manifest");
    let descriptor: GameDescriptor = serde_json::from_value(described)?;
    let variant = descriptor.variants.iter().find(|variant| variant.store == "steam")
        .context("signed Steam variant absent")?;
    let installation_root = proof.join("worker-runtime-test/TEST-ONLY-synthetic-game-layout");
    fs::create_dir(&installation_root)?;
    fs::create_dir_all(installation_root.join(&variant.paks_relative_path))?;
    fs::create_dir_all(installation_root.join(&variant.binaries_relative_path))?;
    fs::write(installation_root.join("TEST-ONLY"), "Synthetic registry/PAK layout, NOT a licensed game installation.\n")?;
    let content = if scope == "game:drg" { "FSD" } else { "RogueCore" };
    let mut registry = Vec::new();
    AssetRegistry::empty().write(&mut registry)?;
    let base_path = installation_root.join(&variant.main_pak_relative_path);
    fs::write(&base_path, pak(&format!("{content}/AssetRegistry.bin"), &registry)?)?;
    let base = store.publish_blob_file(&base_path, Some("application/vnd.unreal.pak".into()))?;
    let mod_bytes = pak(&format!("{content}/Content/Mods/TEST-ONLY/payload.bin"), b"TEST-ONLY worker inspection input")?;
    let mod_artifact = store.publish_blob_reader(&mut Cursor::new(mod_bytes), Some("application/vnd.unreal.pak".into()))?;
    let grants = [store.grant_blob(&base)?, store.grant_blob(&mod_artifact)?];
    let hint = json!({"rootPath":installation_root,"variantId":variant.variant_id});
    let discovered = invoke(supervisor, scope, "installation.discover",
        json!({"roots":[hint.clone()],"includeSteam":false}), &[]).await?;
    let candidates = discovered.as_array().context("discovery did not return an array")?;
    ensure!(candidates.len() == 1 && candidates[0]["gameId"] == scope
        && candidates[0]["rootPath"] == json!(installation_root), "discovery escaped the isolated fixture hint");
    let installation = invoke(supervisor, scope, "installation.validate", hint, &[]).await?;
    ensure!(installation["fingerprint"] == base.digest, "validated fixture fingerprint differs from host artifact grant");
    let settings = invoke(supervisor, scope, "settings.snapshot",
        json!({"revision":1,"options":{"compression":"none","ue4ssEnabled":false}}), &[]).await?;
    let plan: GameBuildPlan = serde_json::from_value(invoke(supervisor, scope, "build.plan", json!({
        "installation":installation, "profileRevision":1, "moduleLockDigest":digest, "settings":settings,
        "baseGameArtifact":base, "patchResourceLockDigest":digest,
        "orderedInputs":[{"inputId":"TEST-ONLY-input", "displayName":"TEST-ONLY input", "sourceId":"local",
            "remoteGameKey":scope.strip_prefix("game:").context("game scope prefix absent")?,
            "remoteModId":"TEST-ONLY", "format":"pak", "artifact":mod_artifact, "audioOnlyHint":false}]
    }), &[]).await?)?;
    let lease = store.acquire_module(digest)?;
    let package = store.module_worker_grant(&lease)?;
    let archive = mintcat_game_api::bundled_resources(&package.manifest)?.into_iter()
        .find(|resource| descriptor.resources.iter().any(|requirement| !requirement.optional
            && requirement.resource_id == resource.resource_id))
        .and_then(|resource| resource.archive_path).context("signed mandatory patch archive absent")?;
    let params = WorkerGameBuildRequest {
        plan, dependencies: Vec::new(), base_grant_id: grants[0].descriptor.grant_id.clone(),
        inputs: vec![WorkerInputGrant { input_id:"TEST-ONLY-input".into(),
            grant_id:grants[1].descriptor.grant_id.clone(), mod_id:"TEST-ONLY".into() }],
        patch_package: Some(package), patch_archive_path: Some(archive), resources: Vec::new(),
    };
    let prepared: WorkerGamePreparationResponse = serde_json::from_value(invoke(supervisor, scope,
        "build.prepare", serde_json::to_value(&params)?, &grants).await?)?;
    ensure!(prepared.conflicts.is_empty() && prepared.companion_manifests.is_empty() && prepared.grants.is_empty(),
        "unexpected outputs from single-PAK inspection with UE4SS disabled");
    let unauthorized_input = rejected(invoke(supervisor, scope, "build.prepare",
        serde_json::to_value(&params)?, &grants[..1]).await, "GAME_ADAPTER_FAILED", "was not found")?;
    let mut forged = params.clone();
    let signature = &mut forged.patch_package.as_mut().context("fixture patch absent")?.signature.signature;
    let replacement = if signature.starts_with('A') { "B" } else { "A" };
    signature.replace_range(0..1, replacement);
    let forged_signature = rejected(invoke(supervisor, scope, "build.execute", serde_json::to_value(forged)?, &grants).await,
        "ARTIFACT_UNAVAILABLE", "module signature verification failed")?;
    let mut wrong_archive = params.clone();
    wrong_archive.patch_archive_path = Some("resources/NOT-SIGNED.zip".into());
    let unsigned_archive = rejected(invoke(supervisor, scope, "build.execute", serde_json::to_value(wrong_archive)?, &grants).await,
        "INVALID_ARGUMENT", "patch signed identity or archive differs from plan")?;
    let marker_rejection = rejected(invoke(supervisor, scope, "build.execute", serde_json::to_value(params)?, &grants).await,
        "GAME_ADAPTER_FAILED", "undeclared patch resource path TEST-ONLY-NOT-INSTALLABLE.txt")?;
    let other_scope = if scope == "game:drg" { "game:rogue-core" } else { "game:drg" };
    ensure!(matches!(invoke(supervisor, other_scope, "describe", json!({}), &[]).await,
        Err(SupervisorError::ProviderUnavailable { .. })), "game provider escaped its activated scope");
    Ok(json!({
        "testOnly":true, "gameId":scope, "moduleDigest":digest,
        "protocol":"PASS production supervisor validates handshake, manifest identity and health",
        "describe":"PASS exact signed descriptor", "discovery":discovered,
        "installation":"PASS synthetic layout and immutable artifact fingerprint only",
        "planning":"PASS actual worker build.plan with signed resource digest",
        "preparation":"PASS actual worker build.prepare inspects authorized synthetic PAK input; signed patch context transported",
        "unauthorizedInput":unauthorized_input, "forgedResourceSignature":forged_signature,
        "unsignedArchivePath":unsigned_archive, "nonInstallableMarkerArchive":marker_rejection,
        "scopeIsolation":"PASS unbound other-game invocation denied",
        "realGameInstallInputs":"UNAVAILABLE: licensed game data, authentic integration archives, optional UE4SS assets",
        "deployment":"NOT ATTEMPTED; marker archive rejected before build outputs or deployment"
    }))
}
