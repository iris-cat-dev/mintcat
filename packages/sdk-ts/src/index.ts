export * from './generated';

import {
  COMMAND_SPECS,
  CORE_SCHEMA,
  type CommandName,
  type CommandParams,
  type JsonRpcRequest,
} from './generated';

type JsonObject = Record<string, unknown>;
type CommandSpec = (typeof COMMAND_SPECS)[number];

export class ContractDecodeError extends Error {
  public constructor(
    public readonly code: string,
    public readonly path: string,
    message: string,
  ) {
    super(message);
    this.name = 'ContractDecodeError';
  }
}

export interface ValidatedRequest {
  envelope: JsonRpcRequest;
  method: CommandName;
  params: CommandParams[CommandName];
  spec: CommandSpec;
}

export function decodeRequest(input: string | unknown): ValidatedRequest {
  let value: unknown = input;
  if (typeof input === 'string') {
    try {
      value = JSON.parse(input);
    } catch (error) {
      throw new ContractDecodeError(
        'MALFORMED_JSON',
        '$',
        error instanceof Error ? error.message : String(error),
      );
    }
  }

  try {
    validateDefinition('JsonRpcRequest', value, '$');
  } catch (error) {
    throw asContractError(error, 'INVALID_REQUEST');
  }

  const envelope = value as JsonRpcRequest;
  const spec = COMMAND_SPECS.find((candidate) => candidate.method === envelope.method);
  if (!spec) {
    throw new ContractDecodeError(
      'UNKNOWN_METHOD',
      '$.method',
      `Unknown method ${envelope.method}`,
    );
  }
  if (envelope.contractVersion !== spec.contractVersion) {
    throw new ContractDecodeError(
      'INCOMPATIBLE_CONTRACT',
      '$.contractVersion',
      `Method ${envelope.method} requires contract version ${spec.contractVersion}, got ${envelope.contractVersion}`,
    );
  }
  if (spec.idempotency === 'required' && !envelope.idempotencyKey?.trim()) {
    throw new ContractDecodeError(
      'INVALID_REQUEST',
      '$.idempotencyKey',
      `Method ${envelope.method} requires an idempotency key`,
    );
  }

  try {
    validateReference(spec.paramsSchema, envelope.params, '$.params');
  } catch (error) {
    throw asContractError(error, 'INVALID_ARGUMENT');
  }

  return {
    envelope,
    method: spec.method,
    params: envelope.params as CommandParams[CommandName],
    spec,
  };
}

export function validateCommandResult(
  method: string,
  contractVersion: number,
  result: unknown,
): void {
  const spec = COMMAND_SPECS.find((candidate) => candidate.method === method);
  if (!spec) {
    throw new ContractDecodeError('UNKNOWN_METHOD', '$.method', `Unknown method ${method}`);
  }
  if (contractVersion !== spec.contractVersion) {
    throw new ContractDecodeError(
      'INCOMPATIBLE_CONTRACT',
      '$.contractVersion',
      `Method ${method} requires contract version ${spec.contractVersion}, got ${contractVersion}`,
    );
  }
  try {
    validateReference(spec.resultSchema, result, '$.result');
  } catch (error) {
    throw asContractError(error, 'INVALID_RESULT');
  }
}

class SchemaViolation extends Error {
  public constructor(public readonly path: string, message: string) {
    super(message);
    this.name = 'SchemaViolation';
  }
}

function asContractError(error: unknown, code: string): ContractDecodeError {
  if (error instanceof SchemaViolation) {
    return new ContractDecodeError(code, error.path, error.message);
  }
  throw error;
}

function validateDefinition(name: string, value: unknown, path: string): void {
  validateReference(`#/$defs/${name}`, value, path);
}

function validateReference(reference: string, value: unknown, path: string): void {
  const prefix = '#/$defs/';
  if (!reference.startsWith(prefix)) {
    throw new SchemaViolation(path, `Unsupported schema reference ${reference}`);
  }
  const definitions = asObject(CORE_SCHEMA.$defs, '$.$defs');
  const name = reference.slice(prefix.length);
  const schema = definitions[name];
  if (schema === undefined) {
    throw new SchemaViolation(path, `Missing schema definition ${name}`);
  }
  validateSchema(schema, value, path);
}

function validateSchema(schemaValue: unknown, value: unknown, path: string): void {
  const schema = asObject(schemaValue, path);
  if (Object.keys(schema).length === 0) return;

  const reference = schema.$ref;
  if (typeof reference === 'string') {
    validateReference(reference, value, path);
    return;
  }
  if (Object.prototype.hasOwnProperty.call(schema, 'const') && !Object.is(schema.const, value)) {
    throw new SchemaViolation(path, `Expected constant ${JSON.stringify(schema.const)}`);
  }
  if (Array.isArray(schema.enum) && !schema.enum.some((candidate) => Object.is(candidate, value))) {
    throw new SchemaViolation(path, 'Value is not in the allowed enum');
  }
  if (Array.isArray(schema.oneOf)) {
    const matches = schema.oneOf.filter((variant) => schemaMatches(variant, value, path)).length;
    if (matches !== 1) {
      throw new SchemaViolation(path, 'Value must match exactly one schema variant');
    }
    return;
  }

  switch (schema.type) {
    case 'object':
      validateObject(schema, value, path);
      return;
    case 'array':
      validateArray(schema, value, path);
      return;
    case 'string':
      validateString(schema, value, path);
      return;
    case 'integer':
      validateInteger(schema, value, path);
      return;
    case 'number':
      if (typeof value !== 'number' || !Number.isFinite(value)) throw typeError(path, 'number');
      validateNumberBounds(schema, value, path);
      return;
    case 'boolean':
      if (typeof value !== 'boolean') throw typeError(path, 'boolean');
      return;
    case 'null':
      if (value !== null) throw typeError(path, 'null');
      return;
    case undefined:
      return;
    default:
      throw new SchemaViolation(path, `Unsupported schema type ${String(schema.type)}`);
  }
}

function validateObject(schema: JsonObject, value: unknown, path: string): void {
  if (!isObject(value)) throw typeError(path, 'object');
  const required = schema.required;
  if (Array.isArray(required)) {
    for (const field of required) {
      if (typeof field === 'string' && !Object.prototype.hasOwnProperty.call(value, field)) {
        throw new SchemaViolation(childPath(path, field), 'Required field is missing');
      }
    }
  }
  const properties = isObject(schema.properties) ? schema.properties : {};
  for (const [field, fieldValue] of Object.entries(value)) {
    if (Object.prototype.hasOwnProperty.call(properties, field)) {
      validateSchema(properties[field], fieldValue, childPath(path, field));
    } else if (schema.additionalProperties === false) {
      throw new SchemaViolation(childPath(path, field), 'Unknown field is not allowed');
    }
  }
}

function validateArray(schema: JsonObject, value: unknown, path: string): void {
  if (!Array.isArray(value)) throw typeError(path, 'array');
  if (typeof schema.minItems === 'number' && value.length < schema.minItems) {
    throw new SchemaViolation(path, `Array requires at least ${schema.minItems} items`);
  }
  if (schema.items !== undefined) {
    value.forEach((item, index) => validateSchema(schema.items, item, `${path}[${index}]`));
  }
}

function validateString(schema: JsonObject, value: unknown, path: string): void {
  if (typeof value !== 'string') throw typeError(path, 'string');
  const length = Array.from(value).length;
  if (typeof schema.minLength === 'number' && length < schema.minLength) {
    throw new SchemaViolation(path, `String is shorter than ${schema.minLength}`);
  }
  if (typeof schema.maxLength === 'number' && length > schema.maxLength) {
    throw new SchemaViolation(path, `String is longer than ${schema.maxLength}`);
  }
  if (typeof schema.pattern === 'string' && !new RegExp(schema.pattern, 'u').test(value)) {
    throw new SchemaViolation(path, `String does not match ${schema.pattern}`);
  }
}

function validateInteger(schema: JsonObject, value: unknown, path: string): void {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) throw typeError(path, 'integer');
  validateNumberBounds(schema, value, path);
}

function validateNumberBounds(schema: JsonObject, value: number, path: string): void {
  if (typeof schema.minimum === 'number' && value < schema.minimum) {
    throw new SchemaViolation(path, `Number is below ${schema.minimum}`);
  }
  if (typeof schema.maximum === 'number' && value > schema.maximum) {
    throw new SchemaViolation(path, `Number is above ${schema.maximum}`);
  }
}

function schemaMatches(schema: unknown, value: unknown, path: string): boolean {
  try {
    validateSchema(schema, value, path);
    return true;
  } catch (error) {
    if (error instanceof SchemaViolation) return false;
    throw error;
  }
}

function asObject(value: unknown, path: string): JsonObject {
  if (!isObject(value)) throw typeError(path, 'object');
  return value;
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function typeError(path: string, expected: string): SchemaViolation {
  return new SchemaViolation(path, `Expected ${expected}`);
}

function childPath(path: string, field: string): string {
  return `${path}.${field}`;
}
