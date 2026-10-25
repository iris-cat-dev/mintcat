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
    expect(error.title).toBe('来源授权失败');
    expect(error.description).toContain('缺少有效凭据');
    expect(error.description).toContain('手动切换');
    expect(error.retryable).toBe(false);
    expect(error.title + error.description).not.toContain('source.operation');
    expect(error.diagnostics).toContain('请求标识：request-auth');
  });
  it('recognizes native serialized AppError rejections and code-key control rejections', () => {
    expect(presentUiError(JSON.stringify(unauthorized), 'search').title).toBe('来源授权失败');
    expect(presentUiError({ error: unauthorized }, 'search').title).toBe('来源授权失败');
    expect(presentUiError('SOURCE_RATE_LIMITED: source.worker.failed', 'search')).toMatchObject({ title: '来源请求过于频繁', retryable: true });
  });
  it('prefers safe configuration reasons over a generic transport code and unknown worker text', () => {
    const error = presentUiError({ code: 'SOURCE_UNAVAILABLE', messageKey: 'source.operation', requestId: 'config-request', details: { reason: 'source_configuration', message: 'worker returned INVALID_ARGUMENT: source.worker.failed' } }, 'search');
    expect(error).toMatchObject({ title: '来源配置有误', retryable: false });
    expect(error.title + error.description).not.toContain('worker');
  });
  it('keeps unknown internals out of headlines and preserves binding-specific guidance', () => {
    const error = presentUiError(new Error('unexpected private worker panic at native.rs:42'));
    expect(error.title).toBe('操作未完成');
    expect(error.description).not.toContain('native.rs');
    expect(error.diagnostics).toContain('技术说明：unexpected private worker panic at native.rs:42');
    expect(presentUiError({ code: 'NOT_FOUND', messageKey: 'library.error', details: {} }, 'bindings').description).toContain('真实安装目录');
    expect(presentUiError({ code: 'NOT_FOUND', messageKey: 'library.error', details: {} }, 'add').title).toBe('目标配置不可用');
  });
  it('starts diagnostics collapsed and excludes stacks, credential fields and credential-bearing URLs', () => {
    const root = document.createElement('div');
    const error = presentUiError({ ...unauthorized, details: {
      reason: 'credential_missing_or_invalid', message: 'request failed https://user:secret@example.org/v1?api_key=private',
      credential: 'do-not-display', headers: { authorization: 'Bearer also-private' }, stack: 'hidden stack',
    } }, 'search');
    renderUiError(root, error);
    expect(root.querySelector('details')?.open).toBe(false);
    expect(root.querySelector('summary')?.textContent).toBe('技术详情');
    expect(root.querySelector('strong')?.textContent).toBe('来源授权失败');
    expect(root.textContent).not.toMatch(/secret|private|do-not-display|hidden stack|Bearer|example\.org/);
    root.querySelector('details')!.open = true;
    expect(root.querySelector('pre')?.textContent).toContain('错误码：SOURCE_UNAUTHORIZED');
    expect(root.querySelector('pre')?.textContent).toContain('请求标识：request-auth');
  });
  it('removes complete URLs even when no recognizable credential keyword is present', () => {
    const error = presentUiError(new Error('connection failed https://user:opaque-value@example.org/resource?session=opaque'));
    expect(error.diagnostics.join('\n')).not.toContain('opaque');
    expect(error.diagnostics.join('\n')).toContain('[地址已隐藏]');
  });
});
