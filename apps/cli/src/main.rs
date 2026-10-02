mod catalog;
mod jobs;

use std::{io::{self, Read, Write}, path::{Path, PathBuf}, process::ExitCode};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use mintcat_runtime_ipc::{connect_or_start, IpcClient, IpcError};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths, DATA_DIR_ENV};
use mintcat_sdk::{command_spec, decode_request_value, validate_command_result, validate_rpc_response, AppError, CommandDescriptor, JsonRpcId, JsonRpcRequest};
use serde_json::{json, Value};
use uuid::Uuid;

const DAEMON_BIN_ENV: &str = "MINTCAT_DAEMON_BIN";

#[derive(Debug)]
struct Failure { code: String, details: Value, exit: u8 }
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}: {}", self.code, self.details) }
}
impl std::error::Error for Failure {}
fn failure(code: &str, details: Value) -> anyhow::Error {
    Failure { code: code.into(), details, exit: exit_code(code) }.into()
}
fn exit_code(code: &str) -> u8 {
    match code {
        "INVALID_ARGUMENT" | "INVALID_REQUEST" | "MALFORMED_JSON" | "UNKNOWN_METHOD" => 2,
        "PERMISSION_DENIED" | "ACTION_REQUIRED" | "INTERACTION_REQUIRED" | "APPROVAL_REQUIRED" => 3,
        "CONFLICT" | "REVISION_CONFLICT" | "STALE_PLAN" | "RESOURCE_BUSY" | "RECOVERY_REQUIRED" | "GAME_RUNNING" => 4,
        "NOT_FOUND" | "MODULE_UNAVAILABLE" => 5,
        "TRANSPORT_ERROR" | "INVALID_RESPONSE" | "INVALID_RESULT" | "INCOMPATIBLE_CONTRACT" | "STALE_SESSION" => 6,
        "DEADLINE_EXCEEDED" | "WAIT_TIMEOUT" => 7,
        "JOB_CANCELLED" => 8,
        _ => 1,
    }
}
fn application_error(error: AppError) -> anyhow::Error {
    let code = error.code.clone();
    failure(&code, serde_json::to_value(error).expect("AppError is serializable"))
}

#[tokio::main]
async fn main() -> ExitCode {
    let command = cli();
    let args: Vec<_> = std::env::args_os().collect();
    let machine = args.iter().any(|arg| arg == "--json" || arg == "--events")
        || args.windows(2).any(|pair| pair[0] == "jobs" && pair[1] == "watch");
    let matches = match command.try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) => {
            if matches!(error.kind(), clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            if machine { let _ = emit(&json!({"error":{"code":"INVALID_ARGUMENT","message":error.to_string()}}), true); }
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let (path, leaf) = catalog::leaf(&matches);
    let machine = machine || path == ["jobs", "watch"]
        || (path == ["api"] && leaf.get_one::<String>("method").is_some_and(|method| method == "jobs.watch"));
    match run(&matches).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let (code, details, exit) = if let Some(failure) = error.downcast_ref::<Failure>() {
                (failure.code.as_str(), failure.details.clone(), failure.exit)
            } else { ("INVALID_ARGUMENT", json!({"message":format!("{error:#}")}), 2) };
            let _ = emit(&json!({"error":{"code":code,"details":details}}), machine);
            eprintln!("mintcat: {error:#}");
            ExitCode::from(exit)
        }
    }
}

fn cli() -> Command {
    let command = Command::new("mintcat").version(env!("CARGO_PKG_VERSION"))
        .about("MintCat product CLI — Application API without a running desktop")
        .subcommand_required(true).arg_required_else_help(true)
        .arg(Arg::new("data-dir").long("data-dir").env(DATA_DIR_ENV).global(true).value_name("DIRECTORY")
            .help("Declared runtime data directory; no legacy desktop directory is inferred"))
        .arg(Arg::new("config").long("config").env("MINTCAT_CONFIG").global(true).value_name("FILE")
            .help("Explicit JSON config with dataDir, optional daemonBin and environment"))
        .arg(Arg::new("daemon-bin").long("daemon-bin").env(DAEMON_BIN_ENV).global(true).value_name("FILE")
            .help("Daemon executable; defaults to sibling mintcatd"))
        .arg(Arg::new("environment").long("environment").global(true).value_parser(["development", "test", "production"]))
        .arg(Arg::new("json").long("json").global(true).action(ArgAction::SetTrue).help("Compact stable JSON; diagnostics go to stderr"))
        .arg(Arg::new("timeout").long("timeout").global(true).default_value("30")
            .value_parser(clap::value_parser!(u64).range(1..=3600)).help("Transport request deadline in seconds"))
        .subcommand(Command::new("environment").about("Show declared runtime paths without initializing or starting the daemon"))
        .subcommand(Command::new("completion").about("Generate shell completion from the canonical command catalog")
            .arg(Arg::new("shell").required(true).value_parser(["bash", "elvish", "fish", "powershell", "zsh"])))
        .subcommand(catalog::operation_options(Command::new("api").about("Raw canonical Application API escape hatch (not a domain alias)"))
            .arg(Arg::new("method").required(true).value_name("METHOD")));
    catalog::build(command)
}

struct Connection {
    paths: RuntimePaths,
    daemon_bin: PathBuf,
    environment: RuntimeEnvironment,
    deadline_ms: u64,
    client: Option<IpcClient>,
}
impl Connection {
    async fn connect(&mut self) -> Result<()> {
        if self.client.is_none() {
            self.client = Some(connect_or_start(&self.paths, &self.daemon_bin, self.environment).await
                .map_err(|error| failure("TRANSPORT_ERROR", json!({"message":error.to_string()})))?);
        }
        Ok(())
    }
    async fn request(&mut self, method: &str, params: Value, key: Option<&str>) -> Result<Value> {
        let spec = command_spec(method).ok_or_else(|| failure("UNKNOWN_METHOD", json!({"method":method})))?;
        // Validate before connecting: malformed requests must not launch a daemon or mutate state.
        let mut request = JsonRpcRequest {
            jsonrpc: "2.0".into(), id: JsonRpcId::String(Uuid::new_v4().to_string()),
            method: method.into(), contract_version: spec.contract_version,
            session_generation: "pending-connection".into(), deadline_ms: self.deadline_ms,
            idempotency_key: key.map(str::to_owned), params,
        };
        decode_request_value(serde_json::to_value(&request)?)
            .map_err(|error| failure(error.code, json!({"path":error.path,"message":error.message})))?;
        // Only reads or explicitly keyed mutations may be replayed after a lost reply.
        let replay_safe = spec.classification == "read" || key.is_some();
        for attempt in 0..=usize::from(replay_safe) {
            self.connect().await?;
            let client = self.client.as_mut().expect("connected client");
            request.session_generation = client.generation_id.clone();
            let response = match client.request(&request).await {
                Ok(response) => response,
                Err(error) => {
                    self.client = None; // A timed-out stream can still contain an old reply.
                    if attempt == 0 && replay_safe { continue; }
                    let code = if matches!(error, IpcError::Deadline) { "DEADLINE_EXCEEDED" } else { "TRANSPORT_ERROR" };
                    return Err(failure(code, json!({"method":method,"idempotencyKey":key,"message":error.to_string(),"recovery":"Replay exactly the same parameters and idempotency key, or query the known job with jobs get/wait."})));
                }
            };
            validate_rpc_response(&serde_json::to_value(&response)?)
                .map_err(|error| failure(error.code, json!({"method":method,"path":error.path,"message":error.message})))?;
            if response.id != request.id {
                self.client = None;
                return Err(failure("INVALID_RESPONSE", json!({"method":method,"message":"Response request ID mismatch"})));
            }
            if let Some(error) = response.error {
                if error.code == "STALE_SESSION" && attempt == 0 && replay_safe { self.client = None; continue; }
                return Err(application_error(error));
            }
            let result = response.result.expect("exclusive result");
            validate_command_result(method, spec.contract_version, &result)
                .map_err(|error| failure(error.code, json!({"path":error.path,"message":error.message})))?;
            return Ok(result);
        }
        unreachable!("request loop returns a response or error")
    }
}

async fn run(matches: &ArgMatches) -> Result<()> {
    let (path, leaf) = catalog::leaf(matches);
    if path == ["completion"] {
        let shell = leaf.get_one::<String>("shell").unwrap().parse::<clap_complete::Shell>()
            .map_err(|error| anyhow!("invalid completion shell: {error}"))?;
        clap_complete::generate(shell, &mut cli(), "mintcat", &mut io::stdout());
        return Ok(());
    }
    let config = load_config(matches)?;
    let portable_data_dir = packaged_data_directory()?;
    let data_dir = matches.get_one::<String>("data-dir").map(PathBuf::from)
        .or_else(|| config.get("dataDir").and_then(Value::as_str).map(PathBuf::from))
        .or(portable_data_dir)
        .ok_or_else(|| anyhow!("Declare --data-dir, {DATA_DIR_ENV}, dataDir in --config, or a packaged portable data directory; legacy desktop storage is never inferred"))?;
    let paths = RuntimePaths::new(data_dir)?;
    let compact = matches.get_flag("json") || leaf.try_get_one::<bool>("events").ok().flatten().copied().unwrap_or(false);
    if path == ["environment"] { return emit(&json!({"result":paths.layout()}), compact); }
    let explicit_daemon = matches.get_one::<String>("daemon-bin").map(String::as_str)
        .or_else(|| config.get("daemonBin").and_then(Value::as_str));
    let environment = matches.get_one::<String>("environment").map(String::as_str)
        .or_else(|| config.get("environment").and_then(Value::as_str)).unwrap_or("production");
    let environment = match environment {
        "development" => RuntimeEnvironment::Development,
        "test" => RuntimeEnvironment::Test,
        "production" => RuntimeEnvironment::Production,
        _ => bail!("config environment must be development, test, or production"),
    };
    let mut connection = Connection { paths, daemon_bin: resolve_daemon_bin(explicit_daemon.map(Path::new))?, environment,
        deadline_ms: *matches.get_one::<u64>("timeout").unwrap() * 1000, client: None };
    if path == ["jobs", "wait"] {
        catalog::validate_operation_options(command_spec("jobs.get").unwrap(), leaf)?;
        let params = catalog::parameters(command_spec("jobs.get").unwrap(), leaf)?;
        let snapshot = connection.request("jobs.get", params, None).await?;
        let job_id = snapshot["jobId"].as_str().expect("validated job snapshot");
        return jobs::follow(&mut connection, job_id, 0, leaf, compact).await;
    }
    let spec = if path == ["api"] {
        let method = leaf.get_one::<String>("method").unwrap();
        command_spec(method).ok_or_else(|| failure("UNKNOWN_METHOD", json!({"method":method})))?
    } else {
        mintcat_sdk::COMMAND_SPECS.iter().find(|spec| spec.cli == path.as_slice())
            .ok_or_else(|| anyhow!("No canonical command for {}", path.join(" ")))?
    };
    catalog::validate_operation_options(spec, leaf)?;
    let params = if path == ["api"] { read_params(leaf)? } else { catalog::parameters(spec, leaf)? };
    if spec.confirmation == "required" && !leaf.get_flag("yes") {
        return Err(failure("APPROVAL_REQUIRED", json!({"method":spec.method,"message":"Use --yes to acknowledge this explicitly supplied human operation. It does not create or bypass a durable approval, revision, plan hash, or module lock."})));
    }
    let key = leaf.get_one::<String>("idempotency-key").map(String::as_str);
    let all = leaf.get_flag("all");
    if all {
        let result = paginated(&mut connection, spec, params, key).await?;
        return emit(&json!({"result":result}), compact);
    }
    let watch_target = if spec.method == "jobs.watch" && (path != ["api"] || leaf.get_flag("wait") || leaf.get_flag("events")) {
        Some((params.get("jobId").and_then(Value::as_str).map(str::to_owned), params.get("afterSeq").and_then(Value::as_u64), params.get("limit").and_then(Value::as_u64).unwrap_or(100)))
    } else { None };
    let result = connection.request(spec.method, params, key).await?;
    if let Some((Some(job_id), Some(cursor), limit)) = watch_target {
        return jobs::follow_initial(&mut connection, &job_id, cursor, leaf, compact, Some((result, limit))).await;
    }
    emit(&json!({"result":result}), compact)?;
    if leaf.get_flag("wait") || leaf.get_flag("events") {
        if let Some(job_id) = result.get("jobId").and_then(Value::as_str) {
            jobs::follow(&mut connection, job_id, 0, leaf, compact).await?;
        } else { bail!("--wait/--events requires a result containing jobId; the command result was emitted"); }
    }
    Ok(())
}

async fn paginated(connection: &mut Connection, spec: &CommandDescriptor, mut params: Value, key: Option<&str>) -> Result<Value> {
    let mut pages = Vec::new();
    let mut cursors = std::collections::HashSet::new();
    if let Some(cursor) = params.get("cursor").and_then(Value::as_str) { cursors.insert(cursor.to_owned()); }
    loop {
        let page = connection.request(spec.method, params.clone(), key).await?;
        let next = page.get("nextCursor").and_then(Value::as_str).map(str::to_owned);
        pages.push(page);
        match next {
            None => break,
            Some(cursor) => {
                if !cursors.insert(cursor.clone()) { return Err(failure("INVALID_RESPONSE", json!({"message":"Pagination cursor repeated","cursor":cursor}))); }
                params["cursor"] = Value::String(cursor);
            }
        }
    }
    Ok(json!({"pages":pages}))
}

fn load_config(matches: &ArgMatches) -> Result<Value> {
    let Some(path) = matches.get_one::<String>("config") else { return Ok(json!({})); };
    let value: Value = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("read config {path}"))?)?;
    let object = value.as_object().ok_or_else(|| anyhow!("config must be a JSON object"))?;
    for (key, value) in object {
        if !["dataDir", "daemonBin", "environment"].contains(&key.as_str()) || !value.is_string() {
            bail!("config accepts only string dataDir, daemonBin and environment fields");
        }
    }
    Ok(value)
}
fn read_params(matches: &ArgMatches) -> Result<Value> {
    let value = if let Some(path) = matches.get_one::<String>("params-file") {
        let mut bytes = Vec::new();
        if path == "-" { io::stdin().take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?; }
        else { std::fs::File::open(path)?.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?; }
        if bytes.len() > 16 * 1024 * 1024 { bail!("parameter input exceeds 16 MiB"); }
        serde_json::from_slice(&bytes).context("--params-file must contain valid JSON")?
    } else if let Some(input) = matches.get_one::<String>("params") {
        serde_json::from_str(input).context("--params must contain valid JSON")?
    } else { json!({}) };
    if !value.is_object() { bail!("parameters must be a JSON object"); }
    Ok(value)
}
fn resolve_daemon_bin(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit { return Ok(path.to_path_buf()); }
    let current = std::env::current_exe().context("could not locate mintcat executable")?;
    Ok(current.parent().context("mintcat executable has no parent directory")?
        .join(if cfg!(windows) { "mintcatd.exe" } else { "mintcatd" }))
}
fn packaged_data_directory() -> Result<Option<PathBuf>> {
    let executable = std::env::current_exe().context("could not locate mintcat executable")?;
    let sibling = executable.parent().context("mintcat executable has no parent directory")?;
    let Some(resources) = mintcat_runtime_kernel::packaged_resource_directory(&executable)? else { return Ok(None); };
    let path = resources.join("release-manifest.json");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => bail!("Packaged immutable release manifest is missing"),
        Err(error) => return Err(error).with_context(|| format!("read packaged manifest {}", path.display())),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() { bail!("Packaged release manifest must be a regular file"); }
    let manifest: Value = serde_json::from_slice(&std::fs::read(&path)?).context("Packaged release manifest must contain valid JSON")?;
    if manifest["schemaVersion"] != 1 || manifest["target"].as_str() != Some(env!("MINTCAT_TARGET")) {
        bail!("Packaged runtime manifest differs from this CLI's exact compiled target");
    }
    let variant = manifest["variant"].as_str().ok_or_else(|| anyhow!("Packaged release variant is missing"))?;
    if !["standard", "portable", "fixed-webview2", "cli"].contains(&variant) { bail!("Invalid packaged CLI release variant"); }
    if variant != "portable" { return Ok(None); }
    if !cfg!(windows) || manifest["dataDirectory"]["mode"] != "portable" { bail!("Invalid portable data directory declaration"); }
    let path = PathBuf::from(manifest["dataDirectory"]["path"].as_str().ok_or_else(|| anyhow!("Portable release lacks its declared data directory"))?);
    if path.as_os_str().is_empty() || path.components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
        bail!("Portable data directory must be a relative child without traversal");
    }
    Ok(Some(sibling.join(path)))
}
fn emit(value: &Value, compact: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if compact { serde_json::to_writer(&mut stdout, value)?; }
    else { serde_json::to_writer_pretty(&mut stdout, value)?; }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}
