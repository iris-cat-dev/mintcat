mod ui_assets;
mod window_surface;

use std::path::{Path, PathBuf};

use mintcat_runtime_ipc::{connect_or_start, IpcClient};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use tauri::Manager;
use uuid::Uuid;

// Signed baseline admission and inventory authenticate native package bytes.
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

pub async fn request_running_daemon_command(
    data_dir: impl Into<PathBuf>,
    method: &str,
    params: Value,
    idempotency_key: Option<String>,
) -> Result<JsonRpcResponse, String> {
    let paths = RuntimePaths::new(data_dir).map_err(|error| error.to_string())?;
    let mut client = IpcClient::connect(&paths)
        .await
        .map_err(|error| error.to_string())?;
    request(&mut client, method, params, idempotency_key).await
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
    if std::env::var_os("MINTCAT_RUNTIME_CONFIG").is_none() {
        if configuration.is_file() {
            std::env::set_var("MINTCAT_RUNTIME_CONFIG", &configuration);
        } else if !cfg!(debug_assertions) {
            return Err("Packaged signed baseline runtime configuration is missing".into());
        }
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
    if manifest["schemaVersion"] != 1 || manifest["target"].as_str() != Some(env!("MINTCAT_TARGET"))
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
                // A background workbench must keep its signed UI lease alive and
                // acknowledge update barriers; WebKit's default suspends these timers.
                tauri::WebviewWindowBuilder::from_config(app.handle(), config)?
                    .transparent(cfg!(any(target_os = "macos", windows)))
                    .background_throttling(
                        tauri::utils::config::BackgroundThrottlingPolicy::Disabled,
                    )
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
        .manage(ui_assets::UiAssets::default())
        .register_uri_scheme_protocol("mintcat-ui", |context, request| {
            ui_assets::respond(context.app_handle(), request)
        })
        .invoke_handler(tauri::generate_handler![
            daemon_status,
            daemon_doctor,
            daemon_call,
            desktop_environment,
            rescue_log,
            window_surface::desktop_window_surface,
            ui_assets::ui_open_generation,
            ui_assets::ui_close_generation,
            ui_assets::ui_request_generation
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

    use super::request_running_daemon;

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
