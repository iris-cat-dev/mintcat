use std::{io::Write, process::{Command, Stdio}};

use serde_json::Value;
use uuid::Uuid;

fn rejected(args: &[&str], input: &str) -> Value {
    let data_dir = std::env::temp_dir().join(format!("mintcat-cli-input-{}", Uuid::new_v4()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_mintcat"))
        .env_remove(mintcat_runtime_kernel::DATA_DIR_ENV)
        .env_remove("MINTCAT_CONFIG")
        .env_remove("MINTCAT_DAEMON_BIN")
        .arg("--json")
        .arg("--data-dir").arg(&data_dir)
        .arg("--daemon-bin").arg(data_dir.join("nonexistent-daemon"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .args(args).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    assert!(!data_dir.exists(), "invalid input must not initialize runtime storage or start a daemon");
    error
}

#[test]
fn pagination_boundary_is_rejected_before_daemon_start() {
    let error = rejected(&["games", "list", "--page-size", "101"], "");
    assert_eq!(error["error"]["details"]["path"], "$.params.pageSize");
}

#[test]
fn stdin_json_string_is_not_coerced_to_a_boolean_mutation() {
    let error = rejected(&[
        "profiles", "create", "--idempotency-key", "invalid-active", "--params-file", "-",
    ], "{\"gameId\":\"fixture\",\"name\":\"profile\",\"active\":\"false\"}");
    assert_eq!(error["error"]["details"]["path"], "$.params.active");
}

#[test]
fn packaged_cli_uses_the_current_manifest_without_initializing_data() {
    let package = std::env::temp_dir().join(format!("mintcat-cli-package-{}", Uuid::new_v4()));
    std::fs::create_dir(&package).unwrap();
    let binary = package.join(if cfg!(windows) { "mintcat.exe" } else { "mintcat" });
    std::fs::copy(env!("CARGO_BIN_EXE_mintcat"), &binary).unwrap();
    let data_dir = package.join("declared-data");
    std::fs::write(package.join("runtime.json"), br#"{"schemaVersion":2,"sources":{},"scopes":{}}"#).unwrap();
    let manifest_path = package.join("release-manifest.json");
    let mut manifest = serde_json::json!({"schemaVersion":2,"target":env!("MINTCAT_TARGET"),"variant":"cli"});
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let run = || Command::new(&binary)
        .env_remove(mintcat_runtime_kernel::DATA_DIR_ENV)
        .env_remove("MINTCAT_CONFIG").env_remove("MINTCAT_RUNTIME_CONFIG")
        .arg("--json").arg("--data-dir").arg(&data_dir).arg("environment")
        .output().unwrap();
    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"]["dataDir"], data_dir.to_str().unwrap());
    assert!(!data_dir.exists());

    manifest["schemaVersion"] = serde_json::json!(1);
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let output = run();
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
    assert!(!data_dir.exists());
    std::fs::remove_dir_all(package).unwrap();
}
