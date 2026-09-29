use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Url};

pub const CONTROL_PLANE_API_VERSION: u32 = 1;

#[derive(Default)]
pub struct HotUpdateStoreState {
    lock: Mutex<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotUpdateComponentKey {
    pub category: String,
    pub component: String,
    #[serde(default = "global_scope")]
    pub game: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_arch")]
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotUpdateArtifactPathRequest {
    #[serde(flatten)]
    pub key: HotUpdateComponentKey,
    pub version: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotUpdateStagingPathRequest {
    #[serde(flatten)]
    pub key: HotUpdateComponentKey,
    pub operation_id: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HotUpdateComponentState {
    #[serde(default)]
    pub active_version: Option<String>,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub failed_version: Option<String>,
    #[serde(default)]
    pub active_file_name: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateHotUpdateComponentRequest {
    #[serde(flatten)]
    pub key: HotUpdateComponentKey,
    pub version: String,
    pub file_name: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSetComponent {
    pub category: String,
    pub component: String,
    pub version: String,
    #[serde(default = "global_scope")]
    pub game: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateReleaseSetRequest {
    pub release_set_id: String,
    #[serde(default)]
    pub components: Vec<ReleaseSetComponent>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionalAssetPackRequest {
    pub capability: String,
    pub component: String,
    pub version: String,
    pub file_name: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub release_set_id: Option<String>,
    #[serde(default)]
    pub schema_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HotUpdateStoreManifest {
    #[serde(default)]
    pub active_release_set_id: Option<String>,
    #[serde(default)]
    pub failed_release_set_id: Option<String>,
    #[serde(default)]
    pub safe_mode: bool,
    #[serde(default)]
    pub capabilities: Vec<CapabilityStatus>,
    #[serde(default)]
    pub components: Vec<ReleaseSetComponent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStatus {
    pub name: String,
    pub supported: bool,
    pub installed: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneStatus {
    pub api_version: u32,
    pub active_release_set_id: Option<String>,
    pub failed_release_set_id: Option<String>,
    pub safe_mode: bool,
    pub capabilities: Vec<CapabilityStatus>,
}

fn global_scope() -> String {
    "global".to_string()
}

fn default_channel() -> String {
    "stable".to_string()
}

fn default_platform() -> String {
    #[cfg(target_os = "windows")]
    {
        "windows".to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "macos".to_string()
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        "linux".to_string()
    }
}

fn default_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("hot-update-store"))
        .map_err(|error| error.to_string())
}

fn store_manifest_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(root(app)?.join("store.json"))
}

fn scoped_dir(app: &AppHandle, key: &HotUpdateComponentKey) -> Result<PathBuf, String> {
    Ok(root(app)?
        .join(safe_segment(&key.category)?)
        .join(safe_segment(&key.channel)?)
        .join(format!(
            "{}-{}",
            safe_segment(&key.platform)?,
            safe_segment(&key.arch)?
        ))
        .join(safe_segment(&key.game)?)
        .join(safe_segment(&key.component)?))
}

fn state_file(app: &AppHandle, key: &HotUpdateComponentKey) -> Result<PathBuf, String> {
    Ok(scoped_dir(app, key)?.join("state.json"))
}

fn artifact_path(
    app: &AppHandle,
    request: &HotUpdateArtifactPathRequest,
) -> Result<PathBuf, String> {
    Ok(scoped_dir(app, &request.key)?
        .join(safe_segment(&request.version)?)
        .join(safe_file_name(&request.file_name)?))
}

fn staging_path(app: &AppHandle, request: &HotUpdateStagingPathRequest) -> Result<PathBuf, String> {
    Ok(scoped_dir(app, &request.key)?
        .join("staging")
        .join(safe_segment(&request.operation_id)?)
        .join(safe_file_name(&request.file_name)?))
}

fn safe_segment(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(|ch| ch.is_control())
    {
        return Err(format!("invalid hot update path segment: {value:?}"));
    }
    Ok(value.to_string())
}

fn safe_file_name(value: &str) -> Result<String, String> {
    let name = safe_segment(value)?;
    if Path::new(&name).file_name().and_then(|part| part.to_str()) != Some(name.as_str()) {
        return Err(format!("invalid hot update file name: {value:?}"));
    }
    Ok(name)
}

fn read_component_state(app: &AppHandle, key: &HotUpdateComponentKey) -> HotUpdateComponentState {
    state_file(app, key)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn write_component_state(
    app: &AppHandle,
    key: &HotUpdateComponentKey,
    state: &HotUpdateComponentState,
) -> Result<(), String> {
    let dir = scoped_dir(app, key)?;
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
    fs::write(dir.join("state.json"), json).map_err(|error| error.to_string())
}

fn read_store_manifest(app: &AppHandle) -> HotUpdateStoreManifest {
    store_manifest_file(app)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn write_store_manifest(app: &AppHandle, manifest: &HotUpdateStoreManifest) -> Result<(), String> {
    let root = root(app)?;
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?;
    fs::write(root.join("store.json"), json).map_err(|error| error.to_string())
}

fn with_store_lock<T>(
    app: &AppHandle,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if let Some(state) = app.try_state::<HotUpdateStoreState>() {
        let _guard = state.lock.lock().map_err(|error| error.to_string())?;
        operation()
    } else {
        operation()
    }
}

fn default_capabilities(manifest: &HotUpdateStoreManifest) -> Vec<CapabilityStatus> {
    let mut capabilities = vec![
        CapabilityStatus {
            name: "backend-runtime".to_string(),
            supported: true,
            installed: true,
            active: true,
        },
        CapabilityStatus {
            name: "hot-update-store".to_string(),
            supported: true,
            installed: true,
            active: true,
        },
        CapabilityStatus {
            name: "internal-assets".to_string(),
            supported: true,
            installed: true,
            active: true,
        },
        CapabilityStatus {
            name: "optional-assets".to_string(),
            supported: true,
            installed: true,
            active: true,
        },
        CapabilityStatus {
            name: "wallpaper".to_string(),
            supported: true,
            installed: false,
            active: false,
        },
    ];

    for capability in &manifest.capabilities {
        if let Some(existing) = capabilities
            .iter_mut()
            .find(|item| item.name == capability.name)
        {
            *existing = capability.clone();
        } else {
            capabilities.push(capability.clone());
        }
    }
    capabilities
}

#[tauri::command]
pub fn get_control_plane_status(app: AppHandle) -> Result<ControlPlaneStatus, String> {
    let manifest = read_store_manifest(&app);
    let capabilities = default_capabilities(&manifest);
    Ok(ControlPlaneStatus {
        api_version: CONTROL_PLANE_API_VERSION,
        active_release_set_id: manifest.active_release_set_id,
        failed_release_set_id: manifest.failed_release_set_id,
        safe_mode: manifest.safe_mode,
        capabilities,
    })
}

#[tauri::command]
pub fn get_hot_update_store_manifest(app: AppHandle) -> Result<HotUpdateStoreManifest, String> {
    Ok(read_store_manifest(&app))
}

#[tauri::command]
pub fn resolve_hot_update_artifact_path(
    app: AppHandle,
    request: HotUpdateArtifactPathRequest,
) -> Result<String, String> {
    let path = artifact_path(&app, &request)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

#[tauri::command]
pub fn resolve_hot_update_staging_path(
    app: AppHandle,
    request: HotUpdateStagingPathRequest,
) -> Result<String, String> {
    let path = staging_path(&app, &request)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

#[tauri::command]
pub fn get_hot_update_component_state(
    app: AppHandle,
    key: HotUpdateComponentKey,
) -> Result<HotUpdateComponentState, String> {
    Ok(read_component_state(&app, &key))
}

#[tauri::command]
pub fn activate_hot_update_component(
    app: AppHandle,
    request: ActivateHotUpdateComponentRequest,
) -> Result<HotUpdateComponentState, String> {
    with_store_lock(&app, || {
        let mut state = read_component_state(&app, &request.key);
        state.previous_version = state.active_version.clone();
        state.active_version = Some(request.version);
        state.active_file_name = Some(request.file_name);
        state.failed_version = None;
        state.metadata = request.metadata;
        write_component_state(&app, &request.key, &state)?;
        Ok(state)
    })
}

#[tauri::command]
pub fn mark_hot_update_component_failed(
    app: AppHandle,
    request: ActivateHotUpdateComponentRequest,
) -> Result<HotUpdateComponentState, String> {
    with_store_lock(&app, || {
        let mut state = read_component_state(&app, &request.key);
        state.failed_version = Some(request.version);
        write_component_state(&app, &request.key, &state)?;
        Ok(state)
    })
}

#[tauri::command]
pub fn activate_release_set(
    app: AppHandle,
    request: ActivateReleaseSetRequest,
) -> Result<HotUpdateStoreManifest, String> {
    with_store_lock(&app, || {
        safe_segment(&request.release_set_id)?;
        let mut manifest = read_store_manifest(&app);
        manifest.active_release_set_id = Some(request.release_set_id);
        manifest.failed_release_set_id = None;
        manifest.safe_mode = false;
        manifest.components = request.components;
        manifest.capabilities = request.capabilities;
        write_store_manifest(&app, &manifest)?;
        Ok(manifest)
    })
}

#[tauri::command]
pub fn enter_safe_mode(
    app: AppHandle,
    failed_release_set_id: Option<String>,
) -> Result<HotUpdateStoreManifest, String> {
    with_store_lock(&app, || {
        let mut manifest = read_store_manifest(&app);
        manifest.failed_release_set_id =
            failed_release_set_id.or(manifest.active_release_set_id.take());
        manifest.active_release_set_id = None;
        manifest.safe_mode = true;
        write_store_manifest(&app, &manifest)?;
        Ok(manifest)
    })
}

#[tauri::command]
pub fn disable_active_release_set(app: AppHandle) -> Result<HotUpdateStoreManifest, String> {
    with_store_lock(&app, || {
        let mut manifest = read_store_manifest(&app);
        manifest.failed_release_set_id = manifest.active_release_set_id.take();
        manifest.safe_mode = true;
        write_store_manifest(&app, &manifest)?;
        Ok(manifest)
    })
}

#[tauri::command]
pub fn hot_update_asset_url(
    app: AppHandle,
    request: HotUpdateArtifactPathRequest,
) -> Result<String, String> {
    let path = artifact_path(&app, &request)?;
    if !path.is_file() {
        return Err("hot update asset is not installed".to_string());
    }
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| "failed to build hot update asset url".to_string())
}

#[tauri::command]
pub fn activate_optional_asset_pack(
    app: AppHandle,
    request: OptionalAssetPackRequest,
) -> Result<HotUpdateComponentState, String> {
    with_store_lock(&app, || {
        safe_segment(&request.capability)?;
        let key = HotUpdateComponentKey {
            category: "optional-assets".to_string(),
            component: request.component.clone(),
            game: "global".to_string(),
            channel: request.channel.clone(),
            platform: request.platform.clone(),
            arch: request.arch.clone(),
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "capability".to_string(),
            serde_json::Value::String(request.capability.clone()),
        );
        if let Some(release_set_id) = &request.release_set_id {
            metadata.insert(
                "releaseSetId".to_string(),
                serde_json::Value::String(release_set_id.clone()),
            );
        }
        if let Some(schema_version) = &request.schema_version {
            metadata.insert(
                "schemaVersion".to_string(),
                serde_json::Value::String(schema_version.clone()),
            );
        }

        let mut state = read_component_state(&app, &key);
        state.previous_version = state.active_version.clone();
        state.active_version = Some(request.version);
        state.active_file_name = Some(request.file_name);
        state.failed_version = None;
        state.metadata = metadata;
        write_component_state(&app, &key, &state)?;

        let mut manifest = read_store_manifest(&app);
        let capability = CapabilityStatus {
            name: request.capability,
            supported: true,
            installed: true,
            active: true,
        };
        if let Some(existing) = manifest
            .capabilities
            .iter_mut()
            .find(|item| item.name == capability.name)
        {
            *existing = capability;
        } else {
            manifest.capabilities.push(capability);
        }
        write_store_manifest(&app, &manifest)?;
        Ok(state)
    })
}

#[cfg(test)]
mod tests {
    use super::safe_segment;

    #[test]
    fn safe_segment_rejects_path_traversal() {
        assert!(safe_segment("stable").is_ok());
        assert!(safe_segment("../stable").is_err());
        assert!(safe_segment("..").is_err());
        assert!(safe_segment("a/b").is_err());
    }
}
