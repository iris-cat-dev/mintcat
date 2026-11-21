// @vitest-environment jsdom
import { runInNewContext } from 'node:vm';
import { describe, expect, it } from 'vitest';
import { presentUiError, renderUiError } from './error-presentation';

const unauthorized = {
  code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', requestId: 'request-auth', retryable: true,
  details: { reason: 'credential_missing_or_invalid', message: 'worker returned SOURCE_UNAUTHORIZED: source.worker.failed' },
};

describe('human error presentation boundaries', () => {
  it('recognizes errors from another SDK realm without using class identity', () => {
    const foreign: unknown = runInNewContext('Object.assign(new Error("source.operation"), { appError: payload })', { payload: unauthorized });
    const error = presentUiError(foreign, 'search');
    expect(error.kind).toBe('source-unauthorized');
    expect(error.retryable).toBe(false);
    expect(error.title + error.description).not.toContain('source.operation');
    expect(error.diagnostics.join('\n')).toContain('request-auth');
  });
  it('recognizes native serialized AppError rejections and code-key control rejections', () => {
    expect(presentUiError(JSON.stringify(unauthorized), 'search').retryable).toBe(false);
    expect(presentUiError({ error: unauthorized }, 'search').retryable).toBe(false);
    const rateLimited = presentUiError('SOURCE_RATE_LIMITED: source.worker.failed', 'search');
    expect(rateLimited.retryable).toBe(true);
    expect(rateLimited.title + rateLimited.description).not.toContain('source.worker.failed');
  });
  it('prefers safe configuration reasons over a generic transport code and unknown worker text', () => {
    const error = presentUiError({ code: 'SOURCE_UNAVAILABLE', messageKey: 'source.operation', requestId: 'config-request', details: { reason: 'source_configuration', message: 'worker returned INVALID_ARGUMENT: source.worker.failed' } }, 'search');
    expect(error.retryable).toBe(false);
    expect(error.title + error.description).not.toContain('worker');
  });
  it('keeps unknown internals out of headlines and preserves binding-specific guidance', () => {
    const error = presentUiError(new Error('unexpected private worker panic at native.rs:42'));
    expect(error.description).not.toContain('native.rs');
    expect(error.diagnostics.join('\n')).toContain('native.rs:42');
    expect(presentUiError({ code: 'NOT_FOUND', messageKey: 'library.error', details: {} }, 'bindings').retryable).toBe(false);
    expect(presentUiError({ code: 'NOT_FOUND', messageKey: 'library.error', details: {} }, 'add').retryable).toBe(false);
  });
  it('starts diagnostics collapsed and excludes stacks, credential fields and credential-bearing URLs', () => {
    const root = document.createElement('div');
    const error = presentUiError({ ...unauthorized, details: {
      reason: 'credential_missing_or_invalid', message: 'request failed https://user:secret@example.org/v1?api_key=private',
      credential: 'do-not-display', headers: { authorization: 'Bearer also-private' }, stack: 'hidden stack',
    } }, 'search');
    renderUiError(root, error);
    expect(root.querySelector('details')?.open).toBe(false);
    expect(root.textContent).not.toMatch(/secret|private|do-not-display|hidden stack|Bearer|example\.org/);
    root.querySelector('details')!.open = true;
    expect(root.querySelector('pre')?.textContent).toContain('SOURCE_UNAUTHORIZED');
    expect(root.querySelector('pre')?.textContent).toContain('request-auth');
  });
  it('removes complete URLs even when no recognizable credential keyword is present', () => {
    const error = presentUiError(new Error('connection failed https://user:opaque-value@example.org/resource?session=opaque'));
    expect(error.diagnostics.join('\n')).not.toContain('opaque');
    expect(error.diagnostics.join('\n')).not.toMatch(/https?:\/\//);
  });
  it('treats missing cloud configuration as non-retryable without exposing operator details', () => {
    const error = presentUiError({ code: 'CLOUD_PREREQUISITE', messageKey: 'backup.cloud.prerequisite', retryable: true,
      details: { reason: 'https://user:opaque-value@example.org/v1/backups?session=opaque' } });
    expect(error.retryable).toBe(false);
    expect(error.title + error.description).not.toMatch(/opaque|example\.org|https?:\/\//);
    expect(error.diagnostics.join('\n')).not.toMatch(/opaque|example\.org|https?:\/\//);
  });
  it('presents credential validation and keychain availability errors clearly without leaking secrets', () => {
    const invalid = presentUiError({
      code: 'INVALID_ARGUMENT',
      messageKey: 'source.credential_format',
      requestId: 'cred-fmt',
      details: { reason: 'credential_invalid_format' },
    }, 'search');
    expect(invalid.kind).toBe('source-credential-invalid');
    expect(invalid.title).toBe('凭据格式无效');
    expect(invalid.retryable).toBe(false);
    expect(invalid.diagnostics.join('\n')).toContain('凭据包含空白或无效字符');

    const vault = presentUiError({
      code: 'SOURCE_UNAVAILABLE',
      messageKey: 'source.credential_vault',
      requestId: 'cred-vault',
      details: { reason: 'credential_vault_unavailable' },
    }, 'host');
    expect(vault.kind).toBe('source-unavailable');
    expect(vault.title).toBe('无法访问系统凭据库');
    expect(vault.retryable).toBe(true);
    expect(vault.diagnostics.join('\n')).toContain('系统凭据库不可用');
  });
  it('explains recovery-required frozen snapshot errors in plain language', () => {
    const stale = presentUiError({
      code: 'RECOVERY_REQUIRED',
      messageKey: 'source.frozen_configuration_unavailable',
      requestId: 'job-stale',
      details: { reason: 'source snapshot identity mismatch' },
    }, 'host');
    expect(stale.title).toBe('旧版未完成任务已失效');
    expect(stale.description).toContain('移除这条记录');
    expect(stale.retryable).toBe(false);
  });
});
