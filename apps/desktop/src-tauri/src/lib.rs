mod window_surface;

use std::path::{Path, PathBuf};

use mintcat_runtime_ipc::{connect_or_start, IpcClient};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use tauri::Manager;
use uuid::Uuid;

// Allow the same bounded cold-start budget exercised by the standalone CLI.
const APPLICATION_REQUEST_DEADLINE_MS: u64 = 120_000;

pub async fn request_running_daemon(
    data_dir: impl Into<PathBuf>,
    method: &str,
) -> Result<JsonRpcResponse, String> {
    let paths = RuntimePaths::new(data_dir).map_err(|error| error.to_string())?;
    let mut client = IpcClient::connect(&paths)
        .await
        .map_err(|error| error.to_string())?;
    request(&mut client, method, json!({}), None).await
}


pub async fn request_daemon_on_demand(
    data_dir: impl Into<PathBuf>,
    daemon_bin: &Path,
    method: &str,
) -> Result<JsonRpcResponse, String> {
    let paths = RuntimePaths::new(data_dir).map_err(|error| error.to_string())?;
    let mut client = connect_or_start(&paths, daemon_bin, desktop_runtime_environment())
        .await
        .map_err(|error| error.to_string())?;
    request(&mut client, method, json!({}), None).await
}

pub fn sibling_daemon_executable() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let name = if cfg!(windows) {
        "mintcatd.exe"
    } else {
        "mintcatd"
    };
    executable
        .parent()
        .map(|parent| parent.join(name))
        .ok_or_else(|| "desktop executable has no parent directory".to_owned())
}

async fn request(
    client: &mut IpcClient,
    method: &str,
    params: Value,
    idempotency_key: Option<String>,
) -> Result<JsonRpcResponse, String> {
    client
        .request(&JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: JsonRpcId::String(Uuid::new_v4().to_string()),
            method: method.to_owned(),
            contract_version: 1,
            session_generation: client.generation_id.clone(),
            deadline_ms: APPLICATION_REQUEST_DEADLINE_MS,
            idempotency_key,
            params,
        })
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn daemon_status(data_dir: String) -> Result<JsonRpcResponse, String> {
    request_running_daemon(data_dir, "system.status").await
}

#[tauri::command]
async fn daemon_doctor(data_dir: String) -> Result<JsonRpcResponse, String> {
    let daemon_bin = sibling_daemon_executable()?;
    request_daemon_on_demand(data_dir, &daemon_bin, "system.doctor").await
}

#[tauri::command]
async fn daemon_call(
    data_dir: String,
    method: String,
    params: Value,
    idempotency_key: Option<String>,
) -> Result<JsonRpcResponse, String> {
    let daemon_bin = sibling_daemon_executable()?;
    let paths = RuntimePaths::new(data_dir).map_err(|error| error.to_string())?;
    let mut client = connect_or_start(&paths, &daemon_bin, desktop_runtime_environment())
        .await
        .map_err(|error| error.to_string())?;
    request(&mut client, &method, params, idempotency_key).await
}

fn desktop_runtime_environment() -> RuntimeEnvironment {
    if cfg!(debug_assertions) {
        RuntimeEnvironment::Development
    } else {
        RuntimeEnvironment::Production
    }
}

struct PackagedRuntime {
    portable_data_dir: Option<PathBuf>,
}

fn configure_packaged_runtime(app: &tauri::AppHandle) -> Result<PackagedRuntime, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let sibling = executable
        .parent()
        .ok_or("desktop executable has no parent")?;
    let resources = app
        .path()
        .resource_dir()
        .map_err(|error| error.to_string())?;
    let base = if sibling.join("runtime.json").is_file() {
        sibling.to_path_buf()
    } else {
        resources
    };
    let configuration = base.join("runtime.json");
    if std::env::var_os("MINTCAT_RUNTIME_CONFIG").is_none() && configuration.is_file() {
        std::env::set_var("MINTCAT_RUNTIME_CONFIG", &configuration);
    }
    let manifest = base.join("release-manifest.json");
    if !manifest.is_file() {
        if cfg!(debug_assertions) {
            return Ok(PackagedRuntime {
                portable_data_dir: None,
            });
        }
        return Err("Packaged immutable release manifest is missing".into());
    }
    let manifest: Value =
        mintcat_runtime_release::read_json(&manifest).map_err(|error| error.to_string())?;
    if manifest["schemaVersion"] != 2 || manifest["target"].as_str() != Some(env!("MINTCAT_TARGET"))
    {
        return Err(
            "Packaged runtime manifest differs from this desktop's exact compiled target".into(),
        );
    }
    let variant = manifest["variant"]
        .as_str()
        .ok_or("Packaged release variant is missing")?;
    if !["standard", "portable", "fixed-webview2"].contains(&variant) {
        return Err("Invalid packaged desktop release variant".into());
    }
    if variant == "fixed-webview2" {
        if !cfg!(windows) {
            return Err("Fixed WebView2 is a Windows-only release".into());
        }
        let runtime = base.join("webview2");
        if !runtime.join("msedgewebview2.exe").is_file() {
            return Err("Authenticated fixed WebView2 runtime is missing; system runtime fallback is forbidden".into());
        }
        // This executes before any configured window is created.
        std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", runtime);
    }
    let portable_data_dir = if variant == "portable" {
        if !cfg!(windows) || manifest["dataDirectory"]["mode"] != "portable" {
            return Err("Invalid portable data directory declaration".into());
        }
        let path = PathBuf::from(
            manifest["dataDirectory"]["path"]
                .as_str()
                .ok_or("Portable release lacks its declared data directory")?,
        );
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(
                "Portable data directory must be a relative child without traversal".into(),
            );
        }
        Some(sibling.join(path))
    } else {
        None
    };
    Ok(PackagedRuntime { portable_data_dir })
}

fn explicit_data_directory() -> Result<Option<PathBuf>, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut explicit = None;
    while let Some(argument) = arguments.next() {
        if argument == "--data-dir" {
            explicit = Some(PathBuf::from(
                arguments.next().ok_or("--data-dir requires a directory")?,
            ));
        } else if let Some(path) = argument
            .to_str()
            .and_then(|argument| argument.strip_prefix("--data-dir="))
        {
            explicit = Some(PathBuf::from(path));
        }
    }
    let directory = explicit
        .or_else(|| std::env::var_os(mintcat_runtime_kernel::DATA_DIR_ENV).map(PathBuf::from));
    if directory
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err("Explicit data directory must not be empty".into());
    }
    Ok(directory)
}

#[tauri::command]
fn desktop_environment(app: tauri::AppHandle) -> Result<Value, String> {
    let data_dir = if let Some(explicit) = explicit_data_directory()? {
        explicit
    } else if let Some(directory) = app.state::<PackagedRuntime>().portable_data_dir.as_ref() {
        directory.clone()
    } else {
        let base = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        if cfg!(debug_assertions) {
            base.join("isolated-vnext-development")
        } else {
            base.join("vnext")
        }
    };
    Ok(json!({"dataDir":data_dir,"environment":desktop_runtime_environment()}))
}

#[tauri::command]
fn rescue_log(data_dir: String) -> Result<Value, String> {
    use std::io::{Read, Seek, SeekFrom};
    let path = RuntimePaths::new(data_dir)
        .map_err(|error| error.to_string())?
        .layout()
        .runtime_dir
        .join("daemon.log");
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Daemon log is not a regular file".into());
    }
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    let count = length.min(64 * 1024);
    file.seek(SeekFrom::Start(length - count))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(count as usize);
    file.take(count)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(json!({"text":String::from_utf8_lossy(&bytes),"truncated":length > count}))
}

fn validate_external_https_url(raw: &str) -> Result<tauri::Url, String> {
    if raw.is_empty() || raw.len() > 2048 || raw.chars().any(char::is_control) {
        return Err("External URL must be a valid HTTPS web address".to_owned());
    }
    let parsed = tauri::Url::parse(raw)
        .map_err(|_| "External URL must be a valid HTTPS web address".to_owned())?;
    if parsed.scheme() != "https" || parsed.cannot_be_a_base() {
        return Err("Only HTTPS external URLs may be opened".to_owned());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("External URL must include a host".to_owned());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("External URL must not contain embedded credentials".to_owned());
    }
    Ok(parsed)
}

#[cfg(target_os = "macos")]
fn launch_external_url(url: &tauri::Url) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    let raw = NSString::from_str(url.as_str());
    let ns_url = NSURL::URLWithString(&raw).ok_or_else(|| "Invalid external URL".to_owned())?;
    if NSWorkspace::sharedWorkspace().openURL(&ns_url) {
        Ok(())
    } else {
        Err("Unable to open external URL in the default browser".to_owned())
    }
}

#[cfg(windows)]
fn launch_external_url(url: &tauri::Url) -> Result<(), String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb = HSTRING::from("open");
    let target = HSTRING::from(url.as_str());
    let instance = unsafe {
        ShellExecuteW(
            None,
            &verb,
            &target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (instance.0 as isize) > 32 {
        Ok(())
    } else {
        Err(format!(
            "Unable to open external URL in the default browser (status {})",
            instance.0 as isize
        ))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn launch_external_url(url: &tauri::Url) -> Result<(), String> {
    use std::process::{Command, Stdio};

    Command::new("xdg-open")
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Unable to open external URL: {error}"))
}

#[tauri::command]
async fn open_external_url(window: tauri::WebviewWindow, url: String) -> Result<(), String> {
    let parsed = validate_external_https_url(&url)?;
    #[cfg(target_os = "macos")]
    {
        let (sender, mut receiver) = tauri::async_runtime::channel(1);
        window
            .run_on_main_thread(move || {
                let _ = sender.try_send(launch_external_url(&parsed));
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .await
            .ok_or_else(|| "External URL launcher callback was released".to_owned())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        launch_external_url(&parsed)
    }
}

fn validate_steam_app_id(app_id: &str) -> Result<tauri::Url, String> {
    let trimmed = app_id.trim();
    if trimmed.is_empty()
        || trimmed.len() > 12
        || !trimmed.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Steam App ID must be a positive numeric identifier".to_owned());
    }
    tauri::Url::parse(&format!("steam://rungameid/{trimmed}"))
        .map_err(|_| "Unable to construct Steam launch URI".to_owned())
}

#[tauri::command]
async fn launch_steam_game(window: tauri::WebviewWindow, app_id: String) -> Result<(), String> {
    let parsed = validate_steam_app_id(&app_id)?;
    #[cfg(target_os = "macos")]
    {
        let (sender, mut receiver) = tauri::async_runtime::channel(1);
        window
            .run_on_main_thread(move || {
                let _ = sender.try_send(launch_external_url(&parsed));
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .await
            .ok_or_else(|| "Game launcher callback was released".to_owned())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        launch_external_url(&parsed)
    }
}

#[cfg(target_os = "macos")]
fn pick_local_path_native(
    kind: &str,
    title: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString};

    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "Native file picker must run on the main thread".to_owned())?;
    let panel = NSOpenPanel::openPanel(mtm);
    let choose_directory = kind == "directory";
    panel.setCanChooseFiles(!choose_directory);
    panel.setCanChooseDirectories(choose_directory);
    panel.setAllowsMultipleSelection(false);
    panel.setResolvesAliases(true);
    if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
        panel.setTitle(Some(&NSString::from_str(&title)));
    }
    if !choose_directory {
        if let Some(exts) = extensions.filter(|items| !items.is_empty()) {
            let ns_exts: Vec<_> = exts
                .iter()
                .map(|ext| NSString::from_str(ext.trim_start_matches('.')))
                .collect();
            let array = NSArray::from_retained_slice(&ns_exts);
            #[allow(deprecated)]
            panel.setAllowedFileTypes(Some(&array));
        }
    }
    let response = panel.runModal();
    if response != NSModalResponseOK {
        return Ok(None);
    }
    let Some(url) = panel.URL() else {
        return Ok(None);
    };
    Ok(url.path().map(|path| path.to_string()))
}

#[cfg(target_os = "macos")]
fn pick_save_path_native(
    title: Option<String>,
    default_name: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSSavePanel};
    use objc2_foundation::{NSArray, NSString};

    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "Native save picker must run on the main thread".to_owned())?;
    let panel = NSSavePanel::savePanel(mtm);
    panel.setCanCreateDirectories(true);
    if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
        panel.setTitle(Some(&NSString::from_str(&title)));
    }
    if let Some(name) = default_name.filter(|value| !value.trim().is_empty()) {
        panel.setNameFieldStringValue(&NSString::from_str(&name));
    }
    if let Some(exts) = extensions.filter(|items| !items.is_empty()) {
        let ns_exts: Vec<_> = exts
            .iter()
            .map(|ext| NSString::from_str(ext.trim_start_matches('.')))
            .collect();
        let array = NSArray::from_retained_slice(&ns_exts);
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&array));
    }
    let response = panel.runModal();
    if response != NSModalResponseOK {
        return Ok(None);
    }
    let Some(url) = panel.URL() else {
        return Ok(None);
    };
    Ok(url.path().map(|path| path.to_string()))
}

#[cfg(windows)]
fn pick_local_path_native(
    kind: &str,
    title: Option<String>,
    _extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> Result<Option<String>, String> {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let mut options = dialog.GetOptions().map_err(|e| e.to_string())?;
            options |= FOS_FORCEFILESYSTEM;
            if kind == "directory" {
                options |= FOS_PICKFOLDERS;
            }
            dialog.SetOptions(options).map_err(|e| e.to_string())?;
            if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
                let htitle = HSTRING::from(title);
                dialog.SetTitle(&htitle).map_err(|e| e.to_string())?;
            }
            if dialog.Show(None).is_err() {
                return Ok(None);
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let path = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|e| e.to_string())?;
            let text = path.to_string().map_err(|e| e.to_string())?;
            windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as _));
            Ok(Some(text))
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(windows)]
fn pick_save_path_native(
    title: Option<String>,
    default_name: Option<String>,
    _extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileSaveDialog, IFileSaveDialog, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
        SIGDN_FILESYSPATH,
    };

    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> Result<Option<String>, String> {
            let dialog: IFileSaveDialog =
                CoCreateInstance(&FileSaveDialog, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let mut options = dialog.GetOptions().map_err(|e| e.to_string())?;
            options |= FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT;
            dialog.SetOptions(options).map_err(|e| e.to_string())?;
            if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
                let htitle = HSTRING::from(title);
                dialog.SetTitle(&htitle).map_err(|e| e.to_string())?;
            }
            if let Some(name) = default_name.filter(|value| !value.trim().is_empty()) {
                let hname = HSTRING::from(name);
                dialog.SetFileName(&hname).map_err(|e| e.to_string())?;
            }
            if dialog.Show(None).is_err() {
                return Ok(None);
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let path = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|e| e.to_string())?;
            let text = path.to_string().map_err(|e| e.to_string())?;
            windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as _));
            Ok(Some(text))
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn pick_local_path_native(
    _kind: &str,
    _title: Option<String>,
    _extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    Err("Native path picker is not supported on this platform".to_owned())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn pick_save_path_native(
    _title: Option<String>,
    _default_name: Option<String>,
    _extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    Err("Native save picker is not supported on this platform".to_owned())
}

#[tauri::command]
async fn open_data_dir(window: tauri::WebviewWindow, data_dir: String) -> Result<(), String> {
    let path = RuntimePaths::new(data_dir)
        .map_err(|error| error.to_string())?
        .layout()
        .data_dir;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("Data directory is not available: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Data directory must be a real directory".to_owned());
    }
    let file_url = tauri::Url::from_directory_path(&path)
        .map_err(|_| "Unable to format data directory path".to_owned())?;
    #[cfg(target_os = "macos")]
    {
        let (sender, mut receiver) = tauri::async_runtime::channel(1);
        window
            .run_on_main_thread(move || {
                let _ = sender.try_send(launch_external_url(&file_url));
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .await
            .ok_or_else(|| "Data directory opener callback was released".to_owned())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        launch_external_url(&file_url)
    }
}

#[tauri::command]
async fn pick_local_path(
    window: tauri::WebviewWindow,
    kind: String,
    title: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    if !matches!(kind.as_str(), "file" | "directory") {
        return Err("Picker kind must be 'file' or 'directory'".to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        let (sender, mut receiver) = tauri::async_runtime::channel(1);
        window
            .run_on_main_thread(move || {
                let _ = sender.try_send(pick_local_path_native(&kind, title, extensions));
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .await
            .ok_or_else(|| "Native path picker callback was released".to_owned())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        tauri::async_runtime::spawn_blocking(move || {
            pick_local_path_native(&kind, title, extensions)
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

#[tauri::command]
async fn pick_save_path(
    window: tauri::WebviewWindow,
    title: Option<String>,
    default_name: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        let (sender, mut receiver) = tauri::async_runtime::channel(1);
        window
            .run_on_main_thread(move || {
                let _ = sender.try_send(pick_save_path_native(title, default_name, extensions));
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv()
            .await
            .ok_or_else(|| "Native save picker callback was released".to_owned())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        tauri::async_runtime::spawn_blocking(move || {
            pick_save_path_native(title, default_name, extensions)
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

pub fn run() {
    let context = tauri::generate_context!();
    if std::env::args_os().any(|argument| argument == "--verify-embedded-ui") {
        if context
            .assets()
            .get(&tauri_utils::assets::AssetKey::from("index.html"))
            .is_none()
        {
            eprintln!("OFFLINE_UI_NOT_EMBEDDED: build the desktop with --features custom-protocol");
            std::process::exit(2);
        }
        println!("{}", json!({"embeddedUi":true,"entry":"index.html"}));
        return;
    }
    let app = tauri::Builder::default()
        .setup(|app| {
            let runtime =
                configure_packaged_runtime(app.handle()).map_err(std::io::Error::other)?;
            app.manage(runtime);
            for config in app.config().app.windows.iter() {
                // Keep task observations responsive while the workbench is in the background.
                tauri::WebviewWindowBuilder::from_config(app.handle(), config)?
                    .transparent(cfg!(any(target_os = "macos", windows)))
                    .background_throttling(
                        tauri::utils::config::BackgroundThrottlingPolicy::Disabled,
                    )
                    .on_new_window(|url, _features| {
                        if let Ok(validated) = validate_external_https_url(url.as_str()) {
                            let _ = launch_external_url(&validated);
                        }
                        tauri::webview::NewWindowResponse::Deny
                    })
                    .build()?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                window_surface::prepare_for_destruction(window.label());
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (window, event);
        })
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            daemon_doctor,
            daemon_call,
            desktop_environment,
            rescue_log,
            open_external_url,
            open_data_dir,
            launch_steam_game,
            pick_local_path,
            pick_save_path,
            window_surface::desktop_window_surface
        ])
        .build(context)
        .expect("vNext desktop host failed");
    app.run(|_app, event| {
        #[cfg(target_os = "macos")]
        if matches!(event, tauri::RunEvent::Exit) {
            window_surface::prepare_for_termination();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = event;
    });
}

#[cfg(test)]
mod tests {
    use mintcat_daemon::BoundDaemon;
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::{request_running_daemon, validate_external_https_url, validate_steam_app_id};

    #[test]
    fn steam_app_id_validation_allows_only_digits() {
        for valid in ["548430", "2605790", "2860770"] {
            assert_eq!(
                validate_steam_app_id(valid).unwrap().as_str(),
                format!("steam://rungameid/{valid}")
            );
        }
        for rejected in ["", "drg", "548430/../../etc", "548430?a=1", "1234567890123"] {
            assert!(
                validate_steam_app_id(rejected).is_err(),
                "expected rejection for {rejected:?}"
            );
        }
    }

    #[test]
    fn external_url_validation_allows_only_credential_free_https_urls() {
        for valid in [
            "https://mod.io/me/access",
            "https://mod.io/me/access#tokens",
            "https://www.nexusmods.com/users/myaccount?tab=api%20access",
            "https://modcat.top",
        ] {
            assert_eq!(validate_external_https_url(valid).unwrap().scheme(), "https");
        }
        for rejected in [
            "",
            "http://mod.io/me/access",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:secret@mod.io/me/access",
            "https://mod.io/me/access\nmalicious",
            "/relative/path",
        ] {
            assert!(
                validate_external_https_url(rejected).is_err(),
                "expected rejection for {rejected:?}"
            );
        }
    }

    #[tokio::test]
    async fn desktop_connector_uses_the_same_authenticated_daemon() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        let daemon = BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let generation = daemon.ready().generation_id;
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(daemon.run(async {
            let _ = receiver.await;
        }));

        let response = request_running_daemon(paths.data_dir(), "system.status")
            .await
            .unwrap();

        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["generationId"], generation);
        shutdown.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
}
