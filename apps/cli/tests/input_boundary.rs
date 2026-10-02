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
