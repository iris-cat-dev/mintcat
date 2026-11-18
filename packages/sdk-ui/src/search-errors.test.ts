// @vitest-environment jsdom
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import '../../../apps/desktop/src/ui-runtime';
import { mountSearch } from '../../../modules/ui-search/ui';
import { getSharedRuntime, kit } from './index';
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
function contextFor(search: (params: CommandParams['sources.search']) => Promise<CommandResult['sources.search']>, games?: () => Promise<CommandResult['games.list']>, currentModules = modules, sourceIds = ['modio', 'modcat']) {
  const hostFailure = vi.fn();
  const context: UiContext = {
    moduleId: 'mintcat.ui.search', signal: new AbortController().signal, kit,
    own: value => value, onInventoryChanged: () => ({ dispose() {} }),
    async call<M extends CommandName>(method: M, params: CommandParams[M]): Promise<CommandResult[M]> {
      if (method === 'games.list') return (games ? await games() : { items: [] }) as CommandResult[M];
      if (method === 'sources.list') return { sources: sourceIds.map(sourceId => ({ sourceId })) } as CommandResult[M];
      if (method === 'sources.search') return await search(params as CommandParams['sources.search']) as CommandResult[M];
      throw new Error(`Unexpected command ${method}`);
    },
    async request<T>(): Promise<T> { return currentModules as unknown as T; },
    query: (_key, load) => load(), invalidate() {}, state: (_key, _version, fallback) => fallback,
    saveState() {}, dirty() {}, notify() {},
    guard: action => (...args) => { void Promise.resolve().then(() => action(...args)).catch(hostFailure); },
    interval: () => ({ dispose() {} }),
  };
  return { context, hostFailure };
}
async function open(context: UiContext): Promise<HTMLElement> {
  const root = document.createElement('section'); document.body.append(root);
  await React.act(async () => { page = await mountSearch(root, context); });
  return root;
}
function click(root: HTMLElement, label: string): void {
  const button = [...root.querySelectorAll('button')].find(item => item.title === label || item.textContent === label);
  if (!button) throw new Error(`Missing button ${label}`);
  flushSync(() => button.click());
}

describe('search failure transitions', () => {
  it('shows auth failure rather than empty results and offers only a configured alternative without a host duplicate', async () => {
    const calls: string[] = [];
    const { context, hostFailure } = contextFor(async params => {
      calls.push(params.sourceId);
      if (params.sourceId === 'modio') throw { appError: { code: 'SOURCE_UNAUTHORIZED', messageKey: 'source.operation', requestId: 'auth-request', details: {}, retryable: false } };
      return { items: [] };
    });
    const root = await open(context);
    await React.act(async () => click(root, '刷新搜索'));
    expect(root.textContent).toContain('来源授权失败');
    expect(root.textContent).toContain('缺少有效凭据');
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
    await React.act(async () => click(root, '刷新搜索'));
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
    await React.act(async () => click(root, '刷新搜索'));
    expect(root.textContent).toContain('真实绑定来源结果');
  });
});
