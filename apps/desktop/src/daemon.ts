import { invoke, isTauri } from '@tauri-apps/api/core';
import {
  MintcatRpcError,
  validateCommandResult,
  type CommandName,
  type CommandParams,
  type CommandResult,
  type JsonRpcResponse,
  type SystemDoctorResult,
  type SystemStatusResult,
} from '@mintcat/sdk-ts';

export function hasNativeDaemonConnector(): boolean {
  return isTauri();
}

export async function readDaemonStatus(dataDir: string): Promise<SystemStatusResult> {
  return invokeAndValidate<SystemStatusResult>('daemon_status', 'system.status', dataDir);
}

export async function runDaemonDoctor(dataDir: string): Promise<SystemDoctorResult> {
  return invokeAndValidate<SystemDoctorResult>('daemon_doctor', 'system.doctor', dataDir);
}

export async function callDaemon<M extends CommandName>(
  dataDir: string,
  method: M,
  params: CommandParams[M],
  idempotencyKey?: string,
): Promise<CommandResult[M]> {
  const response = await invoke<unknown>('daemon_call', {
    dataDir,
    method,
    params,
    idempotencyKey,
  });
  return validateResponse<CommandResult[M]>(response, method);
}

export async function requestApplication<Result>(
  dataDir: string,
  method: string,
  params: Record<string, unknown>,
  idempotencyKey?: string,
): Promise<Result> {
  return validateResponse<Result>(await invoke<unknown>('daemon_call', {
    dataDir, method, params, idempotencyKey,
  }), method);
}

export async function requestUiApplication<Result>(
  token: string,
  moduleId: string,
  method: string,
  params: object,
  idempotencyKey?: string,
): Promise<Result> {
  // The native host derives the session, environment and digest from its verified
  // registry; module code cannot supply a replacement root or generation.
  const response = await invoke<unknown>('ui_request_generation', { token, moduleId, method, params, idempotencyKey });
  return validateResponse<Result>(response, method);
}

async function invokeAndValidate<Result>(
  command: string,
  method: string,
  dataDir: string,
): Promise<Result> {
  const response = await invoke<unknown>(command, { dataDir });
  return validateResponse<Result>(response, method);
}

function validateResponse<Result>(response: unknown, method: string): Result {
  if (!isJsonRpcResponse(response)) {
    throw new Error('Native daemon connector returned an invalid JSON-RPC envelope');
  }
  if (response.error) {
    throw new MintcatRpcError(response.error);
  }
  if (!Object.prototype.hasOwnProperty.call(response, 'result')) {
    throw new Error('Daemon response contains neither result nor error');
  }
  validateCommandResult(method, 1, response.result);
  return response.result as Result;
}

function isJsonRpcResponse(value: unknown): value is JsonRpcResponse {
  return typeof value === 'object'
    && value !== null
    && 'jsonrpc' in value
    && value.jsonrpc === '2.0'
    && 'id' in value;
}
