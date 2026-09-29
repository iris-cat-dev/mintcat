use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::ptr;

use anyhow::{Context, Result};
use libloading::Library;
use mintcat_integrator_api::{InstallEvent, InstallProgress, InstallRequest};
use serde::{Deserialize, Serialize};
use tauri::{path::BaseDirectory, AppHandle, Manager};

const ABI_VERSION: u32 = 1;
const BUNDLED_RUNTIME_VERSION: &str = "bundled";
const PUBLIC_KEY: &str = "";

type ProgressCallback = unsafe extern "C" fn(event_json: *const c_char, user_data: *mut c_void);
type AbiVersionFn = unsafe extern "C" fn() -> u32;
type BackendInvokeFn = unsafe extern "C" fn(
    request_json: *const c_char,
    callback: Option<ProgressCallback>,
    user_data: *mut c_void,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32;
type FreeStringFn = unsafe extern "C" fn(ptr: *mut c_char);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratorRuntimeManifest {
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub min_app_version: Option<String>,
    #[serde(default)]
    pub max_app_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IntegratorRuntimeState {
    #[serde(default)]
    active_version: Option<String>,
    #[serde(default)]
    previous_version: Option<String>,
    #[serde(default)]
    last_failed_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratorRuntimeStatus {
    pub active_version: Option<String>,
    pub previous_version: Option<String>,
    pub last_failed_version: Option<String>,
    pub has_local_runtime: bool,
}

struct ProgressBridge<'a> {
    progress: &'a dyn InstallProgress,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackendInvokeEnvelope<'a> {
    command: &'a str,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallModsInvokePayload {
    game_path: String,
    mod_list_json: Box<str>,
    #[serde(default)]
    skip_ue4ss: bool,
    #[serde(default)]
    ue4ss_zip_path: Option<String>,
    #[serde(default)]
    drg_zip_path: Option<String>,
    #[serde(default)]
    rc_zip_path: Option<String>,
    #[serde(default)]
    compress_mod_pak: Option<bool>,
}

#[tauri::command]
pub async fn backend_invoke(
    app: AppHandle,
    command: String,
    mut payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if is_proxy_backend_command(&command) {
        payload = inject_proxy_host_context(&app, payload).map_err(|error| format!("{error:#}"))?;
    }

    if command == "install_mods" {
        let install_payload = serde_json::from_value::<InstallModsInvokePayload>(payload)
            .map_err(|error| format!("invalid install_mods payload: {error}"))?;
        let mods = serde_json::from_str::<Vec<mintcat_integrator_api::ModInfo>>(
            &install_payload.mod_list_json,
        )
        .map_err(|error| format!("invalid mod list JSON: {error:#}"))?;
        payload = serde_json::to_value(InstallRequest::new(
            install_payload.game_path,
            mods,
            install_payload.skip_ue4ss,
            install_payload.ue4ss_zip_path,
            install_payload.drg_zip_path,
            install_payload.rc_zip_path,
            install_payload.compress_mod_pak.unwrap_or(false),
        ))
        .map_err(|error| error.to_string())?;
        std::thread::spawn(move || {
            let progress = crate::integrator::progress::TauriInstallProgress::new(app.clone());
            if let Err(error) =
                invoke_backend_runtime_command(&app, &command, payload, Some(&progress))
            {
                let _ = progress.emit(InstallEvent::Error(mintcat_integrator_api::text(format!(
                    "{error:#}"
                ))));
            }
        });
        return Ok(serde_json::json!({ "ok": true }));
    }

    invoke_backend_runtime_command(&app, &command, payload, None)
        .map_err(|error| format!("{error:#}"))
}

pub fn stop_proxy_runtime_blocking(app: &AppHandle) -> Result<()> {
    let payload = inject_proxy_host_context(app, serde_json::json!({}))?;
    let _ = invoke_backend_runtime_command(app, "stop_proxy_runtime", payload, None)?;
    Ok(())
}

fn is_proxy_backend_command(command: &str) -> bool {
    matches!(
        command,
        "install_proxy_runtime_from_manifest"
            | "get_proxy_runtime_status"
            | "start_proxy_runtime"
            | "stop_proxy_runtime"
            | "install_proxy_cert"
    )
}

fn inject_proxy_host_context(
    app: &AppHandle,
    mut payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let root_dir = app
        .path()
        .app_data_dir()
        .context("failed to resolve app data dir")?
        .join("plugins")
        .join("proxy")
        .to_string_lossy()
        .to_string();
    let bundled_runtime_path = app
        .path()
        .resolve(
            format!("plugins/proxy/{}", proxy_runtime_file_name()),
            BaseDirectory::Resource,
        )
        .ok()
        .map(|path| path.to_string_lossy().to_string());
    let manual_proxy = app
        .try_state::<crate::network::NetworkProxyState>()
        .and_then(|state| state.get());
    let proxy_url = crate::network::resolve_proxy(manual_proxy);
    let context = serde_json::json!({
        "rootDir": root_dir,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "bundledRuntimePath": bundled_runtime_path,
        "proxyUrl": proxy_url,
    });

    match &mut payload {
        serde_json::Value::Object(map) => {
            map.insert("context".to_string(), context);
            Ok(payload)
        }
        _ => Ok(serde_json::json!({
            "context": context
        })),
    }
}

fn proxy_runtime_file_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "mintcat-proxy.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "mintcat-proxy"
    }
}

#[tauri::command]
pub async fn install_integrator_runtime_from_manifest(
    app: AppHandle,
    manifest: IntegratorRuntimeManifest,
) -> Result<IntegratorRuntimeStatus, String> {
    validate_manifest(&manifest)?;
    validate_compatibility(&manifest)?;
    let bytes = download(&app, &manifest.url).await?;
    verify_hash(&bytes, &manifest)?;
    verify_signature(&manifest)?;

    let version_dir = versions_dir(&app)?.join(&manifest.version);
    fs::create_dir_all(&version_dir).map_err(|e| e.to_string())?;
    let runtime_path = version_dir.join(runtime_file_name());
    install_runtime_payload(&bytes, &runtime_path).map_err(|e| format!("{:#}", e))?;

    unsafe {
        validate_abi(&runtime_path).map_err(|e| format!("{:#}", e))?;
    }

    let mut state = read_state(&app);
    state.previous_version = state.active_version.clone();
    state.active_version = Some(manifest.version);
    state.last_failed_version = None;
    write_state(&app, &state)?;
    cleanup_old_versions(&app, &state);
    status(app)
}

#[tauri::command]
pub fn get_integrator_runtime_status(app: AppHandle) -> Result<IntegratorRuntimeStatus, String> {
    if let Err(error) = ensure_bundled_runtime(&app) {
        log::warn!(
            "[IntegratorRuntime] failed to prepare bundled runtime: {:#}",
            error
        );
    }
    status(app)
}

pub fn ensure_bundled_runtime(app: &AppHandle) -> Result<()> {
    if active_downloaded_library_path(app).is_some() {
        return Ok(());
    }

    let bundled = bundled_runtime_path(app)?;
    if !bundled.is_file() {
        return Ok(());
    }

    unsafe {
        validate_abi(&bundled).context("bundled integrator ABI validation failed")?;
    }
    Ok(())
}

fn active_runtime_path(app: &AppHandle) -> Result<PathBuf> {
    if let Err(error) = ensure_bundled_runtime(app) {
        log::warn!(
            "[IntegratorRuntime] failed to prepare bundled runtime: {:#}",
            error
        );
    }
    active_library_path(app).context("integrator runtime is not available")
}

fn invoke_backend_runtime_command(
    app: &AppHandle,
    command: &str,
    payload: serde_json::Value,
    progress: Option<&dyn InstallProgress>,
) -> Result<serde_json::Value> {
    let path = active_runtime_path(app)?;
    unsafe { call_backend_runtime_command(&path, command, payload, progress) }
}

unsafe fn call_backend_runtime_command(
    path: &Path,
    command: &str,
    payload: serde_json::Value,
    progress: Option<&dyn InstallProgress>,
) -> Result<serde_json::Value> {
    let library = Library::new(path)
        .with_context(|| format!("failed to load backend runtime: {:?}", path))?;
    validate_library_abi(&library)?;
    let invoke = *library
        .get::<BackendInvokeFn>(b"mintcat_backend_runtime_invoke")
        .context("missing mintcat_backend_runtime_invoke")?;
    let free_string = *library
        .get::<FreeStringFn>(b"mintcat_integrator_free_string")
        .context("missing mintcat_integrator_free_string")?;

    let envelope = BackendInvokeEnvelope { command, payload };
    let request_json =
        CString::new(serde_json::to_string(&envelope)?).context("request contains nul byte")?;
    let mut response_json: *mut c_char = ptr::null_mut();
    let mut error_message: *mut c_char = ptr::null_mut();
    let bridge = progress.map(|progress| ProgressBridge { progress });
    let user_data = bridge
        .as_ref()
        .map(|bridge| bridge as *const ProgressBridge<'_> as *mut c_void)
        .unwrap_or(ptr::null_mut());
    let code = invoke(
        request_json.as_ptr(),
        progress.map(|_| progress_callback as ProgressCallback),
        user_data,
        &mut response_json,
        &mut error_message,
    );

    let error = read_and_free(error_message, free_string);
    let response = read_and_free(response_json, free_string);
    if code != 0 {
        anyhow::bail!(error.unwrap_or_else(|| {
            format!("backend runtime command {command} failed with code {code}")
        }));
    }
    let response = response.context("backend runtime command returned no response")?;
    serde_json::from_str(&response).context("failed to parse backend runtime response JSON")
}

unsafe fn validate_abi(path: &Path) -> Result<()> {
    let library = Library::new(path)
        .with_context(|| format!("failed to load integrator runtime: {:?}", path))?;
    validate_library_abi(&library)
}

unsafe fn validate_library_abi(library: &Library) -> Result<()> {
    let abi_version = *library
        .get::<AbiVersionFn>(b"mintcat_integrator_abi_version")
        .context("missing mintcat_integrator_abi_version")?;
    if abi_version() != ABI_VERSION {
        anyhow::bail!("integrator ABI mismatch");
    }
    Ok(())
}

unsafe extern "C" fn progress_callback(event_json: *const c_char, user_data: *mut c_void) {
    if event_json.is_null() || user_data.is_null() {
        return;
    }
    let bridge = &*(user_data as *const ProgressBridge<'_>);
    let event = CStr::from_ptr(event_json)
        .to_str()
        .ok()
        .and_then(|value| serde_json::from_str::<InstallEvent>(value).ok());
    if let Some(event) = event {
        let _ = bridge.progress.emit(event);
    }
}

unsafe fn read_and_free(ptr: *mut c_char, free_string: FreeStringFn) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let value = CStr::from_ptr(ptr).to_string_lossy().into_owned();
    free_string(ptr);
    Some(value)
}

fn runtime_file_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "mintcat_backend_runtime.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libmintcat_backend_runtime.dylib"
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        "libmintcat_backend_runtime.so"
    }
}

fn bundled_runtime_path(app: &AppHandle) -> Result<PathBuf> {
    let resource_path = app
        .path()
        .resolve(
            format!("plugins/{}", runtime_file_name()),
            BaseDirectory::Resource,
        )
        .context("failed to resolve bundled integrator runtime")?;
    if resource_path.is_file() {
        return Ok(resource_path);
    }

    #[cfg(debug_assertions)]
    if let Some(dev_path) = dev_runtime_candidates()
        .into_iter()
        .find(|path| path.is_file())
    {
        log::info!(
            "[IntegratorRuntime] using local dev runtime: {:?}",
            dev_path
        );
        return Ok(dev_path);
    }

    Ok(resource_path)
}

#[cfg(debug_assertions)]
fn dev_runtime_candidates() -> Vec<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = option_env!("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target"));
    let profile = option_env!("PROFILE").unwrap_or("debug");
    let runtime_name = runtime_file_name();

    vec![
        manifest_dir
            .join("assets")
            .join("plugins")
            .join(runtime_name),
        target_dir.join(profile).join(runtime_name),
        target_dir.join("debug").join(runtime_name),
        target_dir.join("release").join(runtime_name),
    ]
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("plugins"))
        .map_err(|e| e.to_string())
}

fn versions_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(root(app)?.join("versions"))
}

fn state_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(root(app)?.join("state.json"))
}

fn read_state(app: &AppHandle) -> IntegratorRuntimeState {
    state_file(app)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn write_state(app: &AppHandle, state: &IntegratorRuntimeState) -> Result<(), String> {
    fs::create_dir_all(root(app)?).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    fs::write(state_file(app)?, json).map_err(|e| e.to_string())
}

fn active_library_path(app: &AppHandle) -> Option<PathBuf> {
    if let Some(path) = active_downloaded_library_path(app) {
        return Some(path);
    }

    bundled_runtime_path(app).ok().filter(|path| path.is_file())
}

fn active_downloaded_library_path(app: &AppHandle) -> Option<PathBuf> {
    let version = read_state(app).active_version?;
    if version == BUNDLED_RUNTIME_VERSION {
        return None;
    }
    valid_version(&version).ok()?;
    let path = versions_dir(app)
        .ok()?
        .join(version)
        .join(runtime_file_name());
    path.is_file().then_some(path)
}

fn status(app: AppHandle) -> Result<IntegratorRuntimeStatus, String> {
    let state = read_state(&app);
    let has_downloaded_runtime = active_downloaded_library_path(&app).is_some();
    let has_bundled_runtime = bundled_runtime_path(&app)
        .map(|path| path.is_file())
        .unwrap_or(false);
    let active_version = if has_downloaded_runtime {
        state.active_version
    } else if has_bundled_runtime {
        Some(BUNDLED_RUNTIME_VERSION.to_string())
    } else {
        None
    };

    Ok(IntegratorRuntimeStatus {
        active_version,
        previous_version: state.previous_version,
        last_failed_version: state.last_failed_version,
        has_local_runtime: has_downloaded_runtime || has_bundled_runtime,
    })
}

fn valid_version(version: &str) -> Result<(), String> {
    if version.is_empty()
        || version.contains("..")
        || version.contains('/')
        || version.contains('\\')
        || version.chars().any(|c| c.is_control())
    {
        Err("invalid integrator version".into())
    } else {
        Ok(())
    }
}

fn validate_manifest(manifest: &IntegratorRuntimeManifest) -> Result<(), String> {
    valid_version(&manifest.version)?;
    if manifest.url.trim().is_empty() {
        return Err("integrator runtime url is required".into());
    }
    if manifest
        .md5
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err("integrator runtime md5 is required".into());
    }
    Ok(())
}

fn validate_compatibility(manifest: &IntegratorRuntimeManifest) -> Result<(), String> {
    let app_version = env!("CARGO_PKG_VERSION");
    if let Some(min) = &manifest.min_app_version {
        if cmp_version(app_version, min).is_lt() {
            return Err(format!("integrator requires app >= {min}"));
        }
    }
    if let Some(max) = &manifest.max_app_version {
        if cmp_version(app_version, max).is_gt() {
            return Err(format!("integrator requires app <= {max}"));
        }
    }
    Ok(())
}

fn install_runtime_payload(bytes: &[u8], runtime_path: &Path) -> Result<()> {
    if is_zip_payload(bytes) {
        extract_runtime_from_zip(bytes, runtime_path)?;
        return Ok(());
    }
    fs::write(runtime_path, bytes).context("failed to write integrator runtime")?;
    Ok(())
}

fn is_zip_payload(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
}

fn extract_runtime_from_zip(bytes: &[u8], runtime_path: &Path) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("invalid integrator runtime zip")?;
    let Some(index) = find_runtime_zip_entry(&mut archive) else {
        anyhow::bail!("integrator runtime library not found in zip");
    };
    let mut file = archive
        .by_index(index)
        .context("failed to read integrator runtime from zip")?;
    let mut library = Vec::new();
    file.read_to_end(&mut library)
        .context("failed to extract integrator runtime from zip")?;
    fs::write(runtime_path, library).context("failed to write integrator runtime library")?;
    Ok(())
}

fn find_runtime_zip_entry<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Option<usize> {
    let expected = runtime_file_name();
    let mut fallback = None;
    for index in 0..archive.len() {
        let Ok(file) = archive.by_index(index) else {
            continue;
        };
        if file.is_dir() {
            continue;
        }
        let Some(path) = file.enclosed_name() else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.eq_ignore_ascii_case(expected) {
            return Some(index);
        }
        if fallback.is_none() && looks_like_runtime_file(name) {
            fallback = Some(index);
        }
    }
    fallback
}

fn looks_like_runtime_file(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    #[cfg(target_os = "windows")]
    {
        (normalized.starts_with("mintcat_backend_runtime")
            || normalized.starts_with("mintcat_integrator"))
            && normalized.ends_with(".dll")
    }
    #[cfg(target_os = "macos")]
    {
        (normalized.starts_with("libmintcat_backend_runtime")
            || normalized.starts_with("libmintcat_integrator"))
            && normalized.ends_with(".dylib")
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        (normalized.starts_with("libmintcat_backend_runtime")
            || normalized.starts_with("libmintcat_integrator"))
            && normalized.ends_with(".so")
    }
}

fn cmp_version(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |value: &str| {
        value
            .split(|c| c == '.' || c == '-')
            .take(3)
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(left).cmp(&parse(right))
}

async fn download(app: &AppHandle, url: &str) -> Result<Vec<u8>, String> {
    let manual_proxy = app
        .try_state::<crate::network::NetworkProxyState>()
        .and_then(|state| state.get());
    let proxy_url = crate::network::resolve_proxy(manual_proxy);
    let builder = reqwest::Client::builder();
    let client = crate::network::apply_proxy_builder(builder, proxy_url.as_deref())
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "integrator runtime download failed: {}",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| e.to_string())
}

fn verify_hash(bytes: &[u8], manifest: &IntegratorRuntimeManifest) -> Result<(), String> {
    let expected = manifest
        .md5
        .as_deref()
        .ok_or_else(|| "integrator runtime md5 is required".to_string())?;
    let actual = format!("{:x}", md5::compute(bytes));
    actual
        .eq_ignore_ascii_case(expected.trim())
        .then_some(())
        .ok_or_else(|| format!("md5 mismatch: expected {expected}, got {actual}"))
}

fn verify_signature(manifest: &IntegratorRuntimeManifest) -> Result<(), String> {
    if manifest.signature.as_deref().unwrap_or_default().is_empty() {
        #[cfg(not(debug_assertions))]
        return Err("backend runtime signature is required".into());
        log::warn!("[IntegratorRuntime] unsigned integrator runtime accepted");
        return Ok(());
    }
    if PUBLIC_KEY.is_empty() {
        #[cfg(not(debug_assertions))]
        return Err("backend runtime public key is not configured".into());
        log::warn!(
            "[IntegratorRuntime] signature is present, but public key is not configured yet"
        );
        return Ok(());
    }
    Ok(())
}

fn cleanup_old_versions(app: &AppHandle, state: &IntegratorRuntimeState) {
    let Ok(root) = versions_dir(app) else {
        return;
    };
    let keep = [
        state.active_version.as_deref(),
        state.previous_version.as_deref(),
    ];
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if keep.iter().any(|version| version == &Some(name)) {
            continue;
        }
        let _ = fs::remove_dir_all(path);
    }
}
