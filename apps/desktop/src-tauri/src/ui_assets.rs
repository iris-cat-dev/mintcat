use mintcat_runtime_artifacts::{ArtifactStore, ModuleLease};
use mintcat_runtime_ipc::IpcClient;
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_runtime_release::{PackageRecord, RootMetadata, Signed};
use mintcat_sdk::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    io::Read,
    sync::{Arc, Mutex, Weak},
};
use tauri::{
    http::{Request, Response, StatusCode},
    Manager, State,
};
use uuid::Uuid;

const MAX_GENERATION_BYTES: u64 = 64 * 1024 * 1024;
struct Asset {
    bytes: Arc<Vec<u8>>,
    mime: &'static str,
}
struct Generation {
    assets: BTreeMap<String, Asset>,
    bindings: BTreeMap<String, String>,
    _leases: Vec<ModuleLease>,
    data_dir: String,
    generation_id: String,
}
#[derive(Default)]
pub struct UiAssets {
    generations: Mutex<HashMap<String, Generation>>,
    buffers: Mutex<HashMap<String, Weak<Vec<u8>>>>,
}

#[tauri::command]
pub async fn ui_open_generation(
    data_dir: String,
    generation_id: String,
    state: State<'_, UiAssets>,
) -> Result<Value, String> {
    let token = Uuid::new_v4().to_string();
    // The daemon lease protects the complete frozen environment before any
    // package read; the host's ModuleLeases additionally retain native objects.
    daemon_result(
        &data_dir,
        "ui.session.open",
        json!({"sessionId":token,"generationId":generation_id}),
    )
    .await?;
    let result = open_generation(&data_dir, &generation_id, &token, &state).await;
    if result.is_err() {
        let _ = daemon_result(&data_dir, "ui.session.close", json!({"sessionId":token})).await;
    }
    result
}

async fn daemon_result(data_dir: &str, method: &str, params: Value) -> Result<Value, String> {
    let response = super::request_running_daemon_command(data_dir, method, params, None).await?;
    if let Some(error) = response.error {
        return Err(format!("{}: {}", error.code, error.message_key));
    }
    response
        .result
        .ok_or_else(|| format!("{method} returned no result"))
}

async fn open_generation(
    data_dir: &str,
    generation_id: &str,
    token: &str,
    state: &UiAssets,
) -> Result<Value, String> {
    let paths = RuntimePaths::new(data_dir).map_err(|e| e.to_string())?;
    // Only the authenticated daemon's protected admitted provenance authorizes
    // these roots. A valid self-signature alone never admits a frontend root.
    let authority = daemon_result(
        data_dir,
        "ui.artifacts.authority",
        json!({"expectedGenerationId":generation_id}),
    )
    .await?;
    if authority["generationId"].as_str() != Some(generation_id) {
        return Err("UI generation changed; refresh modules.list".into());
    }
    let modules = authority["modules"]
        .as_array()
        .ok_or("UI authority modules must be an array")?;
    let layout = paths.layout();
    let mut generation = Generation {
        assets: BTreeMap::new(),
        bindings: BTreeMap::new(),
        _leases: Vec::new(),
        data_dir: data_dir.to_owned(),
        generation_id: generation_id.to_owned(),
    };
    let mut descriptors = Vec::new();
    let mut total = 0_u64;
    {
        let mut buffers = state.buffers.lock().map_err(|e| e.to_string())?;
        buffers.retain(|_, bytes| bytes.strong_count() > 0);
        for binding in modules {
            let digest = binding["digest"].as_str().ok_or("module digest missing")?;
            let package: PackageRecord =
                serde_json::from_value(binding["package"].clone()).map_err(|e| e.to_string())?;
            let frozen_root: Signed<RootMetadata> =
                serde_json::from_value(binding["verifiedRoot"].clone())
                    .map_err(|e| e.to_string())?;
            // Freshness is enforced at release admission, not when an already
            // admitted frozen package is read offline under its exact original root.
            let root = mintcat_runtime_release::trust_pinned_root(frozen_root, 0)
                .map_err(|e| e.to_string())?;
            let store = ArtifactStore::from_layout(
                &layout,
                RuntimeEnvironment::Production,
                env!("MINTCAT_TARGET"),
                "1",
                root.artifact_trust_store().map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let lease = store.acquire_module(digest).map_err(|e| e.to_string())?;
            let module = lease.module();
            if binding["moduleId"].as_str() != Some(&module.manifest.id)
                || package.digest != digest
                || package.manifest != module.manifest
                || package.signature != module.signature
            {
                return Err("module binding does not match exact admitted signed package".into());
            }
            if !module
                .manifest
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                return Err("UI module identity must use portable ASCII URL characters".into());
            }
            if !module
                .manifest
                .files
                .iter()
                .any(|file| file.path == "ui/contributions.json")
            {
                continue;
            }
            if generation
                .bindings
                .insert(module.manifest.id.clone(), digest.to_owned())
                .is_some()
            {
                return Err("Duplicate UI module identity".into());
            }
            let mut descriptor = None;
            for file in &module.manifest.files {
                if !file.path.starts_with("ui/") {
                    continue;
                }
                if !file
                    .path
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
                {
                    return Err("UI resource paths must use portable ASCII URL characters".into());
                }
                total = total
                    .checked_add(file.size)
                    .ok_or("UI byte count overflow")?;
                if total > MAX_GENERATION_BYTES {
                    return Err("UI generation exceeds 64 MiB limit".into());
                }
                let bytes = if let Some(bytes) = buffers.get(&file.sha256).and_then(Weak::upgrade) {
                    bytes
                } else {
                    let path = module.entry_path(&file.path).map_err(|e| e.to_string())?;
                    let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        return Err("UI resource is not a regular file".into());
                    }
                    let mut bytes = Vec::with_capacity(file.size as usize + 1);
                    std::fs::File::open(&path)
                        .map_err(|e| e.to_string())?
                        .take(file.size + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    if bytes.len() as u64 != file.size
                        || format!("sha256:{:x}", Sha256::digest(&bytes)) != file.sha256
                    {
                        return Err(format!("Signed UI file changed: {}", file.path));
                    }
                    let bytes = Arc::new(bytes);
                    buffers.insert(file.sha256.clone(), Arc::downgrade(&bytes));
                    bytes
                };
                if file.path == "ui/contributions.json" {
                    descriptor =
                        Some(serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())?);
                }
                let key = format!(
                    "{}/{}/{}/{}",
                    token,
                    module.manifest.id,
                    &digest[7..],
                    file.path
                );
                if generation
                    .assets
                    .insert(
                        key,
                        Asset {
                            mime: mime(&file.path),
                            bytes,
                        },
                    )
                    .is_some()
                {
                    return Err("Duplicate module UI identity".into());
                }
            }
            let manifest = descriptor.ok_or("UI contributions missing")?;
            if manifest["moduleId"].as_str() != Some(&module.manifest.id)
                || manifest["schemaVersion"] != 1
                || manifest["runtimeVersion"] != 1
            {
                return Err("UI descriptor identity or ABI mismatch".into());
            }
            descriptors.push(json!({"digest":digest,"manifest":manifest,"files":module.manifest.files.iter().filter(|f|f.path.starts_with("ui/")).map(|f|f.path.clone()).collect::<Vec<_>>() }));
            generation._leases.push(lease);
        }
    }
    {
        let mut generations = state.generations.lock().map_err(|e| e.to_string())?;
        if generations.len() >= 8 {
            return Err("Too many UI generations; close old generations or restart WebView".into());
        }
        generations.insert(token.to_owned(), generation);
    }
    Ok(json!({"token":token,"generationId":generation_id,"modules":descriptors}))
}
#[tauri::command]
pub async fn ui_close_generation(token: String, state: State<'_, UiAssets>) -> Result<(), String> {
    let generation = {
        state
            .generations
            .lock()
            .map_err(|e| e.to_string())?
            .remove(&token)
    };
    if let Some(generation) = generation {
        let response = super::request_running_daemon_command(
            &generation.data_dir,
            "ui.session.close",
            json!({"sessionId":token}),
            None,
        )
        .await?;
        if let Some(error) = response.error {
            return Err(format!("{}: {}", error.code, error.message_key));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn ui_request_generation(
    token: String,
    module_id: String,
    method: String,
    params: Value,
    idempotency_key: Option<String>,
    state: State<'_, UiAssets>,
) -> Result<JsonRpcResponse, String> {
    if method.starts_with("ui.") {
        return Err("UI transport/control commands cannot be nested business requests".into());
    }
    let (data_dir, generation_id, digest) = {
        let generations = state.generations.lock().map_err(|e| e.to_string())?;
        let generation = generations.get(&token).ok_or("UI generation is closed")?;
        let digest = generation
            .bindings
            .get(&module_id)
            .ok_or("Module is not in this verified UI generation")?;
        (
            generation.data_dir.clone(),
            generation.generation_id.clone(),
            digest.clone(),
        )
    };
    let paths = RuntimePaths::new(data_dir).map_err(|e| e.to_string())?;
    let mut client = IpcClient::connect(&paths)
        .await
        .map_err(|e| e.to_string())?;
    let inner = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: JsonRpcId::String(Uuid::new_v4().to_string()),
        method,
        contract_version: 1,
        session_generation: client.generation_id.clone(),
        deadline_ms: super::APPLICATION_REQUEST_DEADLINE_MS,
        idempotency_key,
        params,
    };
    // Backend admission binds the lease and exact environment/module before the
    // standard validated business dispatcher. There is no authorize/RPC gap.
    let response = super::request(&mut client, "ui.request", json!({
        "sessionId":token,"generationId":generation_id,"moduleId":module_id,"digest":digest,"request":inner
    }), None).await?;
    if let Some(error) = response.error {
        return Err(format!("{}: {}", error.code, error.message_key));
    }
    serde_json::from_value(response.result.ok_or("ui.request returned no result")?)
        .map_err(|e| e.to_string())
}
pub fn respond(app: &tauri::AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let state = app.state::<UiAssets>();
    let Ok(generations) = state.generations.lock() else {
        return reply(
            StatusCode::INTERNAL_SERVER_ERROR,
            "text/plain",
            b"UI registry unavailable".to_vec(),
        );
    };
    let path = request.uri().path().trim_start_matches('/');
    // No URL decoding or filesystem lookup: an exact signed path is the entire authority.
    let Some(token) = path.split('/').next() else {
        return reply(StatusCode::NOT_FOUND, "text/plain", Vec::new());
    };
    if request.method() != "GET" || request.uri().query().is_some() {
        return reply(StatusCode::FORBIDDEN, "text/plain", Vec::new());
    }
    let Some(asset) = generations
        .get(token)
        .and_then(|generation| generation.assets.get(path))
    else {
        return reply(
            StatusCode::NOT_FOUND,
            "text/plain",
            b"Resource is not in this signed UI generation".to_vec(),
        );
    };
    reply(StatusCode::OK, asset.mime, asset.bytes.as_ref().clone())
}
fn reply(status: StatusCode, mime: &str, bytes: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Access-Control-Allow-Origin", "*")
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(bytes)
        .expect("static response headers")
}
fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}
