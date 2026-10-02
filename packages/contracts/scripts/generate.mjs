#!/usr/bin/env node
import {spawnSync} from 'node:child_process';
import {readFileSync, writeFileSync, mkdirSync, existsSync} from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import {fileURLToPath} from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const contractsDir = path.resolve(scriptDir, '..');
const rootDir = path.resolve(contractsDir, '../..');
const corePath = path.join(contractsDir, 'schema/core.schema.json');
const catalogPath = path.join(contractsDir, 'commands.json');
const core = JSON.parse(readFileSync(corePath, 'utf8'));
const catalog = JSON.parse(readFileSync(catalogPath, 'utf8'));
const definitions = core.$defs ?? {};
const checkOnly = process.argv.includes('--check');

function fail(message) {
  throw new Error(`[contracts] ${message}`);
}

function definitionName(ref) {
  const prefix = '#/$defs/';
  if (typeof ref !== 'string' || !ref.startsWith(prefix)) {
    fail(`unsupported schema reference: ${String(ref)}`);
  }
  return ref.slice(prefix.length);
}

function definitionFor(ref) {
  const name = definitionName(ref);
  const value = definitions[name];
  if (!value) fail(`missing schema definition: ${name}`);
  return value;
}

function validateSources() {
  const requiredDefinitions = [
    'JsonRpcRequest', 'JsonRpcResponse', 'AppError', 'JobRef', 'JobEvent',
    'ActionRequired', 'ModRef', 'ProfileRef', 'ProfileModRef', 'ArtifactRef',
    'DownloadMode', 'OperationFence', 'ModuleManifest', 'ModuleSignature',
    'ArtifactGrantDescriptor', 'WorkerHello', 'WorkerHostHello', 'WorkerRequest',
    'WorkerResponse', 'JobEventPage', 'ScopeReleaseSet', 'EnvironmentLock',
    'ScopedProviderBinding', 'PackageVersion', 'PackageSchemaVersion',
    'ContractVersion', 'DataSchemaVersion',
  ];
  for (const name of requiredDefinitions) {
    if (!definitions[name]) fail(`required core definition is missing: ${name}`);
  }
  if (catalog.schemaVersion !== 1) fail('unsupported command catalog schemaVersion');
  if (typeof catalog.contractSetVersion !== 'string' || catalog.contractSetVersion.length === 0) {
    fail('contractSetVersion is required');
  }
  if (!Array.isArray(catalog.commands) || catalog.commands.length === 0) {
    fail('at least one command is required');
  }

  const methods = new Set();
  const cliPaths = new Set();
  const classifications = new Set(['read', 'mutation']);
  const executions = new Set(['sync', 'job']);
  const idempotencyModes = new Set(['none', 'optional', 'required']);
  const confirmationModes = new Set(['none', 'policy', 'required']);
  for (const command of catalog.commands) {
    if (typeof command.method !== 'string' || !command.method.includes('.')) {
      fail(`invalid command method: ${String(command.method)}`);
    }
    if (methods.has(command.method)) fail(`duplicate command method: ${command.method}`);
    methods.add(command.method);
    if (!Number.isInteger(command.contractVersion) || command.contractVersion < 1) {
      fail(`invalid contract version for ${command.method}`);
    }
    if (!classifications.has(command.classification)) fail(`invalid classification for ${command.method}`);
    if (!executions.has(command.execution)) fail(`invalid execution mode for ${command.method}`);
    if (!idempotencyModes.has(command.idempotency)) fail(`invalid idempotency mode for ${command.method}`);
    if (!confirmationModes.has(command.confirmation)) fail(`invalid confirmation mode for ${command.method}`);
    if (typeof command.permission !== 'string' || command.permission.length === 0) {
      fail(`missing permission for ${command.method}`);
    }
    if (!Array.isArray(command.errorCodes) || command.errorCodes.length === 0) {
      fail(`missing error codes for ${command.method}`);
    }
    const params = definitionFor(command.paramsSchema);
    definitionFor(command.resultSchema);
    if (params.type !== 'object' || params.additionalProperties !== false) {
      fail(`command params must be a closed object: ${command.method}`);
    }
    if (!Array.isArray(command.cli) || command.cli.length === 0) {
      fail(`CLI path is required for ${command.method}`);
    }
    const cliPath = command.cli.join(' ');
    if (cliPaths.has(cliPath)) fail(`duplicate CLI path: ${cliPath}`);
    cliPaths.add(cliPath);
    if (command.classification === 'mutation' && command.idempotency === 'none') {
      fail(`mutation must declare idempotency for ${command.method}`);
    }
  }
}

function tsType(schema) {
  if (!schema || Object.keys(schema).length === 0) return 'unknown';
  if (schema.$ref) return definitionName(schema.$ref);
  if (Object.prototype.hasOwnProperty.call(schema, 'const')) return JSON.stringify(schema.const);
  if (Array.isArray(schema.enum)) return schema.enum.map((value) => JSON.stringify(value)).join(' | ');
  if (Array.isArray(schema.oneOf)) return schema.oneOf.map(tsType).join(' | ');
  switch (schema.type) {
    case 'string': return 'string';
    case 'integer':
    case 'number': return 'number';
    case 'boolean': return 'boolean';
    case 'array': return `Array<${tsType(schema.items ?? {})}>`;
    case 'object': {
      const properties = schema.properties ?? {};
      if (Object.keys(properties).length === 0) return 'Record<string, unknown>';
      const required = new Set(schema.required ?? []);
      const fields = Object.entries(properties).map(([name, value]) => (
        `${JSON.stringify(name)}${required.has(name) ? '' : '?'}: ${tsType(value)};`
      ));
      return `{ ${fields.join(' ')} }`;
    }
    default: return 'unknown';
  }
}

function generateTypeScript() {
  const rows = [
    '// @generated by packages/contracts/scripts/generate.mjs; DO NOT EDIT.',
    '',
  ];
  for (const [name, schema] of Object.entries(definitions)) {
    if (schema.type === 'object' && schema.properties) {
      const required = new Set(schema.required ?? []);
      rows.push(`export interface ${name} {`);
      for (const [property, propertySchema] of Object.entries(schema.properties)) {
        rows.push(`  ${JSON.stringify(property)}${required.has(property) ? '' : '?'}: ${tsType(propertySchema)};`);
      }
      rows.push('}', '');
    } else {
      rows.push(`export type ${name} = ${tsType(schema)};`, '');
    }
  }

  const commandNames = catalog.commands.map((command) => JSON.stringify(command.method)).join(' | ');
  rows.push(`export type CommandName = ${commandNames};`, '');
  rows.push('export interface CommandParams {');
  for (const command of catalog.commands) {
    rows.push(`  ${JSON.stringify(command.method)}: ${definitionName(command.paramsSchema)};`);
  }
  rows.push('}', '', 'export interface CommandResult {');
  for (const command of catalog.commands) {
    rows.push(`  ${JSON.stringify(command.method)}: ${definitionName(command.resultSchema)};`);
  }
  rows.push('}', '');
  rows.push(`export const CORE_SCHEMA: Record<string, unknown> = ${JSON.stringify(core, null, 2)};`, '');
  rows.push(`export const COMMAND_SPECS = ${JSON.stringify(catalog.commands, null, 2)} as const;`, '');
  rows.push(
    'export interface RpcTransport {',
    '  request(request: JsonRpcRequest): Promise<JsonRpcResponse>;',
    '}',
    '',
    'export interface RequestOptions {',
    '  requestId: JsonRpcId;',
    '  sessionGeneration: string;',
    '  deadlineMs: number;',
    '  idempotencyKey?: string;',
    '}',
    '',
    'export class MintcatRpcError extends Error {',
    '  public constructor(public readonly appError: AppError) {',
    '    super(appError.messageKey);',
    "    this.name = 'MintcatRpcError';",
    '  }',
    '}',
    '',
    'export class MintcatClient {',
    '  public constructor(private readonly transport: RpcTransport) {}',
    '',
    '  public async call<M extends CommandName>(',
    '    method: M,',
    '    params: CommandParams[M],',
    '    options: RequestOptions,',
    '  ): Promise<CommandResult[M]> {',
    '    const spec = COMMAND_SPECS.find((candidate) => candidate.method === method);',
    "    if (!spec) throw new Error(`Unknown generated command: ${method}`);",
    "    if (spec.idempotency === 'required' && !options.idempotencyKey?.trim()) {",
    "      throw new Error(`Command ${method} requires an idempotency key`);",
    '    }',
    '    const response = await this.transport.request({',
    "      jsonrpc: '2.0',",
    '      id: options.requestId,',
    '      method,',
    '      contractVersion: spec.contractVersion,',
    '      sessionGeneration: options.sessionGeneration,',
    '      deadlineMs: options.deadlineMs,',
    '      ...(options.idempotencyKey ? {idempotencyKey: options.idempotencyKey} : {}),',
    '      params: params as unknown as Record<string, unknown>,',
    '    });',
    '    if (response.error) throw new MintcatRpcError(response.error);',
    "    if (!Object.prototype.hasOwnProperty.call(response, 'result')) throw new Error('RPC response has neither result nor error');",
    '    return response.result as CommandResult[M];',
    '  }',
    '}',
    '',
  );
  return `${rows.join('\n')}\n`;
}

function snakeCase(value) {
  return value
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
    .replace(/[^A-Za-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '')
    .toLowerCase();
}

const RUST_KEYWORDS = new Set([
  'as', 'async', 'await', 'break', 'const', 'continue', 'crate', 'dyn', 'else', 'enum',
  'extern', 'false', 'fn', 'for', 'if', 'impl', 'in', 'let', 'loop', 'match', 'mod',
  'move', 'mut', 'pub', 'ref', 'return', 'self', 'static', 'struct', 'super', 'trait',
  'true', 'type', 'unsafe', 'use', 'where', 'while',
]);

function rustFieldName(value) {
  const fieldName = snakeCase(value);
  return RUST_KEYWORDS.has(fieldName) ? `r#${fieldName}` : fieldName;
}

function pascalCase(value) {
  const words = value.split(/[^A-Za-z0-9]+/).filter(Boolean);
  return words.map((word) => word.charAt(0).toUpperCase() + word.slice(1)).join('');
}

function rustType(schema) {
  if (!schema || Object.keys(schema).length === 0) return 'serde_json::Value';
  if (schema.$ref) return definitionName(schema.$ref);
  if (Array.isArray(schema.oneOf)) return 'serde_json::Value';
  switch (schema.type) {
    case 'string': return 'String';
    case 'integer': return schema.minimum !== undefined && schema.minimum >= 0 ? 'u64' : 'i64';
    case 'number': return 'f64';
    case 'boolean': return 'bool';
    case 'array': return `Vec<${rustType(schema.items ?? {})}>`;
    case 'object': return 'serde_json::Value';
    default: return 'serde_json::Value';
  }
}

function rustTypeSupportsEq(schema) {
  if (schema?.$ref) {
    const target = definitions[definitionName(schema.$ref)];
    if (target.type === 'object' && target.properties) return false;
    if (Array.isArray(target.oneOf)) return target.oneOf.every(rustTypeSupportsEq);
    return rustTypeSupportsEq(target);
  }
  if (schema?.type === 'array') return rustTypeSupportsEq(schema.items);
  return schema?.type !== 'number';
}

function rustEnumVariant(value) {
  const variant = pascalCase(String(value));
  return /^[0-9]/.test(variant) ? `Value${variant}` : variant;
}

function generateRustDefinition(name, schema) {
  const rows = [];
  if (schema.type === 'object' && schema.properties) {
    const required = new Set(schema.required ?? []);
    rows.push('#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]');
    rows.push('#[serde(rename_all = "camelCase", deny_unknown_fields)]');
    rows.push(`pub struct ${name} {`);
    for (const [property, propertySchema] of Object.entries(schema.properties)) {
      const fieldName = rustFieldName(property);
      const type = rustType(propertySchema);
      if (!required.has(property)) {
        rows.push('    #[serde(default, skip_serializing_if = "Option::is_none")]');
        rows.push(`    pub ${fieldName}: Option<${type}>,`);
      } else {
        rows.push(`    pub ${fieldName}: ${type},`);
      }
    }
    rows.push('}', '');
    return rows;
  }
  if (Array.isArray(schema.enum) && schema.type === 'string') {
    rows.push('#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]');
    rows.push(`pub enum ${name} {`);
    for (const value of schema.enum) {
      rows.push(`    #[serde(rename = ${JSON.stringify(value)})]`);
      rows.push(`    ${rustEnumVariant(value)},`);
    }
    rows.push('}', '');
    return rows;
  }
  if (Array.isArray(schema.oneOf)) {
    const equality = schema.oneOf.every(rustTypeSupportsEq) ? ', Eq' : '';
    rows.push(`#[derive(Debug, Clone, Serialize, Deserialize, PartialEq${equality})]`);
    rows.push('#[serde(untagged)]');
    rows.push(`pub enum ${name} {`);
    const seen = new Set();
    for (const option of schema.oneOf) {
      let variant = option.type === 'integer' ? 'Integer' : option.type === 'string' ? 'String' : 'Value';
      let suffix = 2;
      while (seen.has(variant)) variant = `${variant}${suffix++}`;
      seen.add(variant);
      rows.push(`    ${variant}(${rustType(option)}),`);
    }
    rows.push('}', '');
    return rows;
  }
  rows.push(`pub type ${name} = ${rustType(schema)};`, '');
  return rows;
}

function commandVariant(method) {
  return pascalCase(method);
}

function generateRust() {
  const rows = [
    '// @generated by packages/contracts/scripts/generate.mjs; DO NOT EDIT.',
    'use serde::{Deserialize, Serialize};',
    '',
  ];
  for (const [name, schema] of Object.entries(definitions)) {
    rows.push(...generateRustDefinition(name, schema));
  }
  rows.push('#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]');
  rows.push('pub enum CommandName {');
  for (const command of catalog.commands) {
    rows.push(`    #[serde(rename = ${JSON.stringify(command.method)})]`);
    rows.push(`    ${commandVariant(command.method)},`);
  }
  rows.push('}', '');
  rows.push('#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]');
  rows.push('#[serde(tag = "method", content = "params")]');
  rows.push('pub enum ApplicationCommand {');
  for (const command of catalog.commands) {
    rows.push(`    #[serde(rename = ${JSON.stringify(command.method)})]`);
    rows.push(`    ${commandVariant(command.method)}(${definitionName(command.paramsSchema)}),`);
  }
  rows.push('}', '');
  rows.push('#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]');
  rows.push('#[serde(tag = "method", content = "result")]');
  rows.push('pub enum ApplicationCommandResult {');
  for (const command of catalog.commands) {
    rows.push(`    #[serde(rename = ${JSON.stringify(command.method)})]`);
    rows.push(`    ${commandVariant(command.method)}(${definitionName(command.resultSchema)}),`);
  }
  rows.push('}', '');
  rows.push(
    '#[derive(Debug, Clone, Copy, PartialEq, Eq)]',
    'pub struct CommandDescriptor {',
    "    pub method: &'static str,",
    '    pub contract_version: u64,',
    "    pub params_schema: &'static str,",
    "    pub result_schema: &'static str,",
    "    pub classification: &'static str,",
    "    pub execution: &'static str,",
    "    pub permission: &'static str,",
    "    pub idempotency: &'static str,",
    "    pub confirmation: &'static str,",
    "    pub error_codes: &'static [&'static str],",
    "    pub cli: &'static [&'static str],",
    '    pub agent_exposed: bool,',
    '}',
    '',
    'pub const COMMAND_SPECS: &[CommandDescriptor] = &[',
  );
  for (const command of catalog.commands) {
    rows.push('    CommandDescriptor {');
    rows.push(`        method: ${JSON.stringify(command.method)},`);
    rows.push(`        contract_version: ${command.contractVersion},`);
    rows.push(`        params_schema: ${JSON.stringify(command.paramsSchema)},`);
    rows.push(`        result_schema: ${JSON.stringify(command.resultSchema)},`);
    rows.push(`        classification: ${JSON.stringify(command.classification)},`);
    rows.push(`        execution: ${JSON.stringify(command.execution)},`);
    rows.push(`        permission: ${JSON.stringify(command.permission)},`);
    rows.push(`        idempotency: ${JSON.stringify(command.idempotency)},`);
    rows.push(`        confirmation: ${JSON.stringify(command.confirmation)},`);
    rows.push(`        error_codes: &[${command.errorCodes.map((value) => JSON.stringify(value)).join(', ')}],`);
    rows.push(`        cli: &[${command.cli.map((value) => JSON.stringify(value)).join(', ')}],`);
    rows.push(`        agent_exposed: ${command.agentExposed},`);
    rows.push('    },');
  }
  rows.push('];', '');
  rows.push('pub fn command_spec(method: &str) -> Option<&\'static CommandDescriptor> {');
  rows.push('    COMMAND_SPECS.iter().find(|spec| spec.method == method)');
  rows.push('}', '');
  rows.push(`pub const CORE_SCHEMA_JSON: &str = r####"${JSON.stringify(core)}"####;`);
  rows.push(`pub const COMMAND_CATALOG_JSON: &str = r####"${JSON.stringify(catalog)}"####;`, '');
  return `${rows.join('\n')}\n`;
}

function formatRust(source) {
  const result = spawnSync('rustfmt', ['--emit', 'stdout', '--edition', '2021'], {
    encoding: 'utf8',
    input: source,
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) fail(`rustfmt failed to start: ${result.error.message}`);
  if (result.status !== 0) fail(`rustfmt failed: ${result.stderr.trim()}`);
  return result.stdout;
}

function inlineSchema(schema, stack = []) {
  if (!schema || typeof schema !== 'object') return schema;
  if (schema.$ref) {
    const name = definitionName(schema.$ref);
    if (stack.includes(name)) fail(`recursive schema cannot be inlined for Agent metadata: ${name}`);
    return inlineSchema(definitions[name], [...stack, name]);
  }
  if (Array.isArray(schema)) return schema.map((value) => inlineSchema(value, stack));
  return Object.fromEntries(
    Object.entries(schema).map(([key, value]) => [key, inlineSchema(value, stack)]),
  );
}

function generateMetadata() {
  const tools = catalog.commands
    .filter((command) => command.agentExposed)
    .map((command) => ({
      type: 'function',
      function: {
        name: command.method.replace(/[^A-Za-z0-9_-]/g, '_'),
        description: command.summary,
        parameters: inlineSchema(definitionFor(command.paramsSchema)),
      },
      xMintcat: {
        command: command.method,
        contractVersion: command.contractVersion,
        classification: command.classification,
        execution: command.execution,
        permission: command.permission,
        idempotency: command.idempotency,
        confirmation: command.confirmation,
      },
    }));
  const cli = catalog.commands.map((command) => ({
    command: command.method,
    path: command.cli,
    contractVersion: command.contractVersion,
    paramsSchema: inlineSchema(definitionFor(command.paramsSchema)),
    resultSchema: command.resultSchema,
    classification: command.classification,
    execution: command.execution,
    permission: command.permission,
    idempotency: command.idempotency,
    confirmation: command.confirmation,
  }));
  return `${JSON.stringify({
    schemaVersion: catalog.schemaVersion,
    contractSetVersion: catalog.contractSetVersion,
    tools,
    cli,
  }, null, 2)}\n`;
}

function writeOrCheck(relativePath, content) {
  const outputPath = path.join(rootDir, relativePath);
  if (checkOnly) {
    if (!existsSync(outputPath) || readFileSync(outputPath, 'utf8') !== content) {
      fail(`generated output is stale: ${relativePath}`);
    }
    return;
  }
  mkdirSync(path.dirname(outputPath), {recursive: true});
  writeFileSync(outputPath, content);
  console.log(`[contracts] generated ${relativePath}`);
}

validateSources();
writeOrCheck('packages/sdk-ts/src/generated.ts', generateTypeScript());
writeOrCheck('packages/sdk-rust/src/generated.rs', formatRust(generateRust()));
writeOrCheck('packages/contracts/generated/command-metadata.json', generateMetadata());
if (checkOnly) console.log('[contracts] generated outputs are current');
