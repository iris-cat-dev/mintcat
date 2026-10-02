import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import {
  ContractDecodeError,
  decodeRequest,
  validateCommandResult,
} from './index';

interface BoundaryCase {
  name: string;
  request: unknown;
  ok: boolean;
  errorCode?: string;
}

const cases = JSON.parse(
  readFileSync(new URL('../../../fixtures/contracts/rpc-boundaries.json', import.meta.url), 'utf8'),
) as BoundaryCase[];

describe('generated application contract boundaries', () => {
  for (const boundary of cases) {
    it(boundary.name, () => {
      try {
        const request = decodeRequest(JSON.stringify(boundary.request));
        expect(boundary.ok).toBe(true);
        if (
          typeof boundary.request !== 'object'
          || boundary.request === null
          || !('method' in boundary.request)
        ) {
          throw new Error(`Fixture ${boundary.name} has no method`);
        }
        expect(request.method).toBe(boundary.request.method);
      } catch (error) {
        expect(boundary.ok).toBe(false);
        if (!(error instanceof ContractDecodeError)) throw error;
        expect(error.code).toBe(boundary.errorCode);
      }
    });
  }

  it('validates command results against the generated result schema', () => {
    expect(() => validateCommandResult('mods.setVersion', 1, { jobId: 'job-1' })).not.toThrow();
    expect(() => validateCommandResult('mods.setVersion', 1, { jobId: 1 })).toThrowError(
      expect.objectContaining({ code: 'INVALID_RESULT', path: '$.result.jobId' }),
    );
  });

  it('accepts nullable update recovery reasons and rejects other types', () => {
    for (const recoveryReason of [null, 'explicit recovery required']) {
      expect(() => validateCommandResult('updates.operations', 1, {
        operations: [], recoveryReason, scopeBlockers: {},
      })).not.toThrow();
    }
    for (const recoveryReason of [false, 42, {}]) {
      expect(() => validateCommandResult('updates.operations', 1, {
        operations: [], recoveryReason, scopeBlockers: {},
      })).toThrowError(expect.objectContaining({
        code: 'INVALID_RESULT', path: '$.result.recoveryReason',
      }));
    }
  });


  it('requires both game and profile scope for missing local cleanup', () => {
    expect(() => decodeRequest({
      jsonrpc: '2.0',
      id: 'cleanup-1',
      method: 'mods.cleanupMissing',
      sessionGeneration: 'generation-1',
      contractVersion: 1,
      deadlineMs: 5_000,
      idempotencyKey: 'cleanup',
      params: {
        gameId: 'game:skyrim',
        profileId: 'profile:main',
        expectedContentRevision: 4,
      },
    })).not.toThrow();
    expect(() => decodeRequest({
      jsonrpc: '2.0',
      id: 'cleanup-2',
      method: 'mods.cleanupMissing',
      contractVersion: 1,
      deadlineMs: 5_000,
      sessionGeneration: 'generation-1',
      idempotencyKey: 'cleanup',
      params: {
        profileId: 'profile:main',
        expectedContentRevision: 4,
      },
    })).toThrowError(expect.objectContaining({
      code: 'INVALID_ARGUMENT',
      path: '$.params.gameId',
    }));
  });
});
