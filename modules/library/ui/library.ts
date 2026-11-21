import type {
  ActionRequired, DownloadOverlay, GameInstallationCandidate, GameSummary, GroupSummary,
  InstallReceiptSummary, JobSnapshot, ModConflictItem, ModDetails, ModSummary,
  ModUpdateCheckItem, ModsListResult, ProfileSummary, ProfilesContentStateResult,
  SourceSearchItem,
} from '@mintcat/sdk-ts';
import type { Disposable, Json, UiContext } from '@mintcat/sdk-ui';
import {
  copyTextToClipboard,
  getSharedRuntime,
  launchSteamGame,
  mountReactView,
  openExternalUrl,
  pickLocalPath,
  pickSavePath,
  publishUiSelection,
  subscribeFileDrop,
} from '@mintcat/sdk-ui';
import {
  canEditProfileContent,
  compareModsForSort,
  modSourceUrl,
  shouldRefreshLibrary,
} from './library-model';
import type { ModSortMode } from './library-model';
import { LibraryView } from './library-view';
import type { LibraryPage } from './library-view';

export interface AvailableGameVariant {
  variantId: string;
  displayName: string;
  store?: string;
  storeAppId?: string;
}

export interface AvailableGame {
  gameId: string;
  displayName: string;
  variants: AvailableGameVariant[];
  localRemoteGameKey?: string;
  sourceIds: string[];
}

export interface LibraryTask { profileId: string; job: JobSnapshot; action?: ActionRequired }

export interface UrlBatchAddReport {
  added: number;
  failed: Array<{ url: string; reason: string }>;
}

export interface LibraryState {
  games: AvailableGame[]; installations: GameSummary['installations']; profiles: ProfileSummary[];
  gameId: string; profileId: string; installationId: string; variantId: string; query: string;
  mods?: ModsListResult; groups: GroupSummary[]; content?: ProfilesContentStateResult;
  versions: Record<string, ModDetails>; candidates: GameInstallationCandidate[];
  tasks: LibraryTask[]; loading: boolean; pending: boolean; message: string; generation: number;
  receipts: InstallReceiptSummary[];
  conflicts: ModConflictItem[];
  conflictChecked: boolean;
  conflictSkippedIds: string[];
  updateChecks: Record<string, ModUpdateCheckItem>;
  checkingUpdates: boolean;
  checkingConflicts: boolean;
  loadingSubscribed: boolean;
  subscribedItems: SourceSearchItem[];
  subscribedNextCursor?: string;
}

export interface LibraryActions {
  run(action: () => Promise<void>): void;
  selectGame(id: string): Promise<void>; selectProfile(id: string): Promise<void>;
  selectInstallation(id: string): void; selectVariant(id: string): void; search(query: string): void;
  refresh(): Promise<void>; discover(rootPath: string): Promise<void>; register(rootPath: string): Promise<void>;
  browseGameDirectory(): Promise<string | null>;
  createProfile(name: string): Promise<void>; editProfile(name: string, description: string): Promise<void>;
  cloneProfile(name: string): Promise<void>; deleteProfile(): Promise<void>;
  createGroup(name: string, parentGroupId?: string): Promise<void>; renameGroup(group: GroupSummary, name: string): Promise<void>;
  expandGroup(group: GroupSummary, expanded: boolean): Promise<void>; deleteGroup(group: GroupSummary): Promise<void>;
  moveGroup(group: GroupSummary, parentGroupId: string | undefined, position: number): Promise<void>;
  setEnabled(mod: ModSummary, enabled: boolean): Promise<void>; setBatchEnabled(ids: string[], enabled: boolean): Promise<void>;
  removeMods(ids: string[]): Promise<void>; renameMod(mod: ModSummary, name: string): Promise<void>;
  moveMod(mod: ModSummary, position: number, groupId?: string): Promise<void>;
  moveModsBatch(ids: string[], groupId?: string): Promise<void>;
  sortMods(mode: ModSortMode): Promise<void>;
  loadVersions(mod: ModSummary, refreshRemote?: boolean): Promise<void>;
  setVersion(mod: ModSummary, value: string): Promise<void>;
  importLocal(kind: 'file' | 'directory', preselectedPath?: string, groupId?: string): Promise<void>;
  cleanupMissing(): Promise<void>;
  cancelDownload(operation: DownloadOverlay): Promise<void>;
  cancelTask(task: LibraryTask): Promise<void>; rejectSelection(task: LibraryTask): Promise<void>;
  approveSelection(task: LibraryTask, path: string): Promise<void>;
  dismissTask(jobId: string): void;
  clearCompletedTasks(): void;
  openModPage(mod: ModSummary): Promise<void>;
  copyModLinks(mods: ModSummary[]): Promise<void>;
  checkUpdates(profileModIds?: string[]): Promise<void>;
  updateMods(profileModIds?: string[]): Promise<void>;
  checkConflicts(): Promise<void>;
  exportMod(mod: ModSummary): Promise<void>;
  addModsFromUrls(urls: string[], groupId?: string): Promise<UrlBatchAddReport>;
  loadSubscribed(append?: boolean): Promise<void>;
  addSubscribedMods(items: SourceSearchItem[], groupId?: string): Promise<void>;
  launchGame(): Promise<void>;
  quickInstall(): Promise<void>;
  quickUninstall(): Promise<void>;
}

const TERMINAL_JOB_STATES = new Set(['succeeded', 'failed', 'cancelled', 'recovery-required']);

export function mountLibrary(root: HTMLElement, context: UiContext): Disposable {
  return mountLibraryPage(root, context, 'mods');
}

export function mountDownloads(root: HTMLElement, context: UiContext): Disposable {
  return mountLibraryPage(root, context, 'downloads');
}

function mountLibraryPage(root: HTMLElement, context: UiContext, page: LibraryPage): Disposable {
  root.classList.add('library');
  const saved = context.state('library', 1, { gameId: '', profileId: '', query: '', jobs: [] as Array<{ jobId: string; profileId: string }> });
  let state: LibraryState = {
    games: [], installations: [], profiles: [], gameId: saved.gameId, profileId: '', installationId: '', variantId: '',
    query: saved.query, groups: [], versions: {}, candidates: [], tasks: [], loading: false, pending: false, message: '', generation: 0,
    receipts: [], conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
    checkingUpdates: false, checkingConflicts: false, loadingSubscribed: false, subscribedItems: [],
  };
  let closed = false, epoch = 0, renderGeneration = 0, inventoryGeneration = 0, profileGeneration = 0;
  let pollPending = false;
  let searchTimer: number | undefined;
  const listeners = new Set<() => void>();
  const cache = new Map<string, { mods: ModsListResult; groups: GroupSummary[] }>();
  const tracked = new Map<string, { profileId: string; afterSeq: number; actionId?: string; refreshed?: boolean }>();
  for (const task of saved.jobs ?? []) tracked.set(task.jobId, { profileId: task.profileId, afterSeq: 0 });
  const alive = (): boolean => !closed && !context.signal.aborted;
  const update = (patch: Partial<LibraryState>): void => {
    if (!alive()) return;
    state = { ...state, ...patch };
    for (const listener of listeners) listener();
  };
  const persist = (): void => context.saveState('library', 1, {
    gameId: state.gameId, profileId: state.profileId, query: state.query,
    jobs: [...tracked].map(([jobId, task]) => ({ jobId, profileId: task.profileId })),
  });
  const publish = (): void => publishUiSelection({ gameId: state.gameId,
    gameName: state.games.find(game => game.gameId === state.gameId)?.displayName ?? '',
    ...(state.profileId ? { profileId: state.profileId } : {}),
  });
  const invalidate = (profileId: string): void => {
    context.invalidate(`mods/${profileId}/`);
    for (const key of cache.keys()) if (key.startsWith(`${profileId}\u0000`)) cache.delete(key);
  };
  const loadReceipts = async (installationId = state.installationId): Promise<void> => {
    const session = epoch;
    if (!installationId || !alive()) {
      update({ receipts: [] });
      return;
    }
    try {
      const result = await context.call('install.receipts', { installationId });
      if (!alive() || session !== epoch || installationId !== state.installationId) return;
      update({ receipts: result.items });
    } catch {
      if (alive() && session === epoch && installationId === state.installationId) update({ receipts: [] });
    }
  };
  const loadMods = async (profileId: string, force = false): Promise<void> => {
    if (!profileId || !alive()) return;
    const generation = ++renderGeneration, session = epoch, query = state.query.trim();
    const key = `${profileId}\u0000${query}`;
    update({ loading: true });
    try {
      let result = force ? undefined : cache.get(key);
      if (!result) {
        if (force) invalidate(profileId);
        result = await context.query(`mods/${profileId}/${query}`, async () => {
          const params = { profileId, pageSize: 100, ...(query ? { query } : {}) };
          const first = await context.call('mods.list', params);
          const items = [...first.items]; let cursor = first.nextCursor;
          while (cursor) {
            const pageResult = await context.call('mods.list', { ...params, cursor });
            if (pageResult.contentRevision !== first.contentRevision) throw new Error('列表在加载期间发生变更，请刷新配置');
            items.push(...pageResult.items); cursor = pageResult.nextCursor;
          }
          const groups = await context.call('groups.list', { profileId });
          if (groups.contentRevision !== first.contentRevision) throw new Error('分组在加载期间发生变更，请刷新配置');
          return { mods: { ...first, items, nextCursor: undefined }, groups: groups.items };
        });
        if (session !== epoch || !alive()) return;
        cache.set(key, result);
      }
      if (generation !== renderGeneration || session !== epoch || profileId !== state.profileId || !alive()) return;
      update({ mods: result.mods, groups: result.groups, loading: false });
    } finally {
      if (generation === renderGeneration && session === epoch && profileId === state.profileId) update({ loading: false });
    }
  };
  const pollContent = async (): Promise<void> => {
    const profileId = state.profileId, session = epoch;
    if (!profileId || !alive()) return;
    const content = await context.call('profiles.contentState', { profileId });
    if (!alive() || session !== epoch || profileId !== state.profileId) return;
    update({ content });
    if (shouldRefreshLibrary(state.mods?.contentRevision, content)) await loadMods(profileId, true);
  };
  const selectProfile = async (profileId: string): Promise<void> => {
    if (!alive()) return;
    renderGeneration++;
    update({
      profileId, mods: undefined, content: undefined, groups: [], versions: {}, message: '', pending: false,
      conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
    });
    publish(); persist();
    if (profileId) await Promise.all([loadMods(profileId), pollContent()]);
  };
  const loadProfiles = async (desiredProfileId = state.profileId || saved.profileId): Promise<void> => {
    const generation = ++profileGeneration, session = epoch, gameId = state.gameId;
    if (!gameId) { await selectProfile(''); return; }
    const games = await context.call('games.list', { gameId, pageSize: 100 });
    if (!alive() || session !== epoch || generation !== profileGeneration || gameId !== state.gameId) return;
    const game = games.items.find(item => item.gameId === gameId);
    const profiles: ProfileSummary[] = []; let cursor: string | undefined;
    if (game) {
      do {
        const pageResult = await context.call('profiles.list', { gameId, pageSize: 100, ...(cursor ? { cursor } : {}) });
        if (!alive() || session !== epoch || generation !== profileGeneration || gameId !== state.gameId) return;
        profiles.push(...pageResult.items); cursor = pageResult.nextCursor;
      } while (cursor);
    }
    const installations = game?.installations ?? [];
    const nextInstallationId = installations.some(item => item.installationId === state.installationId)
      ? state.installationId : installations[0]?.installationId ?? '';
    update({ profiles, installations, installationId: nextInstallationId });
    const selectedProfile = profiles.find(item => item.profileId === desiredProfileId) ?? profiles.find(item => item.active) ?? profiles[0];
    await Promise.all([selectProfile(selectedProfile?.profileId ?? ''), loadReceipts(nextInstallationId)]);
    if (!alive() || session !== epoch || generation !== profileGeneration || gameId !== state.gameId) return;
    const support = state.games.find(item => item.gameId === gameId);
    if (!support?.variants.length && alive()) update({ message: '此游戏的支持插件不可用；已有配置和模组元数据仍可浏览、编辑。发现安装目录、安装与本地导入需先安装游戏支持。' });
    else if (!selectedProfile && alive()) update({ message: installations.length ? '请选择或新建配置。' : '请在游戏设置中发现并注册真实安装目录，再创建配置。' });
  };
  const loadInventory = async (): Promise<void> => {
    const generation = ++inventoryGeneration, session = epoch;
    const [inventory, first] = await Promise.all([
      context.call('modules.list', {}), context.call('games.list', { pageSize: 100 }),
    ]);
    if (!alive() || session !== epoch || generation !== inventoryGeneration) return;
    const registered = [...first.items];
    let cursor = first.nextCursor;
    while (cursor) {
      const pageResult = await context.call('games.list', { pageSize: 100, cursor });
      if (!alive() || session !== epoch || generation !== inventoryGeneration) return;
      registered.push(...pageResult.items); cursor = pageResult.nextCursor;
    }
    const games = new Map<string, AvailableGame>(registered.map(game => [game.gameId, {
      gameId: game.gameId, displayName: game.displayName, variants: [], sourceIds: [],
    }]));
    for (const entry of inventory.modules) {
      if (!entry.enabled || !entry.game || !Array.isArray(entry.game.descriptor.variants)) continue;
      const variants: AvailableGameVariant[] = entry.game.descriptor.variants.flatMap(value => {
        if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
        const record = value as Record<string, unknown>;
        return typeof record.variantId === 'string' && typeof record.displayName === 'string'
          ? [{
            variantId: record.variantId,
            displayName: record.displayName,
            ...(typeof record.store === 'string' ? { store: record.store } : {}),
            ...(typeof record.storeAppId === 'string' ? { storeAppId: record.storeAppId } : {}),
          }] : [];
      });
      const rawBindings = entry.game.descriptor.sourceBindings;
      const sourceIds: string[] = [];
      let localRemoteGameKey: string | undefined;
      if (Array.isArray(rawBindings)) {
        for (const value of rawBindings) {
          if (!value || typeof value !== 'object' || Array.isArray(value)) continue;
          const record = value as Record<string, unknown>;
          if (typeof record.sourceId === 'string') {
            sourceIds.push(record.sourceId);
            if (record.sourceId === 'local' && typeof record.remoteGameKey === 'string') {
              localRemoteGameKey = record.remoteGameKey;
            }
          }
        }
      }
      games.set(entry.game.gameId, {
        gameId: entry.game.gameId,
        displayName: entry.game.displayName,
        variants,
        sourceIds,
        ...(localRemoteGameKey ? { localRemoteGameKey } : {}),
      });
    }
    const available = [...games.values()];
    const game = games.get(state.gameId) ?? available[0];
    update({ games: available, gameId: game?.gameId ?? '', variantId: game?.variants.some(item => item.variantId === state.variantId)
      ? state.variantId : game?.variants[0]?.variantId ?? '', candidates: [],
      message: available.length ? '' : '尚未安装游戏支持插件。安装游戏支持后可注册游戏并创建配置；备份和 Agent 仍可使用。' });
    publish();
  };
  const writable = (): { profileId: string; revision: number } => {
    if (!canEditProfileContent(state.content) || !state.mods || state.mods.profileId !== state.profileId) {
      throw new Error(state.content?.busyOperationId ? `配置内容由下载任务 ${state.content.busyOperationId} 锁定，请先查看或取消任务。` : '配置尚未加载或需要恢复，暂时无法修改内容。');
    }
    return { profileId: state.profileId, revision: state.mods.contentRevision };
  };
  const mutate = async (operation: (profileId: string, revision: number) => Promise<unknown>): Promise<void> => {
    const { profileId, revision } = writable(), session = epoch;
    if (state.pending) throw new Error('请等待当前操作完成');
    update({ pending: true });
    try { await operation(profileId, revision); }
    finally {
      invalidate(profileId);
      if (alive() && session === epoch && profileId === state.profileId) {
        update({ pending: false, versions: {} });
        await Promise.all([loadMods(profileId, true), pollContent()]);
      }
    }
  };
  const profile = (): ProfileSummary => {
    const selected = state.profiles.find(item => item.profileId === state.profileId);
    if (!selected) throw new Error('请选择配置');
    return selected;
  };
  const track = (jobId: string, profileId: string): void => {
    if (!alive()) {
      const latest = context.state('library', 1, saved);
      latest.jobs ??= [];
      if (!latest.jobs.some(task => task.jobId === jobId)) latest.jobs.push({ jobId, profileId });
      context.saveState('library', 1, latest);
      return;
    }
    tracked.set(jobId, { profileId, afterSeq: 0 }); persist();
    update({ message: '任务已启动，可在下载管理查看进度。' });
  };
  const waitForJobActionOrTerminal = async (jobId: string): Promise<{ snapshot: JobSnapshot; action?: ActionRequired }> => {
    let afterSeq = 0;
    let actionId: string | undefined;
    for (let attempt = 0; attempt < 240; attempt++) {
      if (!alive()) throw new Error('页面已关闭');
      let pageResult = await context.call('jobs.watch', { jobId, afterSeq, limit: 200 });
      for (;;) {
        for (const event of pageResult.events) {
          if (typeof event.details?.actionId === 'string') actionId = event.details.actionId;
        }
        const previous = afterSeq;
        afterSeq = pageResult.nextAfterSeq;
        if (pageResult.nextAfterSeq >= pageResult.snapshot.lastSeq || pageResult.nextAfterSeq <= previous) break;
        pageResult = await context.call('jobs.watch', { jobId, afterSeq, limit: 200 });
      }
      if (pageResult.snapshot.state === 'awaiting-approval' && actionId) {
        const action = await context.call('actions.get', { actionId });
        return { snapshot: pageResult.snapshot, action };
      }
      if (TERMINAL_JOB_STATES.has(pageResult.snapshot.state)) {
        return { snapshot: pageResult.snapshot };
      }
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    throw new Error('等待后台任务响应超时，请在任务栏继续查看。');
  };
  const pollTasks = async (): Promise<void> => {
    const session = epoch, tasks: LibraryTask[] = [];
    for (const [jobId, trackedTask] of tracked) {
      let pageResult = await context.call('jobs.watch', { jobId, afterSeq: trackedTask.afterSeq, limit: 200 });
      for (;;) {
        if (!alive() || session !== epoch) return;
        for (const event of pageResult.events) if (typeof event.details?.actionId === 'string') trackedTask.actionId = event.details.actionId;
        const previous = trackedTask.afterSeq;
        trackedTask.afterSeq = pageResult.nextAfterSeq;
        if (pageResult.nextAfterSeq >= pageResult.snapshot.lastSeq || pageResult.nextAfterSeq <= previous) break;
        pageResult = await context.call('jobs.watch', { jobId, afterSeq: trackedTask.afterSeq, limit: 200 });
      }
      if (!alive() || session !== epoch) return;
      const action = pageResult.snapshot.state === 'awaiting-approval' && trackedTask.actionId
        ? await context.call('actions.get', { actionId: trackedTask.actionId }) : undefined;
      if (!alive() || session !== epoch) return;
      tasks.push({ profileId: trackedTask.profileId, job: pageResult.snapshot, action });
      if (pageResult.snapshot.state === 'succeeded' && !trackedTask.refreshed) {
        trackedTask.refreshed = true; invalidate(trackedTask.profileId);
        if (trackedTask.profileId === state.profileId) await loadMods(trackedTask.profileId, true);
        if (['install.apply', 'install.uninstall'].includes(pageResult.snapshot.command)) {
          await loadReceipts();
        }
      }
    }
    if (session === epoch) update({ tasks });
  };
  const poll = async (): Promise<void> => {
    if (pollPending || !alive()) return;
    pollPending = true;
    try { await Promise.all([pollContent(), pollTasks()]); }
    finally { pollPending = false; }
  };
  const run = context.guard(async (action: () => Promise<void>) => {
    if (!alive()) return;
    try { await action(); }
    catch (error) { update({ message: error instanceof Error ? error.message : String(error) }); }
  });
  const actions: LibraryActions = {
    run,
    async selectGame(gameId) {
      const game = state.games.find(item => item.gameId === gameId);
      profileGeneration++; renderGeneration++;
      update({
        gameId, variantId: game?.variants[0]?.variantId ?? '', candidates: [], profiles: [], installations: [],
        installationId: '', profileId: '', mods: undefined, groups: [], content: undefined, versions: {}, pending: false,
        receipts: [], conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
        subscribedItems: [], subscribedNextCursor: undefined,
      });
      publish(); await loadProfiles('');
    },
    selectProfile,
    selectInstallation: installationId => {
      update({ installationId });
      run(() => loadReceipts(installationId));
    },
    selectVariant: variantId => update({ variantId }),
    search(query) {
      update({ query }); clearTimeout(searchTimer);
      searchTimer = window.setTimeout(() => run(() => loadMods(state.profileId)), 200);
    },
    async refresh() { await loadInventory(); await loadProfiles(); await poll(); },
    async discover(rootPath) {
      const gameId = state.gameId, session = epoch;
      const result = await context.call('games.discover', { gameId, includeSteam: true,
        roots: rootPath.trim() ? [{ rootPath: rootPath.trim(), variantId: state.variantId }] : [] });
      if (session === epoch && gameId === state.gameId) update({ candidates: result.items,
        message: result.items.length ? '发现安装目录，请选择后注册；注册仍会执行原生验证。' : '未发现安装目录，请填写真实游戏目录。' });
    },
    async register(rootPath) {
      const gameId = state.gameId, session = epoch;
      await context.call('games.register', { gameId, variantId: state.variantId, rootPath: rootPath.trim() }, crypto.randomUUID());
      if (session === epoch && gameId === state.gameId && alive()) await loadProfiles();
    },
    async browseGameDirectory() {
      return await pickLocalPath({ kind: 'directory', title: '选择游戏安装目录' });
    },
    async createProfile(name) {
      const gameId = state.gameId, session = epoch;
      if (!state.installationId) throw new Error('请先注册游戏安装目录');
      const created = await context.call('profiles.create', { gameId, name, active: true }, crypto.randomUUID());
      if (session === epoch && gameId === state.gameId && alive()) await loadProfiles(created.profileId);
    },
    async editProfile(name, description) {
      const selected = profile(), session = epoch;
      await context.call('profiles.update', { profileId: selected.profileId, expectedRevision: selected.revision,
        name, displayName: name, description }, crypto.randomUUID());
      if (session === epoch && selected.gameId === state.gameId && alive()) await loadProfiles();
    },
    async cloneProfile(name) {
      const selected = profile(), { revision } = writable(), session = epoch;
      const cloned = await context.call('profiles.clone', { sourceProfileId: selected.profileId,
        expectedContentRevision: revision, name, active: true }, crypto.randomUUID());
      if (session === epoch && selected.gameId === state.gameId && alive()) await loadProfiles(cloned.profileId);
    },
    async deleteProfile() {
      const selected = profile(), session = epoch;
      await context.call('profiles.delete', { profileId: selected.profileId, expectedRevision: selected.revision }, crypto.randomUUID());
      invalidate(selected.profileId);
      if (session === epoch && selected.gameId === state.gameId && alive()) await loadProfiles('');
    },
    createGroup: (name, parentGroupId) => mutate((profileId, revision) => context.call('groups.create', {
      profileId, expectedContentRevision: revision, name, parentGroupId, groupType: 'custom',
      position: state.groups.filter(group => group.parentGroupId === parentGroupId).length, expanded: true,
    }, crypto.randomUUID())),
    renameGroup: (group, name) => mutate((profileId, revision) => context.call('groups.update', {
      profileId, groupId: group.groupId, expectedContentRevision: revision, name,
    }, crypto.randomUUID())),
    expandGroup: (group, expanded) => mutate((profileId, revision) => context.call('groups.update', {
      profileId, groupId: group.groupId, expectedContentRevision: revision, expanded,
    }, crypto.randomUUID())),
    deleteGroup: group => mutate((profileId, revision) => context.call('groups.delete', {
      profileId, groupId: group.groupId, expectedContentRevision: revision,
    }, crypto.randomUUID())),
    moveGroup: (group, parentGroupId, position) => mutate((profileId, revision) => context.call('groups.move', {
      profileId, groupId: group.groupId, parentGroupId, position, expectedContentRevision: revision,
    }, crypto.randomUUID())),
    setEnabled: (mod, enabled) => mutate((profileId, revision) => context.call('mods.setEnabled', {
      profileId, profileModId: mod.profileModId, expectedContentRevision: revision, enabled,
    }, crypto.randomUUID())),
    setBatchEnabled: (profileModIds, enabled) => mutate((profileId, revision) => context.call('mods.setEnabledBatch', {
      profileId, profileModIds, expectedContentRevision: revision, enabled,
    }, crypto.randomUUID())),
    async removeMods(ids) {
      const target = state.profileId, session = epoch;
      for (const profileModId of ids) {
        if (!alive() || session !== epoch || target !== state.profileId) return;
        await mutate((profileId, revision) => context.call('mods.remove', { profileId, profileModId,
          expectedContentRevision: revision }, crypto.randomUUID()));
      }
    },
    async renameMod(mod, displayName) {
      const target = state.profileId, session = epoch;
      await context.call('mods.rename', { modId: mod.modId, displayName, expectedEntityRevision: mod.entityRevision, scope: 'shared' }, crypto.randomUUID());
      invalidate(target);
      if (session === epoch && target === state.profileId && alive()) await loadMods(target, true);
    },
    moveMod: (mod, position, groupId) => mutate((profileId, revision) => {
      if (state.query.trim()) throw new Error('请先清除搜索筛选再调整顺序');
      return context.call('mods.move', { profileId, profileModId: mod.profileModId,
        expectedContentRevision: revision, groupId, position }, crypto.randomUUID());
    }),
    async moveModsBatch(ids, groupId) {
      if (state.query.trim()) throw new Error('请先清除搜索筛选再批量移动');
      const target = state.profileId, session = epoch;
      for (const profileModId of ids) {
        if (!alive() || session !== epoch || target !== state.profileId) return;
        const mod = state.mods?.items.find(item => item.profileModId === profileModId);
        if (!mod || mod.groupId === groupId) continue;
        await actions.moveMod(mod, 0, groupId);
      }
    },
    async sortMods(mode) {
      if (state.query.trim()) throw new Error('请先清除搜索筛选再排序');
      const target = state.profileId, session = epoch;
      const groups = new Set(state.mods?.items.map(mod => mod.groupId));
      for (const groupId of groups) {
        const sorted = (state.mods?.items ?? [])
          .filter(mod => mod.groupId === groupId)
          .sort((a, b) => compareModsForSort(a, b, mode));
        for (let position = 0; position < sorted.length; position++) {
          if (!alive() || session !== epoch || target !== state.profileId) return;
          await actions.moveMod(sorted[position], position, groupId);
        }
      }
    },
    async loadVersions(mod, refreshRemote = true) {
      const target = state.profileId, generation = renderGeneration, session = epoch;
      if (!refreshRemote && state.versions[mod.profileModId]) return;
      let details: ModDetails;
      if (refreshRemote && mod.mod.sourceId !== 'local') {
        try {
          details = await context.call('mods.refreshVersions', { profileId: target, profileModId: mod.profileModId }, crypto.randomUUID());
        } catch {
          details = await context.call('mods.get', { profileId: target, profileModId: mod.profileModId });
        }
      } else {
        details = await context.call('mods.get', { profileId: target, profileModId: mod.profileModId });
      }
      if (session === epoch && target === state.profileId && generation === renderGeneration) {
        update({ versions: { ...state.versions, [mod.profileModId]: details } });
      }
    },
    async setVersion(mod, value) {
      const { profileId, revision } = writable(), session = epoch;
      const job = await context.call('mods.setVersion', { profileId, profileModId: mod.profileModId, incarnationId: mod.incarnationId,
        expectedContentRevision: revision, selection: value === 'auto' ? { kind: 'auto' } : { kind: 'pinned', versionId: value.slice(7) },
        mode: 'prepare-and-apply' }, crypto.randomUUID());
      track(job.jobId, profileId);
      if (session === epoch && profileId === state.profileId && alive()) await poll();
    },
    async importLocal(inputKind, preselectedPath, groupId) {
      const { profileId, revision } = writable(), session = epoch;
      const remoteGameKey = state.games.find(game => game.gameId === state.gameId)?.localRemoteGameKey;
      if (!remoteGameKey) throw new Error('当前游戏暂不支持本地导入');
      let chosenPath = preselectedPath?.trim() || null;
      if (!chosenPath) {
        const picked = await pickLocalPath({
          kind: inputKind,
          title: inputKind === 'file' ? '选择本地模组文件（.zip / .pak）' : '选择本地模组目录',
          extensions: inputKind === 'file' ? ['zip', 'pak'] : undefined,
        });
        if (!picked) {
          if (Boolean((globalThis as { isTauri?: unknown }).isTauri)) return;
        } else {
          chosenPath = picked;
        }
      }
      const job = await context.call('mods.importLocal', {
        profileId,
        remoteGameKey,
        inputKind,
        expectedContentRevision: revision,
        ...(groupId ? { groupId } : {}),
      }, crypto.randomUUID());
      track(job.jobId, profileId);
      if (chosenPath) {
        const { action } = await waitForJobActionOrTerminal(job.jobId);
        if (action && action.state === 'pending' && ['select-file', 'select-directory'].includes(action.kind)) {
          await context.call('actions.approve', {
            actionId: action.actionId,
            expectedRevision: action.revision,
            response: { selectedPath: chosenPath },
          }, crypto.randomUUID());
        }
      }
      if (session === epoch && alive()) await poll();
    },
    cleanupMissing: () => mutate((profileId, revision) => context.call('mods.cleanupMissing', {
      gameId: state.gameId, profileId, expectedContentRevision: revision,
    }, crypto.randomUUID())),
    async cancelDownload(operation) {
      if (!operation.jobId) return;
      const job = await context.call('jobs.get', { jobId: operation.jobId });
      await context.call('jobs.cancel', { jobId: job.jobId, expectedRevision: job.lastSeq }, crypto.randomUUID());
      await poll();
    },
    async cancelTask(task) {
      if (task.action && task.action.state === 'pending') {
        const action = await context.call('actions.get', { actionId: task.action.actionId });
        if (action.state === 'pending') {
          await context.call('actions.reject', { actionId: action.actionId, expectedRevision: action.revision, reason: '用户取消任务' }, crypto.randomUUID());
        }
      }
      const job = await context.call('jobs.get', { jobId: task.job.jobId });
      const updated = await context.call('jobs.cancel', { jobId: job.jobId, expectedRevision: job.lastSeq }, crypto.randomUUID());
      await poll();
      if (updated.state === 'cancelled') update({ message: '后台任务已取消。' });
    },
    async rejectSelection(task) {
      if (!task.action) return;
      const action = await context.call('actions.get', { actionId: task.action.actionId });
      await context.call('actions.reject', { actionId: action.actionId, expectedRevision: action.revision, reason: '用户取消选择' }, crypto.randomUUID()); await poll();
    },
    async approveSelection(task, path) {
      if (!task.action || task.job.command !== 'mods.importLocal') throw new Error('任务不是本地导入选择');
      const action = await context.call('actions.get', { actionId: task.action.actionId });
      if (!['select-file', 'select-directory'].includes(action.kind) || action.state !== 'pending') throw new Error('选择请求已失效');
      await context.call('actions.approve', { actionId: action.actionId, expectedRevision: action.revision,
        response: { selectedPath: path.trim() } }, crypto.randomUUID()); await poll();
    },
    dismissTask(jobId) {
      tracked.delete(jobId);
      persist();
      update({ tasks: state.tasks.filter(task => task.job.jobId !== jobId) });
    },
    clearCompletedTasks() {
      const terminalJobIds = new Set(
        state.tasks
          .filter(task => TERMINAL_JOB_STATES.has(task.job.state))
          .map(task => task.job.jobId),
      );
      for (const jobId of terminalJobIds) tracked.delete(jobId);
      persist();
      update({ tasks: state.tasks.filter(task => !terminalJobIds.has(task.job.jobId)) });
    },
    async openModPage(mod) {
      const url = modSourceUrl(mod);
      if (!url) throw new Error('该模组没有可打开的在线源主页链接');
      await openExternalUrl(url);
    },
    async copyModLinks(mods) {
      const urls = [...new Set(mods.map(mod => modSourceUrl(mod)).filter((url): url is string => Boolean(url)))];
      if (!urls.length) throw new Error('所选模组中没有可复制的在线链接');
      await copyTextToClipboard(urls.join('\n'));
      update({ message: `已复制 ${urls.length} 条模组链接到剪贴板。` });
    },
    async checkUpdates(profileModIds) {
      const target = state.profileId, session = epoch;
      if (!target) throw new Error('请先选择配置');
      update({ checkingUpdates: true });
      try {
        const result = await context.call('mods.checkUpdates', {
          profileId: target,
          ...(profileModIds?.length ? { profileModIds } : {}),
        }, crypto.randomUUID());
        if (!alive() || session !== epoch || target !== state.profileId) return;
        const nextChecks = { ...state.updateChecks };
        for (const item of result.items) nextChecks[item.profileModId] = item;
        invalidate(target);
        update({
          updateChecks: nextChecks,
          message: `已检查 ${result.checkedCount} 个在线模组：${result.updatedCount} 个有新版本，${result.unavailableCount} 个在线不可用。`,
        });
        await loadMods(target, true);
      } finally {
        if (alive() && session === epoch) update({ checkingUpdates: false });
      }
    },
    async updateMods(profileModIds) {
      const { profileId, revision } = writable(), session = epoch;
      const job = await context.call('mods.updateBatch', {
        profileId,
        expectedContentRevision: revision,
        ...(profileModIds?.length ? { profileModIds } : {}),
        mode: 'prepare-and-apply',
      }, crypto.randomUUID());
      track(job.jobId, profileId);
      if (session === epoch && profileId === state.profileId && alive()) await poll();
    },
    async checkConflicts() {
      const target = state.profileId, session = epoch;
      if (!target) throw new Error('请先选择配置');
      update({ checkingConflicts: true });
      try {
        const result = await context.call('mods.checkConflicts', { profileId: target });
        if (!alive() || session !== epoch || target !== state.profileId) return;
        update({
          conflicts: result.conflicts,
          conflictChecked: true,
          conflictSkippedIds: result.skippedProfileModIds,
          message: result.conflicts.length
            ? `冲突检测完成（已检 ${result.checkedCount} 个缓存模组）：发现 ${result.conflicts.length} 处文件覆盖冲突。`
            : `冲突检测完成（已检 ${result.checkedCount} 个缓存模组）：未发现文件冲突${result.skippedProfileModIds.length ? `（跳过 ${result.skippedProfileModIds.length} 个未下载缓存的模组）` : ''}。`,
        });
      } finally {
        if (alive() && session === epoch) update({ checkingConflicts: false });
      }
    },
    async exportMod(mod) {
      const target = state.profileId;
      if (!target) throw new Error('请先选择配置');
      if (!mod.appliedArtifactDigest) throw new Error('该模组尚未下载缓存文件，请先更新或下载后再导出');
      const safeName = mod.displayName.replace(/[\\/:*?"<>|]/g, '_').trim() || mod.profileModId;
      const destinationPath = await pickSavePath({
        title: `导出模组缓存：${mod.displayName}`,
        defaultName: `${safeName}.zip`,
        extensions: ['zip', 'pak'],
      });
      if (!destinationPath) return;
      const result = await context.call('mods.exportArtifact', {
        profileId: target,
        profileModId: mod.profileModId,
        destinationPath,
      }, crypto.randomUUID());
      update({ message: `已导出 ${mod.displayName}（${result.detectedFormat}，${result.fileSize} 字节）到 ${result.destinationPath}` });
    },
    async addModsFromUrls(urls, groupId) {
      const targetGame = state.gameId;
      const { profileId } = writable();
      if (!targetGame) throw new Error('请先选择游戏');
      let added = 0;
      const failed: Array<{ url: string; reason: string }> = [];
      for (const rawUrl of urls) {
        if (!alive()) break;
        try {
          const resolved = await context.call('sources.resolveUrl', { gameId: targetGame, url: rawUrl });
          const currentState = await context.call('profiles.contentState', { profileId });
          if (!currentState.writable) throw new Error('配置当前正在被其他任务修改，请稍后重试');
          const job = await context.call('mods.add', {
            profileId,
            reference: resolved.reference,
            expectedContentRevision: currentState.contentRevision,
            ...(groupId ? { groupId } : {}),
          }, crypto.randomUUID());
          track(job.jobId, profileId);
          const outcome = await waitForJobActionOrTerminal(job.jobId);
          if (outcome.snapshot.state === 'succeeded') {
            added++;
          } else {
            failed.push({ url: rawUrl, reason: outcome.snapshot.error?.messageKey ?? outcome.snapshot.state });
          }
        } catch (error) {
          failed.push({ url: rawUrl, reason: error instanceof Error ? error.message : String(error) });
        }
      }
      invalidate(profileId);
      if (alive() && profileId === state.profileId) {
        await Promise.all([loadMods(profileId, true), poll()]);
        update({ message: `URL 批量添加完成：成功 ${added} 个${failed.length ? `，失败 ${failed.length} 个` : ''}。` });
      }
      return { added, failed };
    },
    async loadSubscribed(append = false) {
      const targetGame = state.gameId, session = epoch;
      if (!targetGame) throw new Error('请先选择游戏');
      update({ loadingSubscribed: true });
      try {
        const result = await context.call('sources.listSubscribed', {
          sourceId: 'modio',
          gameId: targetGame,
          pageSize: 50,
          ...(append && state.subscribedNextCursor ? { cursor: state.subscribedNextCursor } : {}),
        });
        if (!alive() || session !== epoch || targetGame !== state.gameId) return;
        update({
          subscribedItems: append ? [...state.subscribedItems, ...result.items] : result.items,
          subscribedNextCursor: result.nextCursor,
        });
      } finally {
        if (alive() && session === epoch) update({ loadingSubscribed: false });
      }
    },
    async addSubscribedMods(selectedItems, groupId) {
      const { profileId } = writable();
      let added = 0;
      for (const item of selectedItems) {
        if (!alive()) break;
        const currentState = await context.call('profiles.contentState', { profileId });
        if (!currentState.writable) throw new Error('配置当前正在被其他任务修改，请稍后重试');
        const job = await context.call('mods.add', {
          profileId,
          reference: item.reference,
          expectedContentRevision: currentState.contentRevision,
          ...(groupId ? { groupId } : {}),
        }, crypto.randomUUID());
        track(job.jobId, profileId);
        const outcome = await waitForJobActionOrTerminal(job.jobId);
        if (outcome.snapshot.state === 'succeeded') added++;
      }
      invalidate(profileId);
      if (alive() && profileId === state.profileId) {
        await Promise.all([loadMods(profileId, true), poll()]);
        update({ message: `已导入 ${added} 个 mod.io 订阅模组。` });
      }
    },
    async launchGame() {
      const game = state.games.find(item => item.gameId === state.gameId);
      const variant = game?.variants.find(item => item.variantId === state.variantId) ?? game?.variants[0];
      if (!variant?.storeAppId) {
        throw new Error('当前游戏变体未声明 Steam App ID，请手动启动游戏');
      }
      await launchSteamGame(variant.storeAppId);
      update({ message: `已请求 Steam 启动 ${game?.displayName ?? state.gameId}（AppID ${variant.storeAppId}）。` });
    },
    async quickInstall() {
      const selected = profile();
      const { profileId } = writable();
      if (!state.installationId || !state.variantId) {
        throw new Error('请先在“游戏设置与安装目录”中注册安装目录并选择游戏变体');
      }
      const detected = await context.call('install.detectInputs', { profileId });
      const missing = detected.items.filter(item => !item.verified || !item.format);
      if (missing.length > 0) {
        const names = missing
          .map(item => state.mods?.items.find(mod => mod.profileModId === item.profileModId)?.displayName ?? item.profileModId)
          .slice(0, 3)
          .join('、');
        throw new Error(`有 ${missing.length} 个已启用模组尚未下载缓存或无法识别格式（如 ${names}），请先更新下载或在“安装与打包”页处理。`);
      }
      const settings = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', {
        scope: { kind: 'game', gameId: state.gameId },
        section: 'game',
      });
      const planJob = await context.call('install.plan', {
        profileId,
        revision: selected.revision,
        contentRevision: selected.contentRevision,
        gameId: state.gameId,
        installationId: state.installationId,
        options: {
          variantId: state.variantId,
          settingsRevision: settings.revision,
          settings: settings.values,
          inputMetadata: detected.items.map(item => ({
            profileModId: item.profileModId,
            format: item.format!,
            audioOnlyHint: item.audioOnlyHint,
          })),
        },
      }, crypto.randomUUID());
      track(planJob.jobId, profileId);
      const { snapshot, action } = await waitForJobActionOrTerminal(planJob.jobId);
      if (!action || action.kind !== 'install-approval') {
        throw new Error(`安装计划未进入待批准状态：${snapshot.error?.messageKey ?? snapshot.state}`);
      }
      const planResult = snapshot.result as { planId?: string; planHash?: string } | undefined;
      if (!planResult?.planId || planResult.planHash !== action.planHash) {
        throw new Error('安装计划校验哈希不匹配，已拒绝部署');
      }
      if (!window.confirm(`确认将配置“${selected.displayName}”应用到游戏目录？\n\n启用模组数：${detected.items.length}\n计划哈希：${planResult.planHash.slice(0, 20)}…`)) {
        await context.call('actions.reject', {
          actionId: action.actionId,
          expectedRevision: action.revision,
          reason: '用户取消应用更改',
        }, crypto.randomUUID());
        await poll();
        return;
      }
      await context.call('actions.approve', {
        actionId: action.actionId,
        expectedRevision: action.revision,
        response: { approved: true },
      }, crypto.randomUUID());
      const applyJob = await context.call('install.apply', {
        planId: planResult.planId,
        planHash: planResult.planHash,
        approvalRef: action.actionId,
      }, crypto.randomUUID());
      track(applyJob.jobId, profileId);
      await poll();
    },
    async quickUninstall() {
      const latestReceipt = state.receipts[0];
      if (!state.installationId || !latestReceipt) {
        throw new Error('当前游戏安装目录没有可卸载的活动收据');
      }
      const job = await context.call('install.uninstall', {
        installationId: state.installationId,
        receiptId: latestReceipt.receiptId,
        revision: latestReceipt.profileRevision,
      }, crypto.randomUUID());
      track(job.jobId, state.profileId);
      const { snapshot, action } = await waitForJobActionOrTerminal(job.jobId);
      if (!action || action.kind !== 'uninstall-approval') {
        throw new Error(`卸载任务未进入待批准状态：${snapshot.error?.messageKey ?? snapshot.state}`);
      }
      if (!window.confirm(`确认卸载已部署的模组文件？\n\n收据：${latestReceipt.receiptId.slice(0, 12)}…（${latestReceipt.fileCount} 个文件）`)) {
        await context.call('actions.reject', {
          actionId: action.actionId,
          expectedRevision: action.revision,
          reason: '用户取消卸载',
        }, crypto.randomUUID());
        await poll();
        return;
      }
      await context.call('actions.approve', {
        actionId: action.actionId,
        expectedRevision: action.revision,
        response: { approved: true },
      }, crypto.randomUUID());
      await poll();
    },
  };
  const { React } = getSharedRuntime();
  const view = mountReactView(root, React.createElement(LibraryView, {
    subscribe: (listener: () => void) => { listeners.add(listener); return () => listeners.delete(listener); },
    getSnapshot: () => state, actions, page, toolbar: context.toolbar,
  }));
  const polling = context.interval(() => run(poll), 750);
  const dropSubscription = subscribeFileDrop(paths => {
    if (page !== 'mods') return;
    if (!canEditProfileContent(state.content) || !state.profileId) return;
    const targetPath = paths[0];
    if (!targetPath) return;
    run(() => actions.importLocal('file', targetPath));
  });
  const inventorySubscription = context.onInventoryChanged(async () => {
    epoch++; renderGeneration++; inventoryGeneration++; profileGeneration++; cache.clear();
    context.invalidate('mods/');
    for (const task of tracked.values()) { task.afterSeq = 0; task.actionId = undefined; task.refreshed = false; }
    update({
      mods: undefined, content: undefined, groups: [], versions: {}, pending: false, generation: epoch,
      receipts: [], conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
    });
    await loadInventory(); await loadProfiles(); await poll();
  });
  run(async () => { await loadInventory(); await loadProfiles(); await poll(); });
  return { dispose() {
    persist(); closed = true; epoch++; renderGeneration++; inventoryGeneration++; profileGeneration++;
    polling.dispose(); dropSubscription(); inventorySubscription.dispose(); clearTimeout(searchTimer); listeners.clear(); view.dispose();
    root.classList.remove('library');
  } };
}
