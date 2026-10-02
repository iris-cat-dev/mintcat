mod directory_snapshot;
mod generated;
mod worker;

pub use directory_snapshot::*;
pub use generated::*;
pub use worker::*;

use std::{error::Error, fmt, sync::LazyLock};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractDecodeError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl ContractDecodeError {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ContractDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

impl Error for ContractDecodeError {}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedRequest {
    pub envelope: JsonRpcRequest,
    pub command: ApplicationCommand,
    pub descriptor: &'static CommandDescriptor,
}

pub fn decode_request(input: &str) -> Result<ValidatedRequest, ContractDecodeError> {
    let value: Value = serde_json::from_str(input)
        .map_err(|error| ContractDecodeError::new("MALFORMED_JSON", "$", error.to_string()))?;
    decode_request_value(value)
}

pub fn decode_request_value(value: Value) -> Result<ValidatedRequest, ContractDecodeError> {
    validate_definition("JsonRpcRequest", &value)
        .map_err(|violation| violation.with_code("INVALID_REQUEST"))?;

    let object = value.as_object().expect("validated request is an object");
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .expect("validated method is a string");
    let descriptor = command_spec(method).ok_or_else(|| {
        ContractDecodeError::new(
            "UNKNOWN_METHOD",
            "$.method",
            format!("unknown method {method}"),
        )
    })?;
    let contract_version = object
        .get("contractVersion")
        .and_then(Value::as_u64)
        .expect("validated contractVersion is an unsigned integer");
    if contract_version != descriptor.contract_version {
        return Err(ContractDecodeError::new(
            "INCOMPATIBLE_CONTRACT",
            "$.contractVersion",
            format!(
                "method {} requires contract version {}, got {contract_version}",
                descriptor.method, descriptor.contract_version
            ),
        ));
    }
    if descriptor.idempotency == "required"
        && object
            .get("idempotencyKey")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(ContractDecodeError::new(
            "INVALID_REQUEST",
            "$.idempotencyKey",
            format!("method {} requires an idempotency key", descriptor.method),
        ));
    }

    let params = object
        .get("params")
        .expect("validated request contains params");
    validate_reference(descriptor.params_schema, params, "$.params")
        .map_err(|violation| violation.with_code("INVALID_ARGUMENT"))?;

    let envelope: JsonRpcRequest = serde_json::from_value(value)
        .map_err(|error| ContractDecodeError::new("INVALID_REQUEST", "$", error.to_string()))?;
    let command_value = serde_json::json!({
        "method": descriptor.method,
        "params": envelope.params.clone(),
    });
    let command = serde_json::from_value(command_value).map_err(|error| {
        ContractDecodeError::new("INVALID_ARGUMENT", "$.params", error.to_string())
    })?;

    Ok(ValidatedRequest {
        envelope,
        command,
        descriptor,
    })
}

pub fn validate_command_result(
    method: &str,
    contract_version: u64,
    result: &Value,
) -> Result<(), ContractDecodeError> {
    let descriptor = command_spec(method).ok_or_else(|| {
        ContractDecodeError::new(
            "UNKNOWN_METHOD",
            "$.method",
            format!("unknown method {method}"),
        )
    })?;
    if contract_version != descriptor.contract_version {
        return Err(ContractDecodeError::new(
            "INCOMPATIBLE_CONTRACT",
            "$.contractVersion",
            format!(
                "method {method} requires contract version {}, got {contract_version}",
                descriptor.contract_version
            ),
        ));
    }
    validate_reference(descriptor.result_schema, result, "$.result")
        .map_err(|violation| violation.with_code("INVALID_RESULT"))
}

pub fn validate_rpc_response(value: &Value) -> Result<(), ContractDecodeError> {
    validate_definition("JsonRpcResponse", value)
        .map_err(|violation| violation.with_code("INVALID_RESULT"))?;
    if value.get("result").is_some() == value.get("error").is_some() {
        return Err(ContractDecodeError::new(
            "INVALID_RESULT", "$", "response must contain exactly one of result or error",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct SchemaViolation {
    path: String,
    message: String,
}

impl SchemaViolation {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    fn with_code(self, code: &'static str) -> ContractDecodeError {
        ContractDecodeError::new(code, self.path, self.message)
    }
}

impl std::fmt::Display for SchemaViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for SchemaViolation {}

static CORE_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(CORE_SCHEMA_JSON).expect("generated core schema must be valid JSON")
});

fn core_schema() -> &'static Value {
    &CORE_SCHEMA
}

fn validate_definition(name: &str, value: &Value) -> Result<(), SchemaViolation> {
    let reference = format!("#/$defs/{name}");
    validate_reference(&reference, value, "$")
}

fn validate_reference(reference: &str, value: &Value, path: &str) -> Result<(), SchemaViolation> {
    let name = reference.strip_prefix("#/$defs/").ok_or_else(|| {
        SchemaViolation::new(path, format!("unsupported schema reference {reference}"))
    })?;
    let schema = core_schema()
        .get("$defs")
        .and_then(|definitions| definitions.get(name))
        .ok_or_else(|| SchemaViolation::new(path, format!("missing schema definition {name}")))?;
    validate_schema(schema, value, path)
}

pub fn validate_schema(schema: &Value, value: &Value, path: &str) -> Result<(), SchemaViolation> {
    let Some(schema) = schema.as_object() else {
        return Err(SchemaViolation::new(path, "schema node must be an object"));
    };
    if schema.is_empty() {
        return Ok(());
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return validate_reference(reference, value, path);
    }
    if let Some(expected) = schema.get("const") {
        if expected != value {
            return Err(SchemaViolation::new(
                path,
                format!("expected constant {expected}"),
            ));
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array) {
        if !options.iter().any(|candidate| candidate == value) {
            return Err(SchemaViolation::new(
                path,
                "value is not in the allowed enum",
            ));
        }
    }
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = variants
            .iter()
            .filter(|variant| validate_schema(variant, value, path).is_ok())
            .count();
        if matches != 1 {
            return Err(SchemaViolation::new(
                path,
                "value must match exactly one schema variant",
            ));
        }
        return Ok(());
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("object") => validate_object(schema, value, path),
        Some("array") => validate_array(schema, value, path),
        Some("string") => validate_string(schema, value, path),
        Some("integer") => validate_integer(schema, value, path),
        Some("number") => {
            if value.is_number() {
                Ok(())
            } else {
                Err(type_error(path, "number"))
            }
        }
        Some("boolean") => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(type_error(path, "boolean"))
            }
        }
        Some("null") => {
            if value.is_null() {
                Ok(())
            } else {
                Err(type_error(path, "null"))
            }
        }
        Some(other) => Err(SchemaViolation::new(
            path,
            format!("unsupported schema type {other}"),
        )),
        None => Ok(()),
    }
}

fn validate_object(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<(), SchemaViolation> {
    let object = value
        .as_object()
        .ok_or_else(|| type_error(path, "object"))?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(SchemaViolation::new(
                    child_path(path, field),
                    "required field is missing",
                ));
            }
        }
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    for (field, field_value) in object {
        if let Some(field_schema) = properties.and_then(|items| items.get(field)) {
            validate_schema(field_schema, field_value, &child_path(path, field))?;
        } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            return Err(SchemaViolation::new(
                child_path(path, field),
                "unknown field is not allowed",
            ));
        }
    }
    Ok(())
}

fn validate_array(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<(), SchemaViolation> {
    let items = value.as_array().ok_or_else(|| type_error(path, "array"))?;
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64) {
        if items.len() < minimum as usize {
            return Err(SchemaViolation::new(
                path,
                format!("array requires at least {minimum} items"),
            ));
        }
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            validate_schema(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_string(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<(), SchemaViolation> {
    let text = value.as_str().ok_or_else(|| type_error(path, "string"))?;
    let length = text.chars().count() as u64;
    if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64) {
        if length < minimum {
            return Err(SchemaViolation::new(
                path,
                format!("string is shorter than {minimum}"),
            ));
        }
    }
    if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64) {
        if length > maximum {
            return Err(SchemaViolation::new(
                path,
                format!("string is longer than {maximum}"),
            ));
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        if pattern == "^sha256:[0-9a-f]{64}$" {
            let valid = text.strip_prefix("sha256:").is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            });
            if !valid {
                return Err(SchemaViolation::new(
                    path,
                    "string does not match SHA-256 digest format",
                ));
            }
        } else {
            return Err(SchemaViolation::new(
                path,
                format!("unsupported schema pattern {pattern}"),
            ));
        }
    }
    Ok(())
}

fn validate_integer(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
) -> Result<(), SchemaViolation> {
    let number = value
        .as_i64()
        .map(|value| value as f64)
        .or_else(|| value.as_u64().map(|value| value as f64))
        .ok_or_else(|| type_error(path, "integer"))?;
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if number < minimum {
            return Err(SchemaViolation::new(
                path,
                format!("integer is below {minimum}"),
            ));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if number > maximum {
            return Err(SchemaViolation::new(
                path,
                format!("integer is above {maximum}"),
            ));
        }
    }
    Ok(())
}

fn type_error(path: &str, expected: &str) -> SchemaViolation {
    SchemaViolation::new(path, format!("expected {expected}"))
}

fn child_path(path: &str, field: &str) -> String {
    format!("{path}.{field}")
}

#[cfg(test)]
mod tests {
    use super::{decode_request_value, validate_command_result};
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BoundaryCase {
        name: String,
        request: Value,
        ok: bool,
        error_code: Option<String>,
    }

    #[test]
    fn shared_request_boundaries_match_expected_codes() {
        let cases: Vec<BoundaryCase> = serde_json::from_str(include_str!(
            "../../../fixtures/contracts/rpc-boundaries.json"
        ))
        .unwrap();
        for case in cases {
            let result = decode_request_value(case.request);
            assert_eq!(result.is_ok(), case.ok, "{}", case.name);
            if let Some(expected) = case.error_code {
                assert_eq!(result.unwrap_err().code, expected, "{}", case.name);
            }
        }
    }

    #[test]
    fn command_result_uses_generated_result_schema() {
        assert!(validate_command_result(
            "mods.setVersion",
            1,
            &serde_json::json!({"jobId": "job-1"}),
        )
        .is_ok());
        let error = validate_command_result("mods.setVersion", 1, &serde_json::json!({"jobId": 1}))
            .unwrap_err();
        assert_eq!(error.code, "INVALID_RESULT");
        assert_eq!(error.path, "$.result.jobId");
    }

    #[test]
    fn nullable_update_recovery_reasons_are_valid_but_wrong_types_are_rejected() {
        for reason in [serde_json::Value::Null, serde_json::json!("explicit recovery required")] {
            let result = serde_json::json!({"operations": [], "recoveryReason": reason, "scopeBlockers": {}});
            assert!(validate_command_result("updates.operations", 1, &result).is_ok());
        }
        let result = serde_json::json!({"operations": [], "recoveryReason": false, "scopeBlockers": {}});
        let error = validate_command_result("updates.operations", 1, &result).unwrap_err();
        assert_eq!(error.code, "INVALID_RESULT");
        assert_eq!(error.path, "$.result.recoveryReason");
        assert!(super::validate_schema(&serde_json::json!({"type": "null"}), &serde_json::json!("not null"), "$").is_err());
    }
}
