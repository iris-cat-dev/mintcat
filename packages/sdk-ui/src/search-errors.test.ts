// @vitest-environment jsdom
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import '../../../apps/desktop/src/ui-runtime';
import { mountSearch } from '../../../modules/ui-search/ui';
import { createOnlineModJobs, getSharedRuntime, kit, QueryCache, SessionStore } from './index';
import type { Disposable, GameModuleList, UiContext } from './index';
import type { CommandName, CommandParams, CommandResult } from '@mintcat/sdk-ts';

const { React, flushSync } = getSharedRuntime();
let page: Disposable | undefined;
beforeAll(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  Object.defineProperty(window, 'matchMedia', { configurable: true, value: (query: string) => ({ matches: false, media: query, onchange: null, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {}, dispatchEvent: () => false }) });
});
afterAll(() => vi.unstubAllGlobals());
afterEach(() => { page?.dispose(); page = undefined; document.body.replaceChildren(); });

const modules: GameModuleList = { generationId: 'signed-generation', modules: [
  { moduleId: 'game-example', enabled: true, game: { gameId: 'example', displayName: '示例游戏', descriptor: {
    gameId: 'example', displayName: '示例游戏', settingsSchemaVersion: 1, settingsSchema: { type: 'object', properties: {} }, variants: [],
    sourceBindings: [{ sourceId: 'modio', remoteGameKey: '2475' }, { sourceId: 'modcat', remoteGameKey: 'example' }, { sourceId: 'disabled', remoteGameKey: 'example' }],
  } } },
] };
interface MockSourceState {
  sourceId: 'modio' | 'modcat' | 'nexusmods';
  credentialConfigured?: boolean;
}
function contextFor(
  search: (params: CommandParams['sources.search']) => Promise<CommandResult['sources.search']>,
  games?: () => Promise<CommandResult['games.list']>,
  currentModules = modules,
  sources: Array<MockSourceState['sourceId'] | MockSourceState> = ['modio', 'modcat'],
  credentialHandlers?: {
    set?: (params: CommandParams['sources.credentials.set'], idempotencyKey?: string) => Promise<CommandResult['sources.credentials.set']>;
    clear?: (params: CommandParams['sources.credentials.clear'], idempotencyKey?: string) => Promise<CommandResult['sources.credentials.clear']>;
  },
) {
  const hostFailure = vi.fn();
  const configured: MockSourceState[] = sources.map(item => typeof item === 'string' ? { sourceId: item, credentialConfigured: false } : { ...item });
  const context: UiContext = {
    moduleId: 'mintcat.ui.search', signal: new AbortController().signal, kit,
    onlineModJobs: createOnlineModJobs(new SessionStore()),
    own: value => value, onInventoryChanged: () => ({ dispose() {} }),
    async call<M extends CommandName>(method: M, params: CommandParams[M], idempotencyKey?: string): Promise<CommandResult[M]> {
      if (method === 'games.list') return (games ? await games() : { items: [] }) as CommandResult[M];
      if (method === 'sources.list') return { sources: configured.map(item => ({ ...item })) } as CommandResult[M];
      if (method === 'sources.search') return await search(params as CommandParams['sources.search']) as CommandResult[M];
      if (method === 'sources.credentials.set') {
        const input = params as CommandParams['sources.credentials.set'];
        const result = credentialHandlers?.set
          ? await credentialHandlers.set(input, idempotencyKey)
          : { sourceId: input.sourceId, credentialConfigured: true };
        const entry = configured.find(item => item.sourceId === input.sourceId);
        if (entry) entry.credentialConfigured = result.credentialConfigured;
        return result as CommandResult[M];
      }
      if (method === 'sources.credentials.clear') {
        const input = params as CommandParams['sources.credentials.clear'];
        const result = credentialHandlers?.clear
          ? await credentialHandlers.clear(input, idempotencyKey)
          : { sourceId: input.sourceId, credentialConfigured: false };
        const entry = configured.find(item => item.sourceId === input.sourceId);
        if (entry) entry.credentialConfigured = result.credentialConfigured;
        return result as CommandResult[M];
      }
      throw new Error(`Unexpected command ${method}`);
    },
    request<T>(): Promise<T> { return Promise.resolve(currentModules as unknown as T); },
    query: (_key, load) => load(), invalidate() {}, state: (_key, _version, fallback) => fallback,
    saveState() {}, dirty() {}, notify() {},
    guard: action => (...args) => { void Promise.resolve().then(() => action(...args)).catch(hostFailure); },
    interval: () => ({ dispose() {} }),
  };
  return { context, hostFailure };
}
async function open(context: UiContext): Promise<HTMLElement> {
  const root = document.createElement('section'); document.body.append(root);
  await React.act(async () => {
    page = await mountSearch(root, context);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
  return root;
}
function click(root: HTMLElement, label: string): void {
  const button = [...root.querySelectorAll('button')].find(item => item.title === label || item.textContent === label);
  if (!button) throw new Error(`Missing button ${label}`);
  flushSync(() => button.click());
}
function typePassword(root: HTMLElement, value: string): void {
  const input = root.querySelector<HTMLInputElement>('input[aria-label="来源凭据输入框"]');
  if (!input) throw new Error('Missing credential password input');
  flushSync(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

describe('search failure transitions', () => {
  it('auto-loads catalog on open without empty prompt and reuses cached results across remounts until explicit refresh', async () => {
    const cache = new QueryCache();
    let searchCalls = 0;
    const makeContext = () => {
      const { context, hostFailure } = contextFor(
        async () => {
          searchCalls++;
          return { items: [{ reference: { sourceId: 'modio', remoteGameKey: '2475', remoteModId: String(searchCalls) }, displayName: `热门模组 #${searchCalls}` }] };
        },
        undefined,
        modules,
        [{ sourceId: 'modio', credentialConfigured: true }],
      );
      context.query = (key, load, ttl) => cache.get(`mintcat.ui.search/${key}`, load, ttl);
      context.invalidate = prefix => cache.invalidate(`mintcat.ui.search/${prefix}`);
      return { context, hostFailure };
    };
    const first = makeContext();
    const root1 = await open(first.context);
    expect(searchCalls).toBe(1);
    expect(root1.textContent).toContain('热门模组 #1');
    expect(root1.textContent).not.toContain('探索在线模组库');
    expect(first.hostFailure).not.toHaveBeenCalled();

    // Remounting the search page (e.g. switching tabs and coming back) reuses cached results immediately.
    page?.dispose(); page = undefined; document.body.replaceChildren();
    const second = makeContext();
    const root2 = await open(second.context);
    expect(searchCalls).toBe(1);
    expect(root2.textContent).toContain('热门模组 #1');
    expect(root2.textContent).not.toContain('探索在线模组库');

    // Explicit refresh invalidates cache and reloads fresh data.
    await React.act(async () => click(root2, '刷新搜索'));
    expect(searchCalls).toBe(2);
    expect(root2.textContent).toContain('热门模组 #2');
    expect(second.hostFailure).not.toHaveBeenCalled();
  });
  it('shows auth failure with direct credential entry rather than runtime.json instructions and offers only configured alternatives', async () => {
    const calls: string[] = [];
    const { context, hostFailure } = contextFor(async params => {
      calls.push(params.sourceId);
      if (params.sourceId === 'modio') throw { appError: { code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', requestId: 'auth-request', details: {}, retryable: false } };
      return { items: [] };
    });
    const root = await open(context);
    expect(root.textContent).toContain('来源授权失败');
    expect(root.textContent).toContain('缺少有效凭据');
    expect(root.textContent).toContain('https://mod.io/me/access#tokens');
    expect(root.querySelector<HTMLAnchorElement>('a[href="https://mod.io/me/access#tokens"]')).not.toBeNull();
    expect(root.querySelector<HTMLInputElement>('input[aria-label="来源凭据输入框"]')).not.toBeNull();
    expect(root.textContent).not.toContain('runtime.json');
    expect(root.textContent).not.toContain('credentialRef');
    expect(root.textContent).not.toContain('没有找到模组');
    expect(root.textContent).not.toContain('正在读取配置');
    expect(root.querySelector('details')?.open).toBe(false);
    expect([...root.querySelectorAll('button')].some(item => item.textContent === '切换到 disabled')).toBe(false);
    await React.act(async () => click(root, '切换到 ModCat'));
    expect(calls).toEqual(['modio', 'modcat']);
    expect(root.textContent).not.toContain('来源授权失败');
    expect(root.textContent).toContain('没有找到模组');
    expect(hostFailure).not.toHaveBeenCalled();
  });
  it('saves pasted credentials directly to the daemon vault, clears input, reloads search, and supports clearing expired credentials', async () => {
    const inventory = structuredClone(modules);
    inventory.modules[0].game!.descriptor!.sourceBindings.push({ sourceId: 'nexusmods', remoteGameKey: 'deeprockgalactic' });
    let activeNexusSecret = '';
    const setCalls: Array<{ params: CommandParams['sources.credentials.set']; key?: string }> = [];
    const clearCalls: Array<{ params: CommandParams['sources.credentials.clear']; key?: string }> = [];
    const openSpy = vi.spyOn(window, 'open').mockImplementation(() => null);
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    const { context, hostFailure } = contextFor(
      async params => {
        if (params.sourceId === 'nexusmods') {
          if (activeNexusSecret !== 'nexus-personal-key-999') {
            throw { appError: { code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', requestId: 'nexus-auth', details: { reason: 'credential_missing_or_invalid' }, retryable: false } };
          }
          return { items: [{ reference: { sourceId: 'nexusmods', remoteGameKey: 'deeprockgalactic', remoteModId: '42' }, displayName: '授权后可见模组' }] };
        }
        if (params.sourceId === 'modcat') throw { appError: { code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', requestId: 'modcat-auth', details: {}, retryable: false } };
        return { items: [] };
      },
      undefined,
      inventory,
      [{ sourceId: 'nexusmods', credentialConfigured: false }, { sourceId: 'modcat', credentialConfigured: true }],
      {
        set: async (params, key) => {
          setCalls.push({ params, key });
          if (params.sourceId === 'nexusmods') activeNexusSecret = params.secret;
          return { sourceId: params.sourceId, credentialConfigured: true };
        },
        clear: async (params, key) => {
          clearCalls.push({ params, key });
          return { sourceId: params.sourceId, credentialConfigured: false };
        },
      },
    );
    const root = await open(context);
    expect(root.querySelector<HTMLAnchorElement>('.mintcat-auth-portal-url')?.getAttribute('href')).toBe('https://modcat.top');
    expect(root.textContent).toContain('已保存凭据 · 可能已失效');
    await React.act(async () => click(root, '清除已保存凭据'));
    expect(clearCalls).toHaveLength(1);
    expect(clearCalls[0].params).toEqual({ sourceId: 'modcat' });
    expect(clearCalls[0].key).toEqual(expect.any(String));
    expect(root.textContent).not.toContain('已保存凭据 · 可能已失效');

    await React.act(async () => click(root, '切换到 Nexus Mods'));
    const nexusUrl = 'https://www.nexusmods.com/users/myaccount?tab=api%20access';
    expect(root.querySelector<HTMLAnchorElement>('.mintcat-auth-portal-url')?.getAttribute('href')).toBe(nexusUrl);
    expect(root.textContent).toContain('Personal API Key');
    expect(root.textContent).not.toContain('runtime.json');
    await React.act(async () => click(root, '前往 Nexus Mods 获取密钥'));
    expect(openSpy).toHaveBeenCalledWith(nexusUrl, '_blank', 'noopener,noreferrer');
    await React.act(async () => click(root, '复制地址'));
    expect(writeText).toHaveBeenCalledWith(nexusUrl);

    // Invalid whitespace inside token is rejected locally before calling daemon.
    typePassword(root, 'Bearer invalid space');
    await React.act(async () => click(root, '保存并重新加载'));
    expect(setCalls).toHaveLength(0);
    expect(root.textContent).toContain('凭据不能包含空格、换行或非 ASCII 字符');

    // Valid token saves to daemon vault with idempotency key and immediately reloads search results.
    typePassword(root, '  nexus-personal-key-999  ');
    await React.act(async () => click(root, '保存并重新加载'));
    expect(setCalls).toHaveLength(1);
    expect(setCalls[0].params).toEqual({ sourceId: 'nexusmods', secret: 'nexus-personal-key-999' });
    expect(setCalls[0].key).toEqual(expect.any(String));
    expect(root.textContent).toContain('授权后可见模组');
    expect(root.textContent).not.toContain('来源授权失败');
    expect(root.textContent).toContain('已配置凭据');
    expect(hostFailure).not.toHaveBeenCalled();
    openSpy.mockRestore();
  });
  it('accepts long mod.io personal access tokens over 4096 bytes and rejects payloads exceeding 32768 bytes', async () => {
    const longPat = `eyJ${'A'.repeat(2500)}.${'B'.repeat(2500)}.${'C'.repeat(1000)}`;
    expect(longPat.length).toBeGreaterThan(4096);
    let activeModioSecret = '';
    const setCalls: Array<{ params: CommandParams['sources.credentials.set']; key?: string }> = [];
    const { context, hostFailure } = contextFor(
      async params => {
        if (params.sourceId === 'modio') {
          if (activeModioSecret !== longPat) {
            throw { appError: { code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', requestId: 'modio-pat-auth', details: { reason: 'credential_missing_or_invalid' }, retryable: false } };
          }
          return { items: [{ reference: { sourceId: 'modio', remoteGameKey: '2475', remoteModId: '7' }, displayName: 'PAT 认证通过模组' }] };
        }
        return { items: [] };
      },
      undefined,
      modules,
      [{ sourceId: 'modio', credentialConfigured: false }],
      {
        set: async (params, key) => {
          setCalls.push({ params, key });
          if (params.sourceId === 'modio') activeModioSecret = params.secret;
          return { sourceId: params.sourceId, credentialConfigured: true };
        },
      },
    );
    const root = await open(context);
    expect(root.textContent).toContain('Personal Access Token');

    // Oversized paste (> 32768 chars) is still rejected locally before calling daemon.
    typePassword(root, 'X'.repeat(32769));
    await React.act(async () => click(root, '保存并重新加载'));
    expect(setCalls).toHaveLength(0);
    expect(root.textContent).toContain('凭据长度超出限制');

    // Realistic > 4096-byte mod.io PAT submits and immediately loads search results.
    typePassword(root, longPat);
    await React.act(async () => click(root, '保存并重新加载'));
    expect(setCalls).toHaveLength(1);
    expect(setCalls[0].params).toEqual({ sourceId: 'modio', secret: longPat });
    expect(root.textContent).toContain('PAT 认证通过模组');
    expect(root.textContent).not.toContain('来源授权失败');
    expect(hostFailure).not.toHaveBeenCalled();
  });
  it('ends configuration loading after failure and recovers through the explicit reread action', async () => {
    let attempts = 0;
    const { context } = contextFor(async () => ({ items: [] }), async () => {
      if (++attempts === 1) throw new Error('Library worker unavailable');
      return { items: [] };
    });
    const root = await open(context);
    expect(root.textContent).toContain('读取配置失败');
    expect(root.textContent).not.toContain('正在读取配置');
    await React.act(async () => click(root, '重新读取配置'));
    expect(root.textContent).not.toContain('读取配置失败');
    expect(root.textContent).toContain('可浏览模组');
    expect(attempts).toBe(2);
  });
  it('ignores an obsolete search failure after the user switches sources', async () => {
    let rejectOld: ((cause: unknown) => void) | undefined;
    const { context, hostFailure } = contextFor(params => params.sourceId === 'modio'
      ? new Promise((_resolve, reject) => { rejectOld = reject; }) : Promise.resolve({ items: [] }));
    const root = await open(context);
    const modcatTab = [...root.querySelectorAll<HTMLElement>('[role="tab"]')].find(item => item.textContent === 'modcat');
    expect(modcatTab).toBeDefined();
    await React.act(async () => modcatTab!.click());
    await React.act(async () => rejectOld!({ code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', details: {} }));
    expect(root.textContent).not.toContain('来源授权失败');
    expect(root.textContent).toContain('没有找到模组');
    expect(hostFailure).not.toHaveBeenCalled();
  });
  it('guides an empty game inventory without issuing game or search requests', async () => {
    const search = vi.fn(async () => ({ items: [] }));
    const games = vi.fn(async () => ({ items: [] }));
    const { context, hostFailure } = contextFor(search, games, { generationId: 'builtins', modules: [] });
    const root = await open(context);
    expect(root.textContent).toContain('尚未安装游戏支持插件');
    expect(root.textContent).toContain('备份和 Agent 仍可使用');
    expect(search).not.toHaveBeenCalled(); expect(games).not.toHaveBeenCalled(); expect(hostFailure).not.toHaveBeenCalled();
  });
  it('browses Nexus Mods only when it is configured and bound to the selected game', async () => {
    const inventory = structuredClone(modules);
    inventory.modules[0].game!.descriptor!.sourceBindings.push({ sourceId: 'nexusmods', remoteGameKey: 'deeprockgalactic' });
    const search = vi.fn(async (params: CommandParams['sources.search']) => ({ items: params.sourceId === 'nexusmods' && params.remoteGameKey === 'deeprockgalactic' ? [{ reference: { sourceId: 'nexusmods', remoteGameKey: 'deeprockgalactic', remoteModId: 'retained-nexus' }, displayName: '真实绑定来源结果' }] : [] }));
    const { context } = contextFor(search, undefined, inventory, ['nexusmods']);
    const root = await open(context);
    expect([...root.querySelectorAll('[role="tab"]')].map(tab => tab.textContent)).toEqual(['nexusmods']);
    expect(root.textContent).toContain('真实绑定来源结果');
  });
});
