// @vitest-environment jsdom
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import './ui-runtime';
import { getSharedRuntime, navigateUi, SessionStore, setUiTheme } from '@mintcat/sdk-ui';
import { requestApplication } from './daemon';
import { Workbench } from './workbench';
import type { GameSummary, ModSummary, ProfileSummary } from '@mintcat/sdk-ts';

vi.mock('./daemon', () => ({ requestApplication: vi.fn() }));
const { React } = getSharedRuntime();
let workbench: Workbench | undefined;
beforeAll(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  const jsdomEnvironment: unknown = Reflect.get(globalThis, 'jsdom');
  if (!jsdomEnvironment || typeof jsdomEnvironment !== 'object' || !('window' in jsdomEnvironment)) throw new Error('jsdom environment is required');
  const storageWindow = jsdomEnvironment.window;
  if (!storageWindow || typeof storageWindow !== 'object' || !('localStorage' in storageWindow) || !('sessionStorage' in storageWindow)) throw new Error('jsdom browser storage is required');
  vi.stubGlobal('localStorage', storageWindow.localStorage);
  vi.stubGlobal('sessionStorage', storageWindow.sessionStorage);
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
  Object.defineProperty(window, 'matchMedia', { configurable: true, value: (query: string) => ({ matches: false, media: query, onchange: null, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {}, dispatchEvent: () => false }) });
  HTMLElement.prototype.scrollIntoView = vi.fn();
});
afterAll(() => vi.unstubAllGlobals());
beforeEach(() => {
  localStorage.clear(); sessionStorage.clear(); history.replaceState(null, '', '/');
  vi.mocked(requestApplication).mockReset();
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
});
afterEach(async () => {
  await React.act(async () => { workbench?.dispose(); workbench = undefined; setUiTheme('Light'); });
  vi.useRealTimers(); vi.restoreAllMocks(); document.body.replaceChildren();
});

async function open() {
  const root = document.createElement('main'), toolbar = document.createElement('header'); document.body.append(toolbar, root);
  const rescue = vi.fn();
  await React.act(async () => { workbench = new Workbench(root, '/isolated-test', rescue, undefined, undefined, toolbar); await workbench.refresh(); });
  return { root, toolbar, rescue };
}
function emptyBackend() {
  vi.mocked(requestApplication).mockImplementation(async (_directory, method) => {
    if (method === 'modules.list') return { generationId: 'builtin-services', modules: [] };
    if (method === 'games.list') return { items: [] };
    if (method === 'sources.list') return { sources: [{ sourceId: 'modio' }] };
    if (method === 'backup.cloud.list') return [];
    throw new Error(`Unexpected request ${method}`);
  });
}

describe('bundled workbench pages', () => {
  it('keeps official navigation and builtin services usable without any game plugin', async () => {
    emptyBackend(); const { root, toolbar, rescue } = await open();
    expect(root.textContent).toContain('尚未安装游戏支持插件');
    for (const label of ['模组管理', '在线模组', '下载管理', '设置']) expect(root.textContent).toContain(label);
    for (const [route, label] of [
      ['/mintcat.library/downloads', '下载管理'],
      ['/mintcat.install/tasks', 'daemon 持久任务'], ['/mintcat.feature.backup/backups', '离线便携归档'],
      ['/mintcat.ui.search/search', '尚未安装游戏支持插件'],
    ]) {
      await React.act(async () => { await workbench!.navigate(route); }); expect(root.textContent).toContain(label);
    }
    expect(toolbar.querySelector('.mintcat-search-header--portal')).not.toBeNull();
    await React.act(async () => { navigateUi('robot'); }); expect(root.textContent).toContain('Agent 助手');
    expect(rescue).not.toHaveBeenCalled();
    const requests = vi.mocked(requestApplication).mock.calls.length;
    await React.act(async () => { workbench!.dispose(); });
    expect(vi.getTimerCount()).toBe(0);
    expect(vi.mocked(requestApplication).mock.calls).toHaveLength(requests);
    expect(vi.mocked(requestApplication).mock.calls.some(([, method]) => method === 'jobs.cancel')).toBe(false);
  });
  it('routes both games into the shared renderer with independent drafts and unchanged request revisions', async () => {
    const settings: Record<string, { values: { rootPath: string }; revision: number }> = {
      'game:drg': { values: { rootPath: '/drg' }, revision: 4 }, 'game:rogue-core': { values: { rootPath: '/rogue' }, revision: 9 },
    };
    vi.mocked(requestApplication).mockImplementation(async (_directory, method, params) => {
      if (method === 'modules.list') return { generationId: 'games', modules: ['drg', 'rogue-core'].map(id => ({ moduleId: `mintcat.game.${id}`, enabled: true, game: { gameId: `game:${id}`, displayName: id, descriptor: { gameId: `game:${id}`, displayName: id, settingsSchemaVersion: 1, settingsSchema: { type: 'object', properties: { rootPath: { type: 'string' } } }, variants: [], sourceBindings: [] } } })) };
      if (method === 'games.list') return { items: [] };
      if (method === 'settings.get' || method === 'settings.update') {
        if (!('scope' in params) || !params.scope || typeof params.scope !== 'object' || !('gameId' in params.scope) || typeof params.scope.gameId !== 'string') throw new Error('Missing explicit game scope');
        const gameId = params.scope.gameId;
        if (method === 'settings.update') {
          expect('expectedRevision' in params && params.expectedRevision).toBe(settings[gameId].revision);
          if (!('values' in params) || !params.values || typeof params.values !== 'object' || !('rootPath' in params.values) || typeof params.values.rootPath !== 'string') throw new Error('Missing settings values');
          settings[gameId] = { values: { rootPath: params.values.rootPath }, revision: settings[gameId].revision + 1 };
        }
        return structuredClone(settings[gameId]);
      }
      if (method === 'backup.cloud.list') return [];
      throw new Error(`Unexpected request ${method}`);
    });
    const { root } = await open(); vi.spyOn(window, 'confirm').mockReturnValue(true);
    await React.act(async () => { await workbench!.navigate('/mintcat.game.drg/settings'); });
    const drg = root.querySelector<HTMLInputElement>('.game-settings input')!; drg.value = '/draft'; drg.dispatchEvent(new Event('input', { bubbles: true }));
    await React.act(async () => { await workbench!.navigate('/mintcat.game.rogue-core/settings'); });
    expect(root.querySelector<HTMLInputElement>('.game-settings input')!.value).toBe('/rogue');
    await React.act(async () => { await workbench!.navigate('/mintcat.game.drg/settings'); });
    expect(root.querySelector<HTMLInputElement>('.game-settings input')!.value).toBe('/draft');
    await React.act(async () => { setUiTheme('Dark'); }); expect(root.querySelector<HTMLInputElement>('.game-settings input')!.value).toBe('/draft');
    await React.act(async () => { root.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
    expect(settings['game:drg']).toEqual({ values: { rootPath: '/draft' }, revision: 5 });
    expect(settings['game:rogue-core']).toEqual({ values: { rootPath: '/rogue' }, revision: 9 });
  });
  it('reads and edits registered Library metadata without plugins and preserves game selection', async () => {
    const games: GameSummary[] = ['a', 'b'].map(id => ({ gameId: `game:retained-${id}`, gameKey: `retained-${id}`, displayName: `保留游戏${id.toUpperCase()}`, active: true, installations: [] }));
    const profiles: ProfileSummary[] = games.map((game, index) => ({ profileId: `profile-${index}`, gameId: game.gameId, name: `旧配置${index}`, displayName: `旧配置${index}`, active: true, description: '', revision: 3, contentRevision: 7, incarnationId: `profile-incarnation-${index}` }));
    const mods: ModSummary[] = profiles.map((profile, index) => ({ profileModId: `profile-mod-${index}`, incarnationId: `mod-incarnation-${index}`, modId: `mod-${index}`, mod: { sourceId: 'modio', remoteGameKey: games[index].gameKey, remoteModId: `remote-${index}` }, displayName: `保留模组${index}`, tags: [], approvalStatus: '', entityRevision: 1, enabled: true, sortOrder: 0, versionSelection: { kind: 'auto' } }));
    const session = new SessionStore(localStorage, `mintcat.workbench.session.v1/${encodeURIComponent('/isolated-test')}`);
    session.set('mintcat.library/library', 1, { gameId: games[1].gameId, profileId: profiles[1].profileId, query: '', jobs: [] });
    vi.mocked(requestApplication).mockImplementation(async (_directory, method, params) => {
      if (method === 'modules.list') return { generationId: 'builtins', modules: [] };
      if (method === 'games.list') {
        if ('gameId' in params) return { items: games.filter(game => game.gameId === params.gameId) };
        return 'cursor' in params ? { items: [games[1]] } : { items: [games[0]], nextCursor: 'registered-game-next' };
      }
      if (method === 'profiles.list') return { items: profiles.filter(profile => 'gameId' in params && profile.gameId === params.gameId) };
      const profile = profiles.find(item => 'profileId' in params && item.profileId === params.profileId);
      if (!profile) throw new Error('No selected retained profile');
      const mod = mods.find(item => item.mod.remoteGameKey === games.find(game => game.gameId === profile.gameId)!.gameKey)!;
      if (method === 'mods.list') return { profileId: profile.profileId, contentRevision: profile.contentRevision, total: 1, items: [structuredClone(mod)] };
      if (method === 'groups.list') return { profileId: profile.profileId, contentRevision: profile.contentRevision, items: [] };
      if (method === 'profiles.contentState') return { profileId: profile.profileId, contentRevision: profile.contentRevision, writable: true, recoveryRequired: false, operations: [] };
      if (method === 'mods.setEnabled') {
        if (!('expectedContentRevision' in params) || params.expectedContentRevision !== profile.contentRevision || !('enabled' in params) || typeof params.enabled !== 'boolean') throw new Error('Stale retained content edit');
        mod.enabled = params.enabled; profile.contentRevision++;
        return { operationId: 'metadata-edit', profileId: profile.profileId, revision: profile.revision, contentRevision: profile.contentRevision, changed: true };
      }
      if (method === 'profiles.update') {
        if (!('expectedRevision' in params) || params.expectedRevision !== profile.revision || !('name' in params) || typeof params.name !== 'string') throw new Error('Stale retained profile edit');
        profile.name = profile.displayName = params.name; profile.revision++; return structuredClone(profile);
      }
      throw new Error(`Unexpected retained Library request ${method}`);
    });
    const { root, toolbar, rescue } = await open();
    expect(toolbar.textContent).toContain('保留游戏B'); expect(toolbar.textContent).toContain('旧配置1');
    expect(root.querySelector('table')!.textContent).toContain('保留模组1');
    expect(root.querySelector('table')!.textContent).not.toContain('保留模组0');
    expect(toolbar.querySelector<HTMLButtonElement>('.library-install-button')!.disabled).toBe(true);
    await React.act(async () => { root.querySelector<HTMLButtonElement>('tr[data-mod-id="profile-mod-1"] button[role="switch"]')!.click(); });
    expect(root.querySelector('tr[data-mod-id="profile-mod-1"] button[role="switch"]')!.getAttribute('aria-checked')).toBe('false');
    await React.act(async () => { toolbar.querySelector<HTMLButtonElement>('button[aria-label="导入 / 添加模组"]')!.click(); });
    await React.act(async () => {
      [...document.querySelectorAll<HTMLButtonElement>('.library-import-tab')].find(button => button.textContent?.includes('本地文件'))!.click();
    });
    expect([...document.querySelectorAll<HTMLButtonElement>('.ant-modal button')].find(button => button.textContent === '选择文件')!.disabled).toBe(true);
    await React.act(async () => { document.querySelector<HTMLButtonElement>('.ant-modal-close')!.click(); });
    await React.act(async () => { root.querySelector<HTMLButtonElement>('button[aria-label="管理配置"]')!.click(); });
    await React.act(async () => { [...document.querySelectorAll('button')].find(button => button.textContent === '重命名')!.click(); });
    await React.act(async () => {
      const input = document.querySelector<HTMLInputElement>('input[aria-label="重命名配置"]')!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '新配置B');
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await React.act(async () => { [...document.querySelectorAll('.ant-modal')].find(modal => modal.textContent?.includes('重命名配置'))!.querySelector<HTMLButtonElement>('.ant-btn-primary')!.click(); });
    expect(toolbar.textContent).toContain('新配置B');
    await React.act(async () => { toolbar.querySelector<HTMLInputElement>('.library-game-select input')!.dispatchEvent(new MouseEvent('mousedown', { bubbles: true })); });
    await React.act(async () => { [...document.querySelectorAll<HTMLElement>('.ant-select-item-option-content')].find(option => option.textContent === '保留游戏A')!.click(); });
    expect(root.querySelector('table')!.textContent).toContain('保留模组0');
    expect(root.querySelector('table')!.textContent).not.toContain('保留模组1');
    expect(toolbar.textContent).toContain('旧配置0');
    expect(rescue).not.toHaveBeenCalled();
  });
  it('keeps an accepted backup task alive across navigation and recovers its original request key', async () => {
    let finish!: (job: { jobId: string }) => void;
    let submitted = false;
    const snapshot = { jobId: 'backup-task', command: 'backup.export', state: 'running', generationId: 'builtins', lastSeq: 1, createdAt: '2026-01-01T00:00:00Z', updatedAt: '2026-01-01T00:00:00Z' };
    vi.mocked(requestApplication).mockImplementation(async (_directory, method) => {
      if (method === 'modules.list') return { generationId: 'builtins', modules: [] };
      if (method === 'games.list') return { items: [] };
      if (method === 'backup.cloud.list') return [];
      if (method === 'backup.export') {
        if (submitted) return { jobId: snapshot.jobId };
        submitted = true; return new Promise<{ jobId: string }>(resolve => { finish = resolve; });
      }
      if (method === 'jobs.get') return snapshot;
      if (method === 'jobs.watch') return { snapshot, events: [], nextAfterSeq: 1, retentionStartSeq: 1, resetRequired: false };
      throw new Error(`Unexpected request ${method}`);
    });
    const { root } = await open();
    await React.act(async () => { await workbench!.navigate('/mintcat.feature.backup/backups'); });
    await React.act(async () => {
      const input = root.querySelector<HTMLInputElement>('input[aria-label="输出归档路径"]')!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '/isolated-test/archive.json');
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await React.act(async () => { [...root.querySelectorAll('button')].find(button => button.textContent?.includes('导出离线备份'))!.click(); });
    expect(submitted).toBe(true);
    await React.act(async () => { await workbench!.navigate('/mintcat.library/downloads'); finish({ jobId: snapshot.jobId }); });
    expect(vi.mocked(requestApplication).mock.calls.some(([, method]) => method === 'jobs.cancel')).toBe(false);
    await React.act(async () => { await workbench!.navigate('/mintcat.feature.backup/backups'); });
    expect(root.textContent).toContain('响应尚未确定');
    await React.act(async () => { [...root.querySelectorAll('button')].find(button => button.textContent === '使用原幂等键恢复请求')!.click(); });
    const submissions = vi.mocked(requestApplication).mock.calls.filter(([, method]) => method === 'backup.export');
    expect(submissions).toHaveLength(2); expect(submissions[0][3]).toEqual(expect.any(String)); expect(submissions[1][3]).toBe(submissions[0][3]);
    expect(root.textContent).not.toContain('响应尚未确定');
    await React.act(async () => { workbench!.dispose(); });
    expect(vi.mocked(requestApplication).mock.calls.some(([, method]) => method === 'jobs.cancel')).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
  });
  it('manages remote source credentials from runtime settings without editing config files', async () => {
    let modioConfigured = false;
    vi.mocked(requestApplication).mockImplementation(async (_directory, method, params, key) => {
      if (method === 'modules.list') return { generationId: 'builtins', modules: [] };
      if (method === 'games.list') return { items: [] };
      if (method === 'sources.list') return { sources: [{ sourceId: 'modio', credentialConfigured: modioConfigured }] };
      if (method === 'sources.credentials.set') {
        expect(key).toEqual(expect.any(String));
        expect(params).toEqual({ sourceId: 'modio', secret: 'modio-oauth-secret-123' });
        modioConfigured = true;
        return { sourceId: 'modio', credentialConfigured: true };
      }
      if (method === 'sources.credentials.clear') {
        expect(key).toEqual(expect.any(String));
        expect(params).toEqual({ sourceId: 'modio' });
        modioConfigured = false;
        return { sourceId: 'modio', credentialConfigured: false };
      }
      throw new Error(`Unexpected request ${method}`);
    });
    const { root, rescue } = await open();
    await React.act(async () => { await workbench!.navigate('/mintcat.desktop/runtime'); });
    expect(root.textContent).toContain('未配置凭据');
    await React.act(async () => {
      [...root.querySelectorAll('button')].find(button => button.textContent === '配置凭据')!.click();
    });
    await React.act(async () => {
      const input = document.querySelector<HTMLInputElement>('input[aria-label="来源凭据输入框"]')!;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'modio-oauth-secret-123');
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await React.act(async () => {
      [...document.querySelectorAll('.ant-modal')].find(modal => modal.textContent?.includes('配置 mod.io 凭据'))!
        .querySelector<HTMLButtonElement>('.ant-btn-primary')!.click();
    });
    expect(root.textContent).toContain('已配置凭据');
    expect(root.textContent).toContain('更换凭据');
    await React.act(async () => {
      [...root.querySelectorAll('button')].find(button => button.textContent === '清除凭据')!.click();
    });
    expect(root.textContent).toContain('未配置凭据');
    expect(rescue).not.toHaveBeenCalled();
  });
  it('clears transient info notices on page navigation and auto-loads cached search results when reopening online mods', async () => {
    let searchCalls = 0;
    const game = { gameId: 'game:drg', gameKey: 'drg', displayName: 'Deep Rock Galactic', active: true, installations: [] };
    const profile: ProfileSummary = { profileId: 'profile-1', gameId: game.gameId, name: 'new', displayName: 'new', active: true, description: '', revision: 1, contentRevision: 1, incarnationId: 'inc-1' };
    vi.mocked(requestApplication).mockImplementation(async (_directory, method) => {
      if (method === 'modules.list') return { generationId: 'drg-gen', modules: [{ moduleId: 'mintcat.game.drg', enabled: true, game: { gameId: game.gameId, displayName: game.displayName, descriptor: { gameId: game.gameId, displayName: game.displayName, settingsSchemaVersion: 1, settingsSchema: { type: 'object', properties: {} }, variants: [], sourceBindings: [{ sourceId: 'modio', remoteGameKey: 'drg' }] } } }] };
      if (method === 'games.list') return { items: [game] };
      if (method === 'profiles.list') return { items: [profile] };
      if (method === 'mods.list') return { profileId: profile.profileId, contentRevision: 1, total: 0, items: [] };
      if (method === 'groups.list') return { profileId: profile.profileId, contentRevision: 1, items: [] };
      if (method === 'profiles.contentState') return { profileId: profile.profileId, contentRevision: 1, writable: true, recoveryRequired: false, operations: [] };
      if (method === 'sources.list') return { sources: [{ sourceId: 'modio', credentialConfigured: true }] };
      if (method === 'sources.search') {
        searchCalls++;
        return { items: [{ reference: { sourceId: 'modio', remoteGameKey: 'drg', remoteModId: '100' }, displayName: '热门 DRG 模组' }] };
      }
      if (method === 'mods.add') return { jobId: 'job-add-1' };
      throw new Error(`Unexpected request ${method}`);
    });
    const { root, rescue } = await open();
    await React.act(async () => { await workbench!.navigate('/mintcat.ui.search/search'); });
    expect(searchCalls).toBe(1);
    expect(root.textContent).toContain('热门 DRG 模组');
    expect(root.textContent).not.toContain('探索在线模组库');

    // Adding a mod displays the info notice banner.
    await React.act(async () => {
      [...root.querySelectorAll('button')].find(button => button.textContent === '添加')!.click();
    });
    await React.act(async () => {
      [...document.querySelectorAll('.ant-modal')].find(modal => modal.textContent?.includes('添加模组到配置'))!
        .querySelector<HTMLButtonElement>('.ant-btn-primary')!.click();
    });
    expect(root.querySelector('.module-notice')?.textContent).toContain('已加入下载队列');

    // Navigating away and back clears the stale banner and immediately restores cached catalog items.
    await React.act(async () => { await workbench!.navigate('/mintcat.library/library'); });
    expect(root.querySelector('.module-notice')?.textContent).toBe('');
    await React.act(async () => { await workbench!.navigate('/mintcat.ui.search/search'); });
    expect(root.querySelector('.module-notice')?.textContent).toBe('');
    expect(searchCalls).toBe(1);
    expect(root.textContent).toContain('热门 DRG 模组');
    expect(root.textContent).not.toContain('探索在线模组库');
    expect(rescue).not.toHaveBeenCalled();
  });
  it('renders recovery-required local import tasks in Chinese and lets users dismiss them without crashing', async () => {
    const game = { gameId: 'game:drg', gameKey: 'drg', displayName: 'Deep Rock Galactic', active: true, installations: [] };
    const profile: ProfileSummary = { profileId: 'profile-drg', gameId: game.gameId, name: '深岩日常方案', displayName: '深岩日常方案', active: true, description: '', revision: 1, contentRevision: 1, incarnationId: 'inc-1' };
    const session = new SessionStore(localStorage, `mintcat.workbench.session.v1/${encodeURIComponent('/isolated-test')}`);
    session.set('mintcat.library/library', 1, {
      gameId: game.gameId,
      profileId: profile.profileId,
      query: '',
      jobs: [{ jobId: 'job-stale-import', profileId: profile.profileId }],
    });
    vi.mocked(requestApplication).mockImplementation(async (_directory, method) => {
      if (method === 'modules.list') return { generationId: 'drg-gen', modules: [{ moduleId: 'mintcat.game.drg', enabled: true, game: { gameId: game.gameId, displayName: game.displayName, descriptor: { gameId: game.gameId, displayName: game.displayName, settingsSchemaVersion: 1, settingsSchema: { type: 'object', properties: {} }, variants: [{ variantId: 'steam', displayName: 'Steam' }], sourceBindings: [{ sourceId: 'local', remoteGameKey: 'drg' }] } } }] };
      if (method === 'games.list') return { items: [game] };
      if (method === 'profiles.list') return { items: [profile] };
      if (method === 'mods.list') return { profileId: profile.profileId, contentRevision: 1, total: 0, items: [] };
      if (method === 'groups.list') return { profileId: profile.profileId, contentRevision: 1, items: [] };
      if (method === 'profiles.contentState') return { profileId: profile.profileId, contentRevision: 1, writable: true, recoveryRequired: false, operations: [] };
      if (method === 'jobs.watch') return {
        snapshot: {
          jobId: 'job-stale-import',
          command: 'mods.importLocal',
          state: 'recovery-required',
          generationId: 'drg-gen',
          lastSeq: 3,
          createdAt: '2026-11-21T06:55:00Z',
          updatedAt: '2026-11-21T06:56:12Z',
          error: {
            code: 'RECOVERY_REQUIRED',
            messageKey: 'source.frozen_configuration_unavailable',
            requestId: 'job-stale-import',
            retryable: false,
            recoveryActions: [],
            details: { reason: 'source snapshot identity mismatch' },
          },
        },
        events: [],
        nextAfterSeq: 3,
        retentionStartSeq: 1,
        resetRequired: false,
      };
      throw new Error(`Unexpected request ${method}`);
    });
    const { root, rescue } = await open();
    expect(root.textContent).not.toContain('页面加载失败');
    await React.act(async () => { await workbench!.navigate('/mintcat.library/downloads'); });
    expect(root.textContent).toContain('导入本地模组');
    expect(root.textContent).toContain('已中断（需重试）');
    expect(root.textContent).toContain('旧版未完成任务已失效');
    expect(root.textContent).toContain('深岩日常方案');
    await React.act(async () => {
      [...root.querySelectorAll('button')].find(button => button.textContent?.includes('移除记录'))!.click();
    });
    expect(root.textContent).toContain('当前没有进行中的下载或后台任务');
    expect(rescue).not.toHaveBeenCalled();
  });
});
