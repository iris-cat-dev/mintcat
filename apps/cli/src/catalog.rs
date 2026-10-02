use std::{collections::BTreeMap, sync::LazyLock};

use anyhow::{anyhow, bail, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use mintcat_sdk::{CommandDescriptor, COMMAND_CATALOG_JSON, COMMAND_SPECS, CORE_SCHEMA_JSON};
use serde_json::{json, Value};

static SCHEMA: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(CORE_SCHEMA_JSON).expect("generated core schema"));
static CATALOG: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(COMMAND_CATALOG_JSON).expect("generated command catalog"));

#[derive(Default)]
struct Node { spec: Option<&'static CommandDescriptor>, children: BTreeMap<&'static str, Node> }

pub fn build(mut command: Command) -> Command {
    let mut root = Node::default();
    for spec in COMMAND_SPECS {
        if spec.cli.is_empty() { continue; }
        let mut node = &mut root;
        for segment in spec.cli { node = node.children.entry(segment).or_default(); }
        assert!(node.spec.replace(spec).is_none(), "duplicate canonical CLI command");
    }
    for (name, node) in root.children { command = command.subcommand(node_command(name, node)); }
    command
}
fn node_command(name: &'static str, node: Node) -> Command {
    let mut command = Command::new(name);
    if let Some(spec) = node.spec { command = command_for(command, spec); }
    else { command = command.subcommand_required(true).arg_required_else_help(true); }
    for (name, child) in node.children { command = command.subcommand(node_command(name, child)); }
    if name == "jobs" {
        if let Some(spec) = mintcat_sdk::command_spec("jobs.get") {
            command = command.subcommand(command_for(Command::new("wait"), spec)
                .about("Wait for a durable job; resume after timeout with the same --job-id"));
        }
    }
    command
}
fn command_for(command: Command, spec: &'static CommandDescriptor) -> Command {
    let summary = CATALOG["commands"].as_array().and_then(|commands| commands.iter().find(|entry| entry["method"] == spec.method))
        .and_then(|entry| entry["summary"].as_str()).unwrap_or(spec.method);
    let mut command = operation_options(command.about(summary.to_owned()).after_help(format!(
        "Application API: {} (contract {}). Permission: {}. Confirmation: {}.\nParameter flags are generated from the canonical schema; object/array values take JSON.\nTarget IDs, revisions and approvals are explicit; --yes never invents or bypasses them.",
        spec.method, spec.contract_version, spec.permission, spec.confirmation)));
    let schema = parameter_schema(spec);
    if let Some(properties) = schema["properties"].as_object() {
        for (name, property) in properties {
            let resolved = resolve(property);
            let required = schema["required"].as_array().is_some_and(|fields| fields.iter().any(|field| field == name));
            let help = format!("{}{}", description(resolved), if required { " (required, or provide in --params/--params-file)" } else { " (optional)" });
            let parser_schema = resolved.clone();
            let parser = clap::builder::ValueParser::new(move |input: &str| -> std::result::Result<String, String> {
                parse_parameter(&parser_schema, input).map(|_| input.to_owned()).map_err(|error| error.to_string())
            });
            command = command.arg(Arg::new(name.clone()).long(kebab(name)).value_name(type_name(resolved))
                .allow_hyphen_values(true).help(help).value_parser(parser));
        }
    }
    command
}
pub fn operation_options(command: Command) -> Command {
    command
        .arg(Arg::new("params").long("params").value_name("JSON").conflicts_with("params-file").help("Closed-schema parameters as a JSON object"))
        .arg(Arg::new("params-file").long("params-file").value_name("FILE|-").help("Read a JSON object from file, or stdin with -"))
        .arg(Arg::new("idempotency-key").long("idempotency-key").value_name("KEY").help("Explicit durable key for a mutation; reuse exactly after a lost reply"))
        .arg(Arg::new("yes").long("yes").action(ArgAction::SetTrue).help("Acknowledge only this explicit human command; never approve a returned action automatically"))
        .arg(Arg::new("all").long("all").action(ArgAction::SetTrue).conflicts_with_all(["wait", "events"]).help("Follow all cursor pages of a paginated read; output result.pages"))
        .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue).help("Wait for the returned durable job and emit its final snapshot"))
        .arg(Arg::new("events").long("events").action(ArgAction::SetTrue).help("Emit NDJSON job events and final snapshot, reconnecting with the last cursor"))
        .arg(Arg::new("wait-timeout").long("wait-timeout").default_value("300")
            .value_parser(clap::value_parser!(u64).range(1..=86400)).help("Total job-follow deadline in seconds; timeout does not cancel the job"))
        .arg(Arg::new("poll-ms").long("poll-ms").default_value("250")
            .value_parser(clap::value_parser!(u64).range(10..=60000)).help("Delay between job event polls in milliseconds"))
}
pub fn leaf(matches: &ArgMatches) -> (Vec<&str>, &ArgMatches) {
    let mut path = Vec::new();
    let mut current = matches;
    while let Some((name, child)) = current.subcommand() { path.push(name); current = child; }
    (path, current)
}
fn parameter_schema(spec: &CommandDescriptor) -> &'static Value {
    SCHEMA.pointer(spec.params_schema.strip_prefix('#').expect("local schema reference")).expect("generated parameter schema")
}
fn resolve(mut value: &Value) -> &Value {
    while let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        value = SCHEMA.pointer(reference.strip_prefix('#').expect("local schema reference")).expect("generated schema reference");
    }
    value
}
pub fn has_parameter(spec: &CommandDescriptor, name: &str) -> bool { parameter_schema(spec)["properties"].get(name).is_some() }
pub fn validate_operation_options(spec: &CommandDescriptor, matches: &ArgMatches) -> Result<()> {
    if matches.get_flag("all") && (spec.classification != "read" || !has_parameter(spec, "cursor")) {
        bail!("--all requires a paginated read command with a cursor parameter");
    }
    if (matches.get_flag("wait") || matches.get_flag("events")) && spec.method != "jobs.watch" {
        let result = resolve(SCHEMA.pointer(spec.result_schema.strip_prefix('#').expect("local schema reference")).expect("generated result schema"));
        if !result["required"].as_array().is_some_and(|fields| fields.iter().any(|field| field == "jobId")) {
            bail!("--wait/--events requires a command whose result contains a durable jobId");
        }
    }
    Ok(())
}
fn type_name(value: &Value) -> &'static str {
    match value.get("type").and_then(Value::as_str) {
        Some("string") => "STRING", Some("integer") => "INTEGER", Some("number") => "NUMBER",
        Some("boolean") => "true|false", _ => "JSON",
    }
}
fn description(value: &Value) -> String {
    let mut description = value.get("description").and_then(Value::as_str).unwrap_or(type_name(value)).to_owned();
    for name in ["enum", "minimum", "maximum", "minLength", "maxLength", "pattern"] {
        if let Some(boundary) = value.get(name) { description.push_str(&format!("; {name}={boundary}")); }
    }
    description
}
fn parse_parameter(schema: &Value, input: &str) -> Result<Value> {
    let schema = resolve(schema);
    let value = match schema.get("type").and_then(Value::as_str) {
        Some("string") => Value::String(input.to_owned()),
        Some("integer") => {
            let value: Value = serde_json::from_str(input).map_err(|_| anyhow!("expected an integer"))?;
            if !value.is_i64() && !value.is_u64() { bail!("expected an integer"); }
            value
        }
        Some("number") => {
            let value: Value = serde_json::from_str(input)?;
            if !value.is_number() { bail!("expected a number"); }
            value
        }
        Some("boolean") => match input { "true" => json!(true), "false" => json!(false), _ => bail!("expected true or false") },
        Some("object") => {
            let value: Value = serde_json::from_str(input)?;
            if !value.is_object() { bail!("expected a JSON object"); }
            value
        }
        Some("array") => {
            let value: Value = serde_json::from_str(input)?;
            if !value.is_array() { bail!("expected a JSON array"); }
            value
        }
        _ => serde_json::from_str(input).map_err(|error| anyhow!("expected a JSON value: {error}"))?,
    };
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(&value) { bail!("expected one of {}", schema["enum"]); }
    }
    Ok(value)
}
pub fn parameters(spec: &CommandDescriptor, matches: &ArgMatches) -> Result<Value> {
    let mut params = super::read_params(matches)?;
    if let Some(properties) = parameter_schema(spec)["properties"].as_object() {
        for (name, schema) in properties {
            if let Some(input) = matches.get_one::<String>(name) {
                if params.get(name).is_some() { bail!("parameter {name} was supplied both in JSON and as --{}", kebab(name)); }
                params[name] = parse_parameter(schema, input)?;
            }
        }
    }
    Ok(params)
}
fn kebab(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        if character.is_ascii_uppercase() { output.push('-'); output.push(character.to_ascii_lowercase()); }
        else { output.push(character); }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_input_does_not_coerce_strings_booleans_or_fractional_revisions() {
        assert_eq!(parse_parameter(&json!({"type":"string"}), "false").unwrap(), json!("false"));
        assert_eq!(parse_parameter(&json!({"type":"boolean"}), "false").unwrap(), json!(false));
        assert!(parse_parameter(&json!({"type":"boolean"}), "yes").is_err());
        assert!(parse_parameter(&json!({"type":"integer"}), "1.5").is_err());
        assert!(parse_parameter(&json!({"type":"array"}), "{}").is_err());
    }
    #[test]
    fn conflicting_json_and_flag_target_is_rejected() {
        let spec = mintcat_sdk::command_spec("profiles.list").unwrap();
        let matches = command_for(Command::new("list"), spec).try_get_matches_from([
            "list", "--params", "{\"gameId\":\"a\"}", "--game-id", "b",
        ]).unwrap();
        assert!(parameters(spec, &matches).unwrap_err().to_string().contains("both"));
    }
    #[test]
    fn follow_options_reject_non_job_mutations_before_execution() {
        let spec = mintcat_sdk::command_spec("profiles.create").unwrap();
        let matches = command_for(Command::new("create"), spec)
            .try_get_matches_from(["create", "--wait"]).unwrap();
        assert!(validate_operation_options(spec, &matches).is_err());
        let spec = mintcat_sdk::command_spec("actions.approve").unwrap();
        let matches = command_for(Command::new("approve"), spec)
            .try_get_matches_from(["approve", "--events"]).unwrap();
        validate_operation_options(spec, &matches).unwrap();
    }
}
