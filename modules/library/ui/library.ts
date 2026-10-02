import type {
  ActionRequired, DownloadOverlay, GameInstallationCandidate, GameSummary, GroupSummary,
  JobSnapshot, ModDetails, ModSummary, ModsListResult, ProfileSummary, ProfilesContentStateResult,
} from '@mintcat/sdk-ts';
import type { Disposable, UiContext } from '@mintcat/sdk-ui';
import { getSharedRuntime, mountReactView, publishUiSelection } from '@mintcat/sdk-ui';
import { canEditProfileContent, shouldRefreshLibrary } from './library-model';
import { LibraryView } from './library-view';
import type { LibraryPage } from './library-view';

export interface SignedGame {
  gameId: string;
  displayName: string;
  variants: Array<{ variantId: string; displayName: string }>;
  localRemoteGameKey?: string;
}
export interface LibraryTask { profileId: string; job: JobSnapshot; action?: ActionRequired }
export interface LibraryState {
  games: SignedGame[]; installations: GameSummary['installations']; profiles: ProfileSummary[];
  gameId: string; profileId: string; installationId: string; variantId: string; query: string;
  mods?: ModsListResult; groups: GroupSummary[]; content?: ProfilesContentStateResult;
  versions: Record<string, ModDetails>; candidates: GameInstallationCandidate[];
  tasks: LibraryTask[]; loading: boolean; pending: boolean; message: string; generation: number;
}
export interface LibraryActions {
  run(action: () => Promise<void>): void;
  selectGame(id: string): Promise<void>; selectProfile(id: string): Promise<void>;
  selectInstallation(id: string): void; selectVariant(id: string): void; search(query: string): void;
  refresh(): Promise<void>; discover(rootPath: string): Promise<void>; register(rootPath: string): Promise<void>;
  createProfile(name: string): Promise<void>; editProfile(name: string, description: string): Promise<void>;
  cloneProfile(name: string): Promise<void>; deleteProfile(): Promise<void>;
  createGroup(name: string, parentGroupId?: string): Promise<void>; renameGroup(group: GroupSummary, name: string): Promise<void>;
  expandGroup(group: GroupSummary, expanded: boolean): Promise<void>; deleteGroup(group: GroupSummary): Promise<void>;
  moveGroup(group: GroupSummary, parentGroupId: string | undefined, position: number): Promise<void>;
  setEnabled(mod: ModSummary, enabled: boolean): Promise<void>; setBatchEnabled(ids: string[], enabled: boolean): Promise<void>;
  removeMods(ids: string[]): Promise<void>; renameMod(mod: ModSummary, name: string): Promise<void>;
  moveMod(mod: ModSummary, position: number, groupId?: string): Promise<void>;
  sortMods(order: 'asc' | 'desc'): Promise<void>; loadVersions(mod: ModSummary): Promise<void>;
  setVersion(mod: ModSummary, value: string): Promise<void>; importLocal(kind: 'file' | 'directory'): Promise<void>;
  cleanupMissing(): Promise<void>; cancelDownload(operation: DownloadOverlay): Promise<void>;
  cancelTask(task: LibraryTask): Promise<void>; rejectSelection(task: LibraryTask): Promise<void>;
  approveSelection(task: LibraryTask, path: string): Promise<void>;
}

export function mountLibrary(root: HTMLElement, context: UiContext): Disposable {
  return mountLibraryPage(root, context, 'mods');
}

export function mountLocalImport(root: HTMLElement, context: UiContext): Disposable {
  return mountLibraryPage(root, context, 'local');
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
            const page = await context.call('mods.list', { ...params, cursor });
            if (page.contentRevision !== first.contentRevision) throw new Error('列表在加载期间发生变更，请刷新配置');
            items.push(...page.items); cursor = page.nextCursor;
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
    update({ profileId, mods: undefined, content: undefined, groups: [], versions: {}, message: '', pending: false });
    publish(); persist();
    if (profileId) await Promise.all([loadMods(profileId), pollContent()]);
  };
  const loadProfiles = async (desiredProfileId = state.profileId || saved.profileId): Promise<void> => {
    const generation = ++profileGeneration, session = epoch, gameId = state.gameId;
    if (!gameId) { await selectProfile(''); return; }
    const games = await context.call('games.list', { gameId, pageSize: 100 });
    const profiles: ProfileSummary[] = []; let cursor: string | undefined;
    do {
      const page = await context.call('profiles.list', { gameId, pageSize: 100, ...(cursor ? { cursor } : {}) });
      if (!alive() || session !== epoch || generation !== profileGeneration || gameId !== state.gameId) return;
      profiles.push(...page.items); cursor = page.nextCursor;
    } while (cursor);
    const installations = games.items.find(game => game.gameId === gameId)?.installations ?? [];
    update({ profiles, installations, installationId: installations.some(item => item.installationId === state.installationId)
      ? state.installationId : installations[0]?.installationId ?? '' });
    const profile = profiles.find(item => item.profileId === desiredProfileId) ?? profiles.find(item => item.active) ?? profiles[0];
    await selectProfile(profile?.profileId ?? '');
    if (!profile && alive()) update({ message: installations.length ? '请选择或新建配置。' : '请在游戏设置中发现并注册真实安装目录，再创建配置。' });
  };
  const loadInventory = async (): Promise<void> => {
    const generation = ++inventoryGeneration, session = epoch;
    const inventory = await context.call('modules.list', {});
    if (!alive() || session !== epoch || generation !== inventoryGeneration) return;
    const games: SignedGame[] = [];
    for (const entry of inventory.modules) {
      if (!entry.enabled || !entry.game || !Array.isArray(entry.game.descriptor.variants)) continue;
      const variants = entry.game.descriptor.variants.flatMap(value => {
        if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
        return typeof value.variantId === 'string' && typeof value.displayName === 'string'
          ? [{ variantId: value.variantId, displayName: value.displayName }] : [];
      });
      const rawBindings = entry.game.descriptor.sourceBindings;
      const local = Array.isArray(rawBindings) ? rawBindings.find(value => value && typeof value === 'object'
        && !Array.isArray(value) && value.sourceId === 'local' && typeof value.remoteGameKey === 'string') : undefined;
      if (variants.length) games.push({ gameId: entry.game.gameId, displayName: entry.game.displayName, variants,
        ...(local ? { localRemoteGameKey: local.remoteGameKey as string } : {}) });
    }
    const game = games.find(item => item.gameId === state.gameId) ?? games[0];
    update({ games, gameId: game?.gameId ?? '', variantId: game?.variants.some(item => item.variantId === state.variantId)
      ? state.variantId : game?.variants[0]?.variantId ?? '', candidates: [] });
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
    update({ message: `后台任务 ${jobId} 已启动，关闭页面不会取消任务。` });
  };
  const pollTasks = async (): Promise<void> => {
    const session = epoch, tasks: LibraryTask[] = [];
    for (const [jobId, trackedTask] of tracked) {
      let page = await context.call('jobs.watch', { jobId, afterSeq: trackedTask.afterSeq, limit: 200 });
      for (;;) {
        if (!alive() || session !== epoch) return;
        for (const event of page.events) if (typeof event.details?.actionId === 'string') trackedTask.actionId = event.details.actionId;
        const previous = trackedTask.afterSeq;
        trackedTask.afterSeq = page.nextAfterSeq;
        if (page.nextAfterSeq >= page.snapshot.lastSeq || page.nextAfterSeq <= previous) break;
        page = await context.call('jobs.watch', { jobId, afterSeq: trackedTask.afterSeq, limit: 200 });
      }
      if (!alive() || session !== epoch) return;
      const action = page.snapshot.state === 'awaiting-approval' && trackedTask.actionId
        ? await context.call('actions.get', { actionId: trackedTask.actionId }) : undefined;
      if (!alive() || session !== epoch) return;
      tasks.push({ profileId: trackedTask.profileId, job: page.snapshot, action });
      if (page.snapshot.state === 'succeeded' && !trackedTask.refreshed) {
        trackedTask.refreshed = true; invalidate(trackedTask.profileId);
        if (trackedTask.profileId === state.profileId) await loadMods(trackedTask.profileId, true);
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
      update({ gameId, variantId: game?.variants[0]?.variantId ?? '', candidates: [], profiles: [], installations: [],
        installationId: '', profileId: '', mods: undefined, groups: [], content: undefined, versions: {}, pending: false });
      publish(); await loadProfiles('');
    },
    selectProfile,
    selectInstallation: installationId => update({ installationId }),
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
    async sortMods(order) {
      if (state.query.trim()) throw new Error('请先清除搜索筛选再排序');
      const target = state.profileId, session = epoch;
      const groups = new Set(state.mods?.items.map(mod => mod.groupId));
      for (const groupId of groups) {
        const sorted = (state.mods?.items ?? []).filter(mod => mod.groupId === groupId)
          .sort((a, b) => (order === 'asc' ? 1 : -1) * a.displayName.localeCompare(b.displayName));
        for (let position = 0; position < sorted.length; position++) {
          if (!alive() || session !== epoch || target !== state.profileId) return;
          await actions.moveMod(sorted[position], position, groupId);
        }
      }
    },
    async loadVersions(mod) {
      if (state.versions[mod.profileModId]) return;
      const target = state.profileId, generation = renderGeneration, session = epoch;
      const details = await context.call('mods.get', { profileId: target, profileModId: mod.profileModId });
      if (session === epoch && target === state.profileId && generation === renderGeneration) update({ versions: { ...state.versions, [mod.profileModId]: details } });
    },
    async setVersion(mod, value) {
      const { profileId, revision } = writable(), session = epoch;
      const job = await context.call('mods.setVersion', { profileId, profileModId: mod.profileModId, incarnationId: mod.incarnationId,
        expectedContentRevision: revision, selection: value === 'auto' ? { kind: 'auto' } : { kind: 'pinned', versionId: value.slice(7) },
        mode: 'prepare-and-apply' }, crypto.randomUUID());
      track(job.jobId, profileId);
      if (session === epoch && profileId === state.profileId && alive()) await poll();
    },
    async importLocal(inputKind) {
      const { profileId, revision } = writable(), session = epoch;
      const remoteGameKey = state.games.find(game => game.gameId === state.gameId)?.localRemoteGameKey;
      if (!remoteGameKey) throw new Error('当前签名游戏未声明本地导入绑定');
      const job = await context.call('mods.importLocal', { profileId, remoteGameKey, inputKind,
        expectedContentRevision: revision }, crypto.randomUUID());
      track(job.jobId, profileId);
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
      const job = await context.call('jobs.get', { jobId: task.job.jobId });
      await context.call('jobs.cancel', { jobId: job.jobId, expectedRevision: job.lastSeq }, crypto.randomUUID()); await poll();
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
  };
  const { React } = getSharedRuntime();
  const view = mountReactView(root, React.createElement(LibraryView, {
    subscribe: (listener: () => void) => { listeners.add(listener); return () => listeners.delete(listener); },
    getSnapshot: () => state, actions, page, toolbar: context.toolbar,
  }));
  const polling = context.interval(() => run(poll), 750);
  const generationSubscription = context.onGenerationChanged(async () => {
    epoch++; renderGeneration++; inventoryGeneration++; profileGeneration++; cache.clear();
    context.invalidate('mods/');
    for (const task of tracked.values()) { task.afterSeq = 0; task.actionId = undefined; task.refreshed = false; }
    update({ mods: undefined, content: undefined, groups: [], versions: {}, pending: false, generation: epoch });
    await loadInventory(); await loadProfiles(); await poll();
  });
  run(async () => { await loadInventory(); await loadProfiles(); await poll(); });
  return { dispose() {
    persist(); closed = true; epoch++; renderGeneration++; inventoryGeneration++; profileGeneration++;
    polling.dispose(); generationSubscription.dispose(); clearTimeout(searchTimer); listeners.clear(); view.dispose();
    root.classList.remove('library');
  } };
}
