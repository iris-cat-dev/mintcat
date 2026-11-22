// @vitest-environment jsdom
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import './ui-runtime';
import { getSharedRuntime } from '@mintcat/sdk-ui';
import { callDaemon } from './daemon';
import { mountDesktop } from './bootstrap';

vi.mock('./daemon', () => ({
  hasNativeDaemonConnector: () => true,
  callDaemon: vi.fn(), requestApplication: vi.fn(), readDaemonStatus: vi.fn(), runDaemonDoctor: vi.fn(),
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ label: 'main', listen: async () => () => {} }),
}));
vi.mock('./window-surface', () => ({ initializeWindowSurface: () => () => {} }));
// Exercise rescue with the actual workbench chunk unavailable, rather than relying on its initialization.
vi.mock('./workbench', () => { throw new Error('Workbench chunk unavailable'); });
const { React } = getSharedRuntime();
beforeAll(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
  Object.defineProperty(window, 'matchMedia', { configurable: true, value: (query: string) => ({ matches: false, media: query, onchange: null, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {}, dispatchEvent: () => false }) });
});
afterAll(() => vi.unstubAllGlobals());
beforeEach(() => { vi.mocked(callDaemon).mockReset(); });
afterEach(async () => {
  await React.act(async () => { window.dispatchEvent(new Event('pagehide')); });
  vi.restoreAllMocks(); document.body.replaceChildren();
});

function button(root: HTMLElement, label: string): HTMLButtonElement {
  const result = [...root.querySelectorAll('button')].find(item => item.textContent === label);
  if (!result) throw new Error(`Missing recovery control ${label}`);
  return result;
}

describe('independent desktop recovery', () => {
  it('reads actual operations and starts only a human-selected confirmed local recovery', async () => {
    let recovering = false;
    vi.mocked(callDaemon).mockImplementation(async (directory, method, params) => {
      if (method === 'updates.operations') return { operations: [
        { operationId: 'first-install', scopeId: 'game:drg', phase: 'confirmed', previous: null },
        { operationId: 'draft-update', scopeId: 'game:rogue-core', phase: 'probation', previous: { generationId: 'draft', confirmed: false } },
        { operationId: 'confirmed-update', scopeId: 'game:drg', phase: 'confirmed', previous: { generationId: 'local-pinned-v1', revision: 5, confirmed: true, newWrites: false } },
      ], recoveryReason: null, scopeBlockers: {} };
      if (method === 'updates.rollback' && 'operationId' in params && params.operationId === 'confirmed-update') {
        recovering = true; return { jobId: 'local-recovery-job' };
      }
      throw new Error(`Invalid recovery request ${directory} ${method}`);
    });
    const root = document.createElement('div'); document.body.append(root);
    await React.act(async () => { await mountDesktop(root, '/isolated-recovery'); });
    await React.act(async () => { button(root, 'Safe mode').click(); });
    expect(root.textContent).toContain('Safe mode: workbench closed');
    await React.act(async () => { button(root, 'Local confirmed recovery sets').click(); });
    const recoveryButtons = [...root.querySelectorAll('button')].filter(item => item.textContent?.startsWith('Restore local confirmed'));
    expect(recoveryButtons.map(item => item.textContent)).toEqual(['Restore local confirmed game:drg: local-pinned-v1']);
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    await React.act(async () => { recoveryButtons[0].click(); }); expect(recovering).toBe(false);
    confirm.mockReturnValue(true);
    await React.act(async () => { recoveryButtons[0].click(); });
    expect(recovering).toBe(true);
    expect(root.textContent).toContain('Recovery task local-recovery-job started in daemon');
    expect(recoveryButtons[0].disabled).toBe(false);
  });
});
