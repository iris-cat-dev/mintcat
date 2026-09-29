use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use anyhow::{Context, Result};
use mintcat_integrator_api::{
    CheckForeignPaksRequest, CheckInstalledRequest, CheckModConflictsRequest, FindGamePakRequest,
    InstallEvent, InstallProgress, InstallRequest, PathRequest, UninstallModsRequest,
};
use mintcat_integrator_core::{
    check_foreign_paks_by_game_path, check_installed_by_game_path, check_mod_conflicts_from_json,
    find_game_pak_by_name, install_mods_with_progress, is_valid_mod_directory, is_valid_zip,
    uninstall_mods_by_game_path,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod nexus_webview;
mod proxy_runtime;

const ABI_VERSION: u32 = 1;
const BACKEND_RUNTIME_ABI_VERSION: u32 = 1;

type MintCatProgressCallback =
    unsafe extern "C" fn(event_json: *const c_char, user_data: *mut c_void);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendInvokeEnvelope {
    command: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchSteamGameRequest {
    steam_app_id: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckSteamGameRequest {
    exe_name: String,
}

struct FfiProgress {
    callback: Option<MintCatProgressCallback>,
    user_data: *mut c_void,
}

unsafe impl Send for FfiProgress {}
unsafe impl Sync for FfiProgress {}

impl InstallProgress for FfiProgress {
    fn emit(&self, event: InstallEvent) -> Result<()> {
        let Some(callback) = self.callback else {
            return Ok(());
        };
        let json = serde_json::to_string(&event).context("failed to serialize progress event")?;
        let event_json = string_to_c(json);
        unsafe {
            callback(event_json.as_ptr(), self.user_data);
        }
        Ok::<(), anyhow::Error>(())
    }
}

#[no_mangle]
pub extern "C" fn mintcat_integrator_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn mintcat_backend_runtime_abi_version() -> u32 {
    BACKEND_RUNTIME_ABI_VERSION
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_backend_runtime_manifest(
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    clear_out(response_json);
    clear_out(error_message);
    match catch_unwind(AssertUnwindSafe(|| {
        let manifest = serde_json::json!({
            "name": "mintcat-backend-runtime",
            "abiVersion": BACKEND_RUNTIME_ABI_VERSION,
            "minControlPlaneVersion": 1,
            "commands": [
                {"name": "install_mods", "mode": "task"},
                {"name": "uninstall_mods", "mode": "requestResponse"},
                {"name": "check_installed", "mode": "requestResponse"},
                {"name": "find_game_pak", "mode": "requestResponse"},
                {"name": "check_foreign_paks_in_paks_dir", "mode": "requestResponse"},
                {"name": "is_valid_unpacked_mod", "mode": "requestResponse"},
                {"name": "check_mod_conflicts", "mode": "requestResponse"},
                {"name": "validate_zip_file", "mode": "requestResponse"},
                {"name": "launch_steam_game", "mode": "requestResponse"},
                {"name": "check_steam_game", "mode": "requestResponse"},
                {"name": "install_proxy_runtime_from_manifest", "mode": "requestResponse"},
                {"name": "get_proxy_runtime_status", "mode": "requestResponse"},
                {"name": "start_proxy_runtime", "mode": "requestResponse"},
                {"name": "stop_proxy_runtime", "mode": "requestResponse"},
                {"name": "install_proxy_cert", "mode": "requestResponse"},
                {"name": "open_nexus_download_webview", "mode": "requestResponse"},
                {"name": "nexus_download_webview_decide", "mode": "requestResponse"}
            ],
            "capabilities": ["integrator", "game-business"]
        });
        set_out(
            response_json,
            serde_json::to_string(&manifest).context("failed to serialize backend manifest")?,
        );
        Ok::<(), anyhow::Error>(())
    })) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            set_out(error_message, format!("{error:#}"));
            1
        }
        Err(_) => {
            set_out(error_message, "backend runtime manifest panic".to_string());
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_backend_runtime_invoke(
    request_json: *const c_char,
    callback: Option<MintCatProgressCallback>,
    user_data: *mut c_void,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    clear_out(response_json);
    clear_out(error_message);

    match catch_unwind(AssertUnwindSafe(|| {
        if request_json.is_null() {
            anyhow::bail!("request_json is null");
        }
        let request = CStr::from_ptr(request_json)
            .to_str()
            .context("request_json is not valid UTF-8")?;
        let envelope: BackendInvokeEnvelope =
            serde_json::from_str(request).context("invalid backend invoke JSON")?;
        let response = backend_invoke(envelope, callback, user_data)?;
        set_out(response_json, response);
        Ok(())
    })) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            set_out(error_message, format!("{error:#}"));
            1
        }
        Err(_) => {
            set_out(error_message, "backend runtime invoke panic".to_string());
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_install(
    request_json: *const c_char,
    callback: Option<MintCatProgressCallback>,
    user_data: *mut c_void,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    clear_out(response_json);
    clear_out(error_message);

    match catch_unwind(AssertUnwindSafe(|| {
        install_inner(request_json, callback, user_data, response_json)
    })) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            set_out(error_message, format!("{error:#}"));
            1
        }
        Err(_) => {
            set_out(error_message, "integrator panic".to_string());
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_uninstall_mods(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: UninstallModsRequest| {
            uninstall_mods_by_game_path(request.game_path, request.is_delete_ue4ss)
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_check_installed(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: CheckInstalledRequest| {
            check_installed_by_game_path(request.game_path, request.install_time)
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_find_game_pak(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: FindGamePakRequest| Ok(find_game_pak_by_name(request.game_name.as_deref())),
    )
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_check_foreign_paks(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: CheckForeignPaksRequest| check_foreign_paks_by_game_path(&request.game_path),
    )
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_is_valid_unpacked_mod(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: PathRequest| Ok(is_valid_mod_directory(&request.path)),
    )
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_check_mod_conflicts(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: CheckModConflictsRequest| {
            check_mod_conflicts_from_json(&request.mod_list_json, request.game_name.as_deref())
                .map_err(anyhow::Error::msg)
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn mintcat_integrator_validate_zip_file(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
) -> i32 {
    command_entry(
        request_json,
        response_json,
        error_message,
        |request: PathRequest| Ok(is_valid_zip(&request.path)),
    )
}

unsafe fn install_inner(
    request_json: *const c_char,
    callback: Option<MintCatProgressCallback>,
    user_data: *mut c_void,
    response_json: *mut *mut c_char,
) -> Result<()> {
    if request_json.is_null() {
        anyhow::bail!("request_json is null");
    }

    let request = CStr::from_ptr(request_json)
        .to_str()
        .context("request_json is not valid UTF-8")?;
    let request: InstallRequest =
        serde_json::from_str(request).context("invalid install request JSON")?;
    let progress = FfiProgress {
        callback,
        user_data,
    };

    install_mods_with_progress(&progress, request)?;
    set_out(response_json, r#"{"ok":true}"#.to_string());
    Ok(())
}

unsafe fn backend_invoke(
    envelope: BackendInvokeEnvelope,
    callback: Option<MintCatProgressCallback>,
    user_data: *mut c_void,
) -> Result<String> {
    match envelope.command.as_str() {
        "install_mods" => {
            let request: InstallRequest =
                serde_json::from_value(envelope.payload).context("invalid install_mods payload")?;
            let progress = FfiProgress {
                callback,
                user_data,
            };
            install_mods_with_progress(&progress, request)?;
            Ok(r#"{"ok":true}"#.to_string())
        }
        "uninstall_mods" => {
            backend_command_response(envelope.payload, |request: UninstallModsRequest| {
                uninstall_mods_by_game_path(request.game_path, request.is_delete_ue4ss)
            })
        }
        "check_installed" => {
            backend_command_response(envelope.payload, |request: CheckInstalledRequest| {
                check_installed_by_game_path(request.game_path, request.install_time)
            })
        }
        "find_game_pak" => {
            backend_command_response(envelope.payload, |request: FindGamePakRequest| {
                Ok(find_game_pak_by_name(request.game_name.as_deref()))
            })
        }
        "check_foreign_paks_in_paks_dir" | "check_foreign_paks" => {
            backend_command_response(envelope.payload, |request: CheckForeignPaksRequest| {
                check_foreign_paks_by_game_path(&request.game_path)
            })
        }
        "is_valid_unpacked_mod" => {
            backend_command_response(envelope.payload, |request: PathRequest| {
                Ok(is_valid_mod_directory(&request.path))
            })
        }
        "check_mod_conflicts" => {
            backend_command_response(envelope.payload, |request: CheckModConflictsRequest| {
                check_mod_conflicts_from_json(&request.mod_list_json, request.game_name.as_deref())
                    .map_err(anyhow::Error::msg)
            })
        }
        "validate_zip_file" => {
            backend_command_response(envelope.payload, |request: PathRequest| {
                Ok(is_valid_zip(&request.path))
            })
        }
        "launch_steam_game" => {
            backend_command_response(envelope.payload, |request: LaunchSteamGameRequest| {
                launch_steam_game(request.steam_app_id)
            })
        }
        "check_steam_game" => {
            backend_command_response(envelope.payload, |request: CheckSteamGameRequest| {
                Ok(check_steam_game(request.exe_name))
            })
        }
        "install_proxy_runtime_from_manifest"
        | "get_proxy_runtime_status"
        | "start_proxy_runtime"
        | "stop_proxy_runtime"
        | "install_proxy_cert" => serde_json::to_string(&proxy_runtime::invoke(
            envelope.command.as_str(),
            envelope.payload,
        )?)
        .context("failed to serialize proxy runtime response"),
        "open_nexus_download_webview" => {
            serde_json::to_string(&nexus_webview::open_request(envelope.payload)?)
                .context("failed to serialize nexus webview request")
        }
        "nexus_download_webview_decide" => {
            serde_json::to_string(&nexus_webview::decide(envelope.payload)?)
                .context("failed to serialize nexus webview decision")
        }
        command => anyhow::bail!("unknown backend runtime command: {command}"),
    }
}

fn check_steam_game(exe_name: String) -> bool {
    #[cfg(windows)]
    {
        let Ok(output) = std::process::Command::new("tasklist")
            .args(["/FI", format!("IMAGENAME eq {exe_name}").as_str()])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout).contains(exe_name.as_str())
    }
    #[cfg(not(windows))]
    {
        let _ = exe_name;
        false
    }
}

fn launch_steam_game(steam_app_id: u32) -> Result<bool> {
    #[cfg(target_os = "windows")]
    {
        let url = format!("steam://run/{steam_app_id}");
        let status = std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .status()
            .context("failed to launch Steam game")?;
        Ok(status.success())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = steam_app_id;
        Ok(false)
    }
}

fn backend_command_response<T, R, F>(payload: Value, handler: F) -> Result<String>
where
    T: DeserializeOwned,
    R: Serialize,
    F: FnOnce(T) -> Result<R>,
{
    let request: T = serde_json::from_value(payload).context("invalid backend command payload")?;
    let response = handler(request)?;
    serde_json::to_string(&response).context("failed to serialize backend command response")
}

unsafe fn command_entry<T, R, F>(
    request_json: *const c_char,
    response_json: *mut *mut c_char,
    error_message: *mut *mut c_char,
    handler: F,
) -> i32
where
    T: DeserializeOwned,
    R: Serialize,
    F: FnOnce(T) -> Result<R>,
{
    clear_out(response_json);
    clear_out(error_message);

    match catch_unwind(AssertUnwindSafe(|| {
        if request_json.is_null() {
            anyhow::bail!("request_json is null");
        }
        let request = CStr::from_ptr(request_json)
            .to_str()
            .context("request_json is not valid UTF-8")?;
        let request: T = serde_json::from_str(request).context("invalid request JSON")?;
        let response = handler(request)?;
        let response = serde_json::to_string(&response).context("failed to serialize response")?;
        set_out(response_json, response);
        Ok(())
    })) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            set_out(error_message, format!("{error:#}"));
            1
        }
        Err(_) => {
            set_out(error_message, "integrator panic".to_string());
            2
        }
    }
}

unsafe fn clear_out(target: *mut *mut c_char) {
    if !target.is_null() {
        *target = ptr::null_mut();
    }
}

unsafe fn set_out(target: *mut *mut c_char, value: String) {
    if !target.is_null() {
        *target = string_to_c(value).into_raw();
    }
}

fn string_to_c(value: String) -> CString {
    CString::new(value).unwrap_or_else(|error| {
        let bytes = error
            .into_vec()
            .into_iter()
            .filter(|byte| *byte != 0)
            .collect::<Vec<_>>();
        CString::new(bytes).expect("nul bytes were removed")
    })
}
