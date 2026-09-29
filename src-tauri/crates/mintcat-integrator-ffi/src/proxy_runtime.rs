use std::{
    fs,
    io::{Cursor, Read, Seek},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BUNDLED_RUNTIME_VERSION: &str = "bundled";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyHostContext {
    pub root_dir: String,
    pub app_version: String,
    #[serde(default)]
    pub bundled_runtime_path: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRuntimeManifest {
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartProxyRuntimeOptions {
    #[serde(default = "default_proxy_port")]
    pub port: u16,
    #[serde(default)]
    pub offline: bool,
    #[serde(default = "default_bind")]
    pub bind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyCommandEnvelope<T> {
    context: ProxyHostContext,
    #[serde(flatten)]
    data: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProxyRuntimeState {
    #[serde(default)]
    active_version: Option<String>,
    #[serde(default)]
    previous_version: Option<String>,
    #[serde(default)]
    last_failed_version: Option<String>,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    last_pid: Option<u32>,
    #[serde(default)]
    started_at: Option<u64>,
    #[serde(default)]
    cert_installed: bool,
    #[serde(default)]
    needs_hosts_cleanup: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRuntimeStatus {
    pub active_version: Option<String>,
    pub previous_version: Option<String>,
    pub last_failed_version: Option<String>,
    pub installed: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub has_cert_installed: bool,
    pub log_path: Option<String>,
}

fn default_proxy_port() -> u16 {
    443
}

fn default_bind() -> String {
    "0.0.0.0".to_string()
}

pub fn invoke(command: &str, payload: Value) -> Result<Value> {
    match command {
        "install_proxy_runtime_from_manifest" => {
            let envelope: ProxyCommandEnvelope<ProxyRuntimeManifest> =
                serde_json::from_value(payload)?;
            serde_json::to_value(install_from_manifest(&envelope.context, envelope.data)?)
                .map_err(Into::into)
        }
        "get_proxy_runtime_status" => {
            let envelope: ProxyCommandEnvelope<Value> = serde_json::from_value(payload)?;
            serde_json::to_value(status(&envelope.context)).map_err(Into::into)
        }
        "start_proxy_runtime" => {
            let envelope: ProxyCommandEnvelope<StartProxyRuntimeOptions> =
                serde_json::from_value(payload)?;
            serde_json::to_value(start(&envelope.context, envelope.data)?).map_err(Into::into)
        }
        "stop_proxy_runtime" => {
            let envelope: ProxyCommandEnvelope<Value> = serde_json::from_value(payload)?;
            serde_json::to_value(stop(&envelope.context)?).map_err(Into::into)
        }
        "install_proxy_cert" => {
            let envelope: ProxyCommandEnvelope<Value> = serde_json::from_value(payload)?;
            serde_json::to_value(install_cert(&envelope.context)?).map_err(Into::into)
        }
        _ => anyhow::bail!("unknown proxy runtime command: {command}"),
    }
}

fn install_from_manifest(
    context: &ProxyHostContext,
    manifest: ProxyRuntimeManifest,
) -> Result<ProxyRuntimeStatus> {
    valid_version(&manifest.version)?;
    validate_compatibility(context, &manifest)?;
    verify_signature(&manifest)?;
    let expected = manifest
        .md5
        .as_deref()
        .context("proxy runtime md5 is required")?;
    let bytes = download(context, &manifest.url)?;
    let actual = format!("{:x}", md5::compute(&bytes));
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        anyhow::bail!("md5 mismatch: expected {expected}, got {actual}");
    }

    let version_dir = versions_dir(context).join(&manifest.version);
    fs::create_dir_all(&version_dir)?;
    let runtime_path = version_dir.join(runtime_file_name());
    install_runtime_payload(&bytes, &runtime_path)?;
    make_executable(&runtime_path)?;

    let mut state = read_state(context);
    state.previous_version = state.active_version.clone();
    state.active_version = Some(manifest.version);
    state.last_failed_version = None;
    write_state(context, &state)?;
    cleanup_old_versions(context, &state);
    Ok(status(context))
}

fn start(
    context: &ProxyHostContext,
    options: StartProxyRuntimeOptions,
) -> Result<ProxyRuntimeStatus> {
    let current = status(context);
    if current.running {
        return Ok(current);
    }
    let runtime = active_runtime_path(context).context("proxy runtime is not installed")?;
    fs::create_dir_all(root(context))?;
    let log_path = runtime_log_path(context);
    let _ = fs::write(&log_path, "");
    let should_install_cert = should_install_cert_on_start(context);

    let mut args = common_proxy_args(context)?;
    args.extend([
        "start".to_string(),
        "--port".to_string(),
        options.port.to_string(),
        "--bind".to_string(),
        options.bind,
        "--log-file".to_string(),
        log_path.to_string_lossy().to_string(),
    ]);
    if options.offline {
        args.push("--offline".to_string());
    }
    if should_install_cert {
        args.push("--install-cert".to_string());
    }

    let pid = parse_pid(&run_elevated(&runtime, &args, false)?)
        .context("failed to get proxy process id")?;
    let mut state = read_state(context);
    state.running = true;
    state.last_pid = Some(pid);
    state.started_at = Some(now_unix_seconds());
    state.needs_hosts_cleanup = true;
    if should_install_cert {
        state.cert_installed = true;
    }
    write_state(context, &state)?;
    Ok(status(context))
}

fn stop(context: &ProxyHostContext) -> Result<ProxyRuntimeStatus> {
    let state = read_state(context);
    let pid = running_pid(&state);
    if state.running || state.needs_hosts_cleanup {
        stop_proxy(context, pid)?;
    } else if let Some(pid) = pid {
        terminate_proxy_process(context, pid)?;
    }
    mark_stopped(context)?;
    Ok(status(context))
}

fn install_cert(context: &ProxyHostContext) -> Result<ProxyRuntimeStatus> {
    let runtime = active_runtime_path(context).context("proxy runtime is not installed")?;
    let mut args = common_proxy_args(context)?;
    args.extend(["cert".into(), "install".into()]);
    run_elevated(&runtime, &args, true)?;
    let mut state = read_state(context);
    state.cert_installed = true;
    write_state(context, &state)?;
    Ok(status(context))
}

fn status(context: &ProxyHostContext) -> ProxyRuntimeStatus {
    let state = read_state(context);
    let has_downloaded_runtime = active_downloaded_runtime_path(context).is_some();
    let has_bundled_runtime = bundled_runtime_path(context)
        .map(|path| path.is_file())
        .unwrap_or(false);
    let installed = has_downloaded_runtime || has_bundled_runtime;
    let pid = running_pid(&state);
    let running = state.running && pid.is_some();
    let started_at = if running { state.started_at } else { None };
    let active_version = if has_downloaded_runtime {
        state.active_version
    } else if has_bundled_runtime {
        Some(BUNDLED_RUNTIME_VERSION.to_string())
    } else {
        None
    };

    ProxyRuntimeStatus {
        active_version,
        previous_version: state.previous_version,
        last_failed_version: state.last_failed_version,
        installed,
        running,
        pid,
        started_at,
        has_cert_installed: state.cert_installed && cert_path(context).is_file(),
        log_path: Some(runtime_log_path(context).to_string_lossy().to_string()),
    }
}

fn stop_proxy(context: &ProxyHostContext, pid: Option<u32>) -> Result<()> {
    let runtime = active_runtime_path(context).context("proxy runtime is not installed")?;
    let mut args = common_proxy_args(context)?;
    args.push("stop".into());
    if let Some(pid) = pid {
        args.extend(["--pid".into(), pid.to_string()]);
    }
    run_elevated(&runtime, &args, true).map(|_| ())
}

fn terminate_proxy_process(context: &ProxyHostContext, pid: u32) -> Result<()> {
    if !process_exists(pid) {
        return Ok(());
    }
    let runtime = active_runtime_path(context).context("proxy runtime is not installed")?;
    let mut args = common_proxy_args(context)?;
    args.extend(["kill".into(), "--pid".into(), pid.to_string()]);
    run_elevated(&runtime, &args, true).map(|_| ())
}

fn root(context: &ProxyHostContext) -> PathBuf {
    PathBuf::from(&context.root_dir)
}

fn versions_dir(context: &ProxyHostContext) -> PathBuf {
    root(context).join("versions")
}

fn state_file(context: &ProxyHostContext) -> PathBuf {
    root(context).join("state.json")
}

fn runtime_log_path(context: &ProxyHostContext) -> PathBuf {
    root(context).join("runtime.log")
}

fn cert_path(context: &ProxyHostContext) -> PathBuf {
    proxy_data_dir(context).join("ca.pem")
}

fn proxy_data_dir(context: &ProxyHostContext) -> PathBuf {
    root(context).join("data")
}

fn common_proxy_args(context: &ProxyHostContext) -> Result<Vec<String>> {
    fs::create_dir_all(proxy_data_dir(context))?;
    Ok(vec![
        "--data-dir".to_string(),
        proxy_data_dir(context).to_string_lossy().to_string(),
    ])
}

fn read_state(context: &ProxyHostContext) -> ProxyRuntimeState {
    fs::read_to_string(state_file(context))
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn write_state(context: &ProxyHostContext, state: &ProxyRuntimeState) -> Result<()> {
    fs::create_dir_all(root(context))?;
    let json = serde_json::to_string_pretty(state)?;
    fs::write(state_file(context), json)?;
    Ok(())
}

fn mark_stopped(context: &ProxyHostContext) -> Result<()> {
    let mut state = read_state(context);
    state.running = false;
    state.last_pid = None;
    state.started_at = None;
    state.needs_hosts_cleanup = false;
    write_state(context, &state)
}

fn valid_version(version: &str) -> Result<()> {
    if version.is_empty()
        || version.contains("..")
        || version.contains('/')
        || version.contains('\\')
        || version.chars().any(|ch| ch.is_control())
    {
        anyhow::bail!("invalid proxy runtime version");
    }
    Ok(())
}

fn validate_compatibility(
    context: &ProxyHostContext,
    manifest: &ProxyRuntimeManifest,
) -> Result<()> {
    if let Some(min) = &manifest.min_app_version {
        if cmp_version(&context.app_version, min).is_lt() {
            anyhow::bail!("proxy runtime requires app >= {min}");
        }
    }
    if let Some(max) = &manifest.max_app_version {
        if cmp_version(&context.app_version, max).is_gt() {
            anyhow::bail!("proxy runtime requires app <= {max}");
        }
    }
    Ok(())
}

fn verify_signature(manifest: &ProxyRuntimeManifest) -> Result<()> {
    if manifest.signature.as_deref().unwrap_or_default().is_empty() {
        #[cfg(not(debug_assertions))]
        anyhow::bail!("proxy runtime signature is required");
    }
    Ok(())
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

fn runtime_file_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "mintcat-proxy.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "mintcat-proxy"
    }
}

fn active_runtime_path(context: &ProxyHostContext) -> Option<PathBuf> {
    active_downloaded_runtime_path(context).or_else(|| bundled_runtime_path(context).ok())
}

fn active_downloaded_runtime_path(context: &ProxyHostContext) -> Option<PathBuf> {
    let version = read_state(context).active_version?;
    if version == BUNDLED_RUNTIME_VERSION || valid_version(&version).is_err() {
        return None;
    }
    let path = versions_dir(context)
        .join(version)
        .join(runtime_file_name());
    path.is_file().then_some(path)
}

fn bundled_runtime_path(context: &ProxyHostContext) -> Result<PathBuf> {
    context
        .bundled_runtime_path
        .as_ref()
        .map(PathBuf::from)
        .context("bundled proxy runtime path is missing")
}

fn download(context: &ProxyHostContext, url: &str) -> Result<Vec<u8>> {
    let mut builder = reqwest::blocking::Client::builder();
    if let Some(proxy_url) = context
        .proxy_url
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
    }
    let response = builder.build()?.get(url).send()?;
    if !response.status().is_success() {
        anyhow::bail!("proxy runtime download failed: {}", response.status());
    }
    Ok(response.bytes()?.to_vec())
}

fn install_runtime_payload(bytes: &[u8], runtime_path: &Path) -> Result<()> {
    if is_zip_payload(bytes) {
        extract_runtime_from_zip(bytes, runtime_path)?;
    } else {
        fs::write(runtime_path, bytes)?;
    }
    Ok(())
}

fn is_zip_payload(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
}

fn extract_runtime_from_zip(bytes: &[u8], runtime_path: &Path) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("invalid proxy runtime zip")?;
    let Some(index) = find_runtime_zip_entry(&mut archive) else {
        anyhow::bail!("proxy runtime executable not found in zip");
    };
    let mut file = archive.by_index(index)?;
    let mut executable = Vec::new();
    file.read_to_end(&mut executable)?;
    fs::write(runtime_path, executable)?;
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
    #[cfg(target_os = "windows")]
    {
        name.to_ascii_lowercase().ends_with(".exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let normalized = name.to_ascii_lowercase();
        normalized == "mintcat-proxy" || normalized.starts_with("mintcat_proxy")
    }
}

fn should_install_cert_on_start(context: &ProxyHostContext) -> bool {
    let state = read_state(context);
    !state.cert_installed || !cert_path(context).is_file()
}

fn running_pid(state: &ProxyRuntimeState) -> Option<u32> {
    state.last_pid.filter(|pid| process_exists(*pid))
}

fn run_elevated(exe: &Path, args: &[String], wait: bool) -> Result<String> {
    #[cfg(target_os = "windows")]
    {
        return run_elevated_windows(exe, args, wait);
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = shell_quote(&exe.to_string_lossy());
        for arg in args {
            command.push(' ');
            command.push_str(&shell_quote(arg));
        }
        if !wait {
            command = format!("nohup {} >/dev/null 2>&1 & echo $!", command);
        }
        let script = format!(
            "do shell script {} with administrator privileges",
            apple_script_quote(&command)
        );
        let output = Command::new("osascript").args(["-e", &script]).output()?;
        if !output.status.success() {
            anyhow::bail!(
                "osascript failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (exe, args, wait);
        anyhow::bail!("mintcat proxy runtime is only supported on Windows and macOS");
    }
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct ShellExecuteInfoW {
    cb_size: u32,
    f_mask: u32,
    hwnd: *mut std::ffi::c_void,
    lp_verb: *const u16,
    lp_file: *const u16,
    lp_parameters: *const u16,
    lp_directory: *const u16,
    n_show: i32,
    h_inst_app: *mut std::ffi::c_void,
    lp_id_list: *mut std::ffi::c_void,
    lp_class: *const u16,
    hkey_class: *mut std::ffi::c_void,
    dw_hot_key: u32,
    h_icon: *mut std::ffi::c_void,
    h_process: *mut std::ffi::c_void,
}

#[cfg(target_os = "windows")]
#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn GetProcessId(process: *mut std::ffi::c_void) -> u32;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "windows")]
fn run_elevated_windows(exe: &Path, args: &[String], wait: bool) -> Result<String> {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr};

    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x00000040;
    const SW_HIDE: i32 = 0;
    const INFINITE: u32 = 0xFFFF_FFFF;

    fn wide_null(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let verb = wide_null(OsStr::new("runas"));
    let file = wide_null(exe.as_os_str());
    let parameters = windows_quote_args(args);
    let parameters = wide_null(OsStr::new(&parameters));
    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        f_mask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: ptr::null_mut(),
        lp_verb: verb.as_ptr(),
        lp_file: file.as_ptr(),
        lp_parameters: parameters.as_ptr(),
        lp_directory: ptr::null(),
        n_show: SW_HIDE,
        h_inst_app: ptr::null_mut(),
        lp_id_list: ptr::null_mut(),
        lp_class: ptr::null(),
        hkey_class: ptr::null_mut(),
        dw_hot_key: 0,
        h_icon: ptr::null_mut(),
        h_process: ptr::null_mut(),
    };
    let launched = unsafe { ShellExecuteExW(&mut info) };
    if launched == 0 {
        anyhow::bail!(
            "failed to launch elevated proxy runtime: {}",
            std::io::Error::last_os_error()
        );
    }
    if info.h_process.is_null() {
        return Ok(String::new());
    }
    let pid = unsafe { GetProcessId(info.h_process) };
    if wait {
        unsafe {
            WaitForSingleObject(info.h_process, INFINITE);
            CloseHandle(info.h_process);
        }
        return Ok(String::new());
    }
    unsafe {
        CloseHandle(info.h_process);
    }
    Ok(pid.to_string())
}

#[cfg(target_os = "windows")]
fn windows_quote_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| windows_quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "windows")]
fn windows_quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\\')
    {
        return arg.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

fn parse_pid(output: &str) -> Option<u32> {
    output
        .split_whitespace()
        .find_map(|part| part.trim().parse::<u32>().ok())
}

fn process_exists(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output();
        return output
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(false);
    }

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pid="])
            .output();
        return output
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .split_whitespace()
                        .any(|value| value == pid.to_string())
            })
            .unwrap_or(false);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = pid;
        false
    }
}

fn cleanup_old_versions(context: &ProxyHostContext, state: &ProxyRuntimeState) {
    let root = versions_dir(context);
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

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn apple_script_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
