use mintcat_sdk::{command_spec, decode_request_value, CommandDescriptor, ValidatedRequest, COMMAND_SPECS, CORE_SCHEMA_JSON};
use serde_json::{json, Value};
use crate::{AgentError, SessionTarget};

pub fn agent_allowed(method: &str) -> bool {
    !matches!(method, "actions.approve" | "install.apply" | "install.uninstall" | "uninstall")
        && command_spec(method).is_some_and(|spec| spec.agent_exposed)
}

pub fn function_name(method: &str) -> String { method.replace('.', "__") }

pub fn tool_descriptors() -> &'static Value {
    static TOOLS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    let schema: Value = serde_json::from_str(CORE_SCHEMA_JSON).expect("generated SDK schema");
    Value::Array(COMMAND_SPECS.iter().filter(|spec| agent_allowed(spec.method)).map(|spec| {
        json!({"type":"function", "function": {
            "name":function_name(spec.method),
            "description":format!("Application command {}. Classification: {}. Explicit target and revisions required by the schema; never infer an active profile. Approval is human-only.", spec.method, spec.classification),
            "parameters":inline_schema(&schema, &json!({"$ref":spec.params_schema}), 0)
        }})
    }).collect())
    });
    &TOOLS
}

fn inline_schema(root: &Value, value: &Value, depth: usize) -> Value {
    assert!(depth <= 32, "generated SDK schema recursion limit");
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let node = root.pointer(reference.strip_prefix('#').unwrap_or(reference)).expect("generated SDK reference");
        return inline_schema(root, node, depth + 1);
    }
    match value {
        Value::Object(object) => Value::Object(object.iter().map(|(key, value)| (key.clone(), inline_schema(root, value, depth + 1))).collect()),
        Value::Array(values) => Value::Array(values.iter().map(|value| inline_schema(root, value, depth + 1)).collect()),
        _ => value.clone(),
    }
}
fn named_spec(name: &str) -> Option<&'static CommandDescriptor> {
    COMMAND_SPECS.iter().find(|spec| spec.method == name || spec.method.split('.').eq(name.split("__")))
}

pub(crate) fn display_tool_name(name: &str) -> &'static str {
    named_spec(name).map_or("unavailable-tool", |spec| spec.method)
}

pub fn decode_tool(name: &str, arguments: &str, target: &SessionTarget, generation: &str, key: &str) -> Result<ValidatedRequest, AgentError> {
    let spec = named_spec(name).ok_or(AgentError::PermissionDenied)?;
    // This is independent of which tools were advertised to the provider.
    if !agent_allowed(spec.method) { return Err(AgentError::PermissionDenied); }
    let params: Value = serde_json::from_str(arguments).map_err(|_| AgentError::InvalidTool)?;
    check_target(&params, target)?;
    decode_request_value(json!({
        "jsonrpc":"2.0", "id":key, "method":spec.method,
        "contractVersion":spec.contract_version, "sessionGeneration":generation,
        "deadlineMs":120000, "idempotencyKey":key, "params":params
    })).map_err(|_| AgentError::InvalidTool)
}

fn check_target(value: &Value, target: &SessionTarget) -> Result<(), AgentError> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let expected = match key.as_str() {
                    "gameId" => Some(json!(target.game_id)),
                    "profileId" | "sourceProfileId" => Some(json!(target.profile_id)),
                    "installationId" => Some(json!(target.installation_id)),
                    "profileRevision" | "expectedProfileRevision" => Some(json!(target.profile_revision)),
                    "expectedContentRevision" | "contentRevision" => Some(json!(target.content_revision)),
                    "installationRevision" | "expectedInstallationRevision" => Some(json!(target.installation_revision)),
                    "revision" | "expectedRevision" if fields.contains_key("profileId") => Some(json!(target.profile_revision)),
                    "expectedRevision" if fields.contains_key("installationId") => Some(json!(target.installation_revision)),
                    _ => None,
                };
                if expected.is_some_and(|expected| expected != *value) { return Err(AgentError::TargetMismatch); }
                check_target(value, target)?;
            }
        }
        Value::Array(values) => for value in values { check_target(value, target)?; },
        _ => {}
    }
    Ok(())
}
