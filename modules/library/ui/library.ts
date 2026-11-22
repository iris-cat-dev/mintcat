import type {
  ActionRequired, DetectedInstallInput, DownloadOverlay, GameSummary, GroupSummary,
  InstallReceiptsResult, JobEvent, JobSnapshot, ModConflictItem, ModDetails, ModSummary,
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
} from '@mintcat/sdk-ui';
import {
  canEditProfileContent,
  compareModsForSort,
  deploymentStatus,
  deployJobProgress,
  sameDeployTarget,
  modSourceUrl,
  shouldRefreshLibrary,
} from './library-model';
import type { DeployProgress, DeployTarget, ModSortMode } from './library-model';
import { LibraryView } from './library-view';
import type { LibraryPage } from './library-view';

import { librarySession, loadAvailableGames, selectedTarget } from './game-inventory';
import type { AvailableGame } from './game-inventory';

export interface LibraryTask { profileId: string; job: JobSnapshot; action?: ActionRequired; downloadErrorCode?: string }

interface TrackedTask {
  profileId: string;
  afterSeq: number;
  actionId?: string;
  refreshed?: boolean;
  deployTarget?: DeployTarget;
  lastEvent?: JobEvent;
  onlineMod?: true;
  downloadFollowed?: true;
}

export interface UrlBatchAddReport {
  added: number;
  failed: Array<{ url: string; reason: string }>;
}

export interface LibraryState {
  games: AvailableGame[]; installations: GameSummary['installations']; profiles: ProfileSummary[];
  gameId: string; profileId: string; installationId: string; variantId: string; query: string;
  mods?: ModsListResult; groups: GroupSummary[]; content?: ProfilesContentStateResult;
  versions: Record<string, ModDetails>;
  tasks: LibraryTask[]; loading: boolean; pending: boolean; message: string; generation: number;
  newImportRowId?: string;
  receipts?: InstallReceiptsResult;
  deploying?: DeployProgress & { target: DeployTarget; jobId?: string };
  deploySweep?: string;
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
  search(query: string): void;
  refresh(): Promise<void>;
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

function onlineAddDownload(snapshot: JobSnapshot): { jobId?: string; errorCode?: string } {
  const result = snapshot.result;
  if (snapshot.command !== 'mods.add' || snapshot.state !== 'succeeded' || !result || typeof result !== 'object') return {};
  if ('downloadJobId' in result && typeof result.downloadJobId === 'string' && result.downloadJobId) {
    return { jobId: result.downloadJobId };
  }
  if ('downloadErrorCode' in result && typeof result.downloadErrorCode === 'string' && result.downloadErrorCode) {
    return { errorCode: result.downloadErrorCode };
  }
  return {};
}

export function mountLibrary(root: HTMLElement, context: UiContext): Disposable {
  return mountLibraryPage(root, context, 'mods');
}

export function mountDownloads(root: HTMLElement, context: UiContext): Disposable {
  return mountLibraryPage(root, context, 'downloads');
}

function mountLibraryPage(root: HTMLElement, context: UiContext, page: LibraryPage): Disposable {
  root.classList.add('library');
  const saved = librarySession(context);
  let rememberedProfileId = saved.profileId;
  let state: LibraryState = {
    games: [], installations: [], profiles: [], gameId: saved.gameId, profileId: '', installationId: '', variantId: '',
    query: saved.query, groups: [], versions: {}, tasks: [], loading: false, pending: false, message: '', generation: 0,
    conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
    checkingUpdates: false, checkingConflicts: false, loadingSubscribed: false, subscribedItems: [],
  };
  let closed = false, epoch = 0, renderGeneration = 0, inventoryGeneration = 0, profileGeneration = 0;
  let pollPending = false;
  let selectionEpoch = 0;
  let sweepTimer: number | undefined;
  let localApply: { jobId: string; planHash: string; selection: number; contentRevision: number } | undefined;
  let receiptGeneration = 0, contentGeneration = 0, lastReceiptRead = 0;
  let searchTimer: number | undefined;
  let importJobs = new Set<string>(), importMarkTimer: number | undefined;
  const listeners = new Set<() => void>();
  const cache = new Map<string, { mods: ModsListResult; groups: GroupSummary[] }>();
  const tracked = new Map<string, TrackedTask>();
  for (const task of saved.jobs ?? []) {
    tracked.set(task.jobId, { ...task, afterSeq: 0 });
    if (task.deployTarget) state.deploying = { target: task.deployTarget, jobId: task.jobId, label: '读取部署状态' };
  }
  const syncOnlineModJobs = (): void => {
    for (const { jobId, profileId, downloadFollowed } of context.onlineModJobs.list()) {
      const task = tracked.get(jobId);
      if (task) {
        task.onlineMod = true;
        task.downloadFollowed = downloadFollowed;
      } else {
        tracked.set(jobId, { profileId, afterSeq: 0, onlineMod: true, downloadFollowed });
      }
    }
  };
  const alive = (): boolean => !closed && !context.signal.aborted;
  const update = (patch: Partial<LibraryState>): void => {
    if (!alive()) return;
    if ((patch.profileId !== undefined && patch.profileId !== state.profileId)
      || (patch.gameId !== undefined && patch.gameId !== state.gameId)
      || (patch.generation !== undefined && patch.generation !== state.generation)) {
      importJobs = new Set();
      clearTimeout(importMarkTimer);
      patch.newImportRowId = undefined;
    }
    const next = { ...state, ...patch };
    if (!sameDeployTarget(state, next) || state.generation !== next.generation) {
      selectionEpoch++;
      next.deploySweep = undefined;
    } else if (next.deploySweep && deploymentStatus(next) !== 'applied') next.deploySweep = undefined;
    state = next;
    for (const listener of listeners) listener();
  };
  const persist = (): void => context.saveState('library', 1, {
    gameId: state.gameId, profileId: rememberedProfileId, query: state.query,
    jobs: [...tracked].filter(([, task]) => !task.onlineMod)
      .map(([jobId, { profileId, deployTarget }]) => ({ jobId, profileId, ...(deployTarget ? { deployTarget } : {}) })),
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
    const generation = ++receiptGeneration, session = epoch;
    lastReceiptRead = Date.now();
    if (!installationId || !alive()) {
      update({ receipts: undefined });
      return;
    }
    try {
      const result = await context.call('install.receipts', { installationId });
      if (alive() && session === epoch && generation === receiptGeneration && installationId === state.installationId) {
        update({ receipts: result });
      }
    } catch (error) {
      if (alive() && session === epoch && generation === receiptGeneration && installationId === state.installationId) {
        update({ receipts: undefined });
        throw error;
      }
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
  const pollContent = async (): Promise<ProfilesContentStateResult | undefined> => {
    const profileId = state.profileId, session = epoch, generation = ++contentGeneration;
    if (!profileId || !alive()) return;
    let content: ProfilesContentStateResult;
    try {
      content = await context.call('profiles.contentState', { profileId });
    } catch (error) {
      if (alive() && session === epoch && generation === contentGeneration && profileId === state.profileId) {
        update({ content: undefined });
        throw error;
      }
      return;
    }
    if (!alive() || session !== epoch || profileId !== state.profileId) return;
    if (generation === contentGeneration) {
      update({ content });
      if (shouldRefreshLibrary(state.mods?.contentRevision, content)) await loadMods(profileId, true);
    }
    return content;
  };
  const selectProfile = async (profileId: string): Promise<void> => {
    if (!alive()) return;
    rememberedProfileId = profileId;
    renderGeneration++;
    update({
      profileId, mods: undefined, content: undefined, groups: [], versions: {}, message: '', pending: false,
      conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
    });
    publish(); persist();
    if (profileId) await Promise.all([loadMods(profileId), pollContent()]);
  };
  const loadProfiles = async (desiredProfileId = rememberedProfileId): Promise<void> => {
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
    else if (!selectedProfile && alive()) update({ message: installations.length ? '请选择或新建配置。' : '请先在「设置 → 游戏安装目录」注册目录，再创建配置。' });
  };
  const loadInventory = async (): Promise<void> => {
    const generation = ++inventoryGeneration, session = epoch;
    const available = await loadAvailableGames(context);
    if (!alive() || session !== epoch || generation !== inventoryGeneration) return;
    const game = available.find(item => item.gameId === state.gameId) ?? available[0];
    update({ games: available, gameId: game?.gameId ?? '',
      ...(game ? selectedTarget(context, game) : { variantId: '', installationId: '' }),
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
        update({ pending: false, versions: {}, content: undefined });
        await Promise.all([loadMods(profileId, true), pollContent()]);
      }
    }
  };
  const profile = (): ProfileSummary => {
    const selected = state.profiles.find(item => item.profileId === state.profileId);
    if (!selected) throw new Error('请选择配置');
    return selected;
  };
  const track = (jobId: string, profileId: string, deployTarget?: DeployTarget, showMessage = true): void => {
    if (!alive()) {
      const latest = context.state('library', 1, saved);
      latest.jobs ??= [];
      if (!latest.jobs.some(task => task.jobId === jobId)) latest.jobs.push({ jobId, profileId, ...(deployTarget ? { deployTarget } : {}) });
      context.saveState('library', 1, latest);
      return;
    }
    tracked.set(jobId, { profileId, afterSeq: 0, deployTarget }); persist();
    if (showMessage) update({ message: '任务已启动，可在下载管理查看进度。' });
  };
  const trackOnlineModAdd = (jobId: string, profileId: string): void => {
    context.onlineModJobs.track(jobId, profileId);
    if (!alive()) return;
    syncOnlineModJobs();
    update({ message: '已提交模组添加任务，可在下载管理查看进度。' });
  };
  const followAddDownload = (parentJobId: string, downloadJobId: string): void => {
    const parent = tracked.get(parentJobId);
    if (!parent || parent.downloadFollowed) return;
    const legacy = !parent.onlineMod;
    if (legacy) context.onlineModJobs.track(parentJobId, parent.profileId);
    context.onlineModJobs.followDownload(parentJobId, downloadJobId);
    syncOnlineModJobs();
    if (legacy) persist();
  };
  const waitForDownloadBeforeNextAdd = async (jobId: string): Promise<boolean> => {
    // A prepare-and-apply download holds the profile content lock until it settles.
    while (alive()) {
      const snapshot = await context.call('jobs.get', { jobId });
      if (TERMINAL_JOB_STATES.has(snapshot.state)) return true;
      if (snapshot.state === 'awaiting-approval') return false;
      await new Promise(resolve => setTimeout(resolve, 750));
    }
    return false;
  };
  const waitForJobActionOrTerminal = async (jobId: string, options: {
    progress?: (value: DeployProgress) => void;
    maxAttempts?: number;
    isSelected?: () => boolean;
  } = {}): Promise<{ snapshot: JobSnapshot; action?: ActionRequired }> => {
    const checkSelection = (): void => {
      if (!alive()) throw new Error('页面已关闭');
      if (options.isSelected?.() === false) throw new Error('应用目标已切换');
    };
    let afterSeq = 0;
    let actionId: string | undefined;
    let lastEvent: JobEvent | undefined;
    for (let attempt = 0; attempt < (options.maxAttempts ?? 240); attempt++) {
      checkSelection();
      let pageResult = await context.call('jobs.watch', { jobId, afterSeq, limit: 200 });
      for (;;) {
        checkSelection();
        for (const event of pageResult.events) {
          lastEvent = event;
          if (typeof event.details?.actionId === 'string') actionId = event.details.actionId;
        }
        const previous = afterSeq;
        afterSeq = pageResult.nextAfterSeq;
        if (pageResult.nextAfterSeq >= pageResult.snapshot.lastSeq || pageResult.nextAfterSeq <= previous) break;
        pageResult = await context.call('jobs.watch', { jobId, afterSeq, limit: 200 });
      }
      options.progress?.(deployJobProgress(pageResult.snapshot, lastEvent));
      // Provider action is handled explicitly in Downloads, not by Apply Configuration.
      if (pageResult.snapshot.command === 'library.download.apply' && pageResult.snapshot.state === 'awaiting-approval') {
        return { snapshot: pageResult.snapshot };
      }
      // install.plan publishes the pending action before appending its verified plan result.
      if (pageResult.snapshot.state === 'awaiting-approval' && actionId
        && (pageResult.snapshot.command !== 'install.plan' || pageResult.snapshot.result != null)) {
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
    syncOnlineModJobs();
    const session = epoch, tasks: LibraryTask[] = [];
    for (const [jobId, trackedTask] of tracked) {
      let pageResult = await context.call('jobs.watch', { jobId, afterSeq: trackedTask.afterSeq, limit: 200 });
      for (;;) {
        if (!alive() || session !== epoch) return;
        for (const event of pageResult.events) if (typeof event.details?.actionId === 'string') trackedTask.actionId = event.details.actionId;
        if (trackedTask.deployTarget && pageResult.events.length) trackedTask.lastEvent = pageResult.events[pageResult.events.length - 1];
        const previous = trackedTask.afterSeq;
        trackedTask.afterSeq = pageResult.nextAfterSeq;
        if (pageResult.nextAfterSeq >= pageResult.snapshot.lastSeq || pageResult.nextAfterSeq <= previous) break;
        pageResult = await context.call('jobs.watch', { jobId, afterSeq: trackedTask.afterSeq, limit: 200 });
      }
      if (!alive() || session !== epoch) return;
      const action = pageResult.snapshot.state === 'awaiting-approval' && trackedTask.actionId
        ? await context.call('actions.get', { actionId: trackedTask.actionId }) : undefined;
      if (!alive() || session !== epoch) return;
      if (tracked.get(jobId) !== trackedTask) continue;
      const download = onlineAddDownload(pageResult.snapshot);
      if (download.jobId) followAddDownload(jobId, download.jobId);
      tasks.push({ profileId: trackedTask.profileId, job: pageResult.snapshot, action, downloadErrorCode: download.errorCode });
      const { command, state: jobState } = pageResult.snapshot;
      const deployTarget = trackedTask.deployTarget;
      const watchingApply = command === 'install.apply' && deployTarget && state.deploying?.jobId === jobId;
      if (watchingApply) update({ deploying: { target: deployTarget, jobId, ...deployJobProgress(pageResult.snapshot, trackedTask.lastEvent) } });
      if (TERMINAL_JOB_STATES.has(jobState) && !trackedTask.refreshed) {
        if (['install.plan', 'install.apply', 'install.uninstall'].includes(command)) await loadReceipts();
        if (!alive() || session !== epoch) return;
        if (watchingApply) {
          if (jobState === 'succeeded' && sameDeployTarget(state, deployTarget)) await pollContent();
          if (!alive() || session !== epoch) return;
          const current = sameDeployTarget(state, deployTarget);
          const result = pageResult.snapshot.result;
          const committed = result !== null && typeof result === 'object' && 'disposition' in result
            && result.disposition === 'committed' && !('cancelRequestedDuringCommit' in result && result.cancelRequestedDuringCommit);
          const sweep = current && jobState === 'succeeded' && committed && localApply?.jobId === jobId
            && localApply.selection === selectionEpoch && localApply.contentRevision === state.content?.contentRevision
            && state.receipts?.items[0]?.planHash === localApply.planHash && deploymentStatus(state) === 'applied';
          update({ deploying: undefined, deploySweep: sweep ? jobId : undefined,
            ...(current ? { message: jobState === 'succeeded' ? '' : deployJobProgress(pageResult.snapshot).label } : {}) });
          if (sweep) {
            clearTimeout(sweepTimer);
            sweepTimer = window.setTimeout(() => update({ deploySweep: undefined }), 650);
          }
          localApply = undefined;
          delete trackedTask.deployTarget;
          persist();
        }
        if (jobState === 'succeeded') {
          invalidate(trackedTask.profileId);
          if (trackedTask.profileId === state.profileId) await loadMods(trackedTask.profileId, true);
        }
        if (importJobs.delete(jobId) && command === 'mods.importLocal' && jobState === 'succeeded') {
          const result = pageResult.snapshot.result;
          if (result && typeof result === 'object' && 'profileModCreated' in result && result.profileModCreated === true
            && 'profileId' in result && result.profileId === state.profileId
            && 'profileModId' in result && typeof result.profileModId === 'string') {
            clearTimeout(importMarkTimer);
            update({ newImportRowId: result.profileModId });
            importMarkTimer = window.setTimeout(() => update({ newImportRowId: undefined }), 1200);
          }
        }
        trackedTask.refreshed = true;
      }
    }
    if (session === epoch) update({ tasks: tasks.filter(task => tracked.has(task.job.jobId)) });
  };
  const poll = async (): Promise<void> => {
    if (pollPending || !alive()) return;
    pollPending = true;
    try {
      // A different page or CLI can also deploy or uninstall without changing profile content.
      await Promise.all([pollContent(), pollTasks(),
        state.installationId && Date.now() - lastReceiptRead >= 5000 ? loadReceipts() : undefined]);
    } finally { pollPending = false; }
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
      if (!game) return;
      profileGeneration++; renderGeneration++;
      update({
        gameId, ...selectedTarget(context, game), profiles: [], installations: [],
        profileId: '', mods: undefined, groups: [], content: undefined, versions: {}, pending: false,
        receipts: undefined, conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
        subscribedItems: [], subscribedNextCursor: undefined,
      });
      publish(); await loadProfiles('');
    },
    selectProfile,
    search(query) {
      update({ query }); clearTimeout(searchTimer);
      searchTimer = window.setTimeout(() => run(() => loadMods(state.profileId)), 200);
    },
    async refresh() { await loadInventory(); await loadProfiles(); await poll(); },
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
      const originJobs = importJobs;
      const remoteGameKey = state.games.find(game => game.gameId === state.gameId)?.localRemoteGameKey;
      if (!remoteGameKey) throw new Error('当前游戏暂不支持本地导入');
      let chosenPath = preselectedPath ?? null;
      if (!chosenPath) {
        const picked = await pickLocalPath({
          kind: inputKind,
          title: inputKind === 'file' ? '选择本地模组文件（.zip / .pak）' : '选择本地模组目录',
          extensions: inputKind === 'file' ? ['zip', 'pak'] : undefined,
        });
        if (!picked) {
          if (getSharedRuntime().pickLocalPath) return;
        } else {
          chosenPath = picked;
        }
      }
      if (!alive() || originJobs !== importJobs) return;
      const job = await context.call('mods.importLocal', {
        profileId,
        remoteGameKey,
        inputKind,
        expectedContentRevision: revision,
        ...(groupId ? { groupId } : {}),
      }, crypto.randomUUID());
      originJobs.add(job.jobId);
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
      if (task.job.command !== 'library.download.apply' && task.action?.state === 'pending') {
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
      if (tracked.get(jobId)?.deployTarget) return;
      context.onlineModJobs.dismiss(new Set([jobId]));
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
      context.onlineModJobs.dismiss(terminalJobIds);
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
      let added = 0, previousDownloadId: string | undefined;
      const failed: Array<{ url: string; reason: string }> = [];
      for (const [index, rawUrl] of urls.entries()) {
        if (!alive()) break;
        try {
          if (previousDownloadId && !(await waitForDownloadBeforeNextAdd(previousDownloadId))) {
            failed.push(...urls.slice(index).map(url => ({ url, reason: '请先在下载管理处理等待确认的下载，再添加剩余模组。' })));
            break;
          }
          previousDownloadId = undefined;
          const resolved = await context.call('sources.resolveUrl', { gameId: targetGame, url: rawUrl });
          const currentState = await context.call('profiles.contentState', { profileId });
          if (!currentState.writable) throw new Error('配置当前正在被其他任务修改，请稍后重试');
          const job = await context.call('mods.add', {
            profileId,
            reference: resolved.reference,
            expectedContentRevision: currentState.contentRevision,
            ...(groupId ? { groupId } : {}),
          }, crypto.randomUUID());
          trackOnlineModAdd(job.jobId, profileId);
          const outcome = await waitForJobActionOrTerminal(job.jobId);
          if (outcome.snapshot.state === 'succeeded') {
            added++;
            previousDownloadId = onlineAddDownload(outcome.snapshot).jobId;
            if (previousDownloadId) followAddDownload(job.jobId, previousDownloadId);
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
      let added = 0, previousDownloadId: string | undefined, waitingForApproval = false;
      for (const item of selectedItems) {
        if (!alive()) break;
        if (previousDownloadId && !(await waitForDownloadBeforeNextAdd(previousDownloadId))) {
          waitingForApproval = true;
          break;
        }
        previousDownloadId = undefined;
        const currentState = await context.call('profiles.contentState', { profileId });
        if (!currentState.writable) throw new Error('配置当前正在被其他任务修改，请稍后重试');
        const job = await context.call('mods.add', {
          profileId,
          reference: item.reference,
          expectedContentRevision: currentState.contentRevision,
          ...(groupId ? { groupId } : {}),
        }, crypto.randomUUID());
        trackOnlineModAdd(job.jobId, profileId);
        const outcome = await waitForJobActionOrTerminal(job.jobId);
        if (outcome.snapshot.state === 'succeeded') {
          added++;
          previousDownloadId = onlineAddDownload(outcome.snapshot).jobId;
          if (previousDownloadId) followAddDownload(job.jobId, previousDownloadId);
        }
      }
      invalidate(profileId);
      if (alive() && profileId === state.profileId) {
        await Promise.all([loadMods(profileId, true), poll()]);
        update({ message: `已添加 ${added} 个 mod.io 订阅模组。${waitingForApproval ? '请先在下载管理处理等待确认的下载，再添加剩余模组。' : ''}` });
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
      const status = deploymentStatus(state);
      if (state.deploying || state.pending || (status !== 'changed' && status !== 'not-deployed')) return;
      const selected = profile();
      const { profileId, revision } = writable();
      let contentRevision = revision;
      const { gameId, installationId, variantId } = state;
      if (contentRevision !== state.content?.contentRevision) throw new Error('配置内容已变化，请刷新后重新应用');
      if (!installationId || !variantId) {
        throw new Error('请先在「设置 → 游戏安装目录」注册安装目录并选择游戏变体');
      }
      const target = { gameId, installationId, variantId, profileId };
      const selection = selectionEpoch;
      const isSelected = (): boolean => alive() && selection === selectionEpoch;
      const matchesContent = (content: ProfilesContentStateResult | undefined): boolean =>
        canEditProfileContent(content) && content?.profileId === profileId && content.contentRevision === contentRevision;
      // Background list renders can lag a successful cache commit.
      // Fence the install against a fresh owner read, not the displayed list.
      const refreshCurrentContent = async (): Promise<boolean> => {
        if (!isSelected()) return false;
        const live = await context.call('profiles.contentState', { profileId });
        if (!isSelected()) return false;
        if (!matchesContent(live)) throw new Error('配置内容或锁定状态已变化，请刷新后重新应用');
        return true;
      };
      const progress = (value: DeployProgress): void => {
        if (isSelected()) update({ deploying: { target, ...value } });
      };
      let handedOff = false;
      update({ deploying: { target, label: '校验模组' }, deploySweep: undefined, message: '' });
      try {
        const detectAtRevision = async (refresh = false) => {
          if (refresh) {
            progress({ label: '重新校验模组' });
            if (!await refreshCurrentContent()) return;
          }
          const result = await context.call('install.detectInputs', { profileId });
          if (!await refreshCurrentContent()) return;
          return result;
        };
        let detected = await detectAtRevision();
        if (!detected) return;
        if (detected.items.some(item => !item.verified && item.legacyCacheAvailable)) {
          progress({ label: '恢复旧版模组缓存' });
          const restored = await context.call('install.restoreLegacyCache', {
            profileId, expectedContentRevision: contentRevision,
          }, crypto.randomUUID());
          if (!isSelected()) return;
          if (restored.restoredCount > 0) {
            contentRevision = restored.contentRevision;
            detected = await detectAtRevision(true);
            if (!detected) return;
          } else if (restored.contentRevision !== contentRevision) {
            throw new Error('配置内容已变化，请刷新后重新应用');
          }
        }
        const sourceOf = (item: DetectedInstallInput): string | undefined =>
          item.sourceId ?? state.mods?.items.find(mod => mod.profileModId === item.profileModId)?.mod.sourceId;
        const downloadIds = detected.items.filter(item => {
          const source = sourceOf(item);
          return !item.verified && (source === 'modio' || source === 'modcat' || source === 'nexusmods');
        }).map(item => item.profileModId);
        if (downloadIds.length) {
          progress({ label: '准备下载缺失模组' });
          const job = await context.call('mods.updateBatch', {
            profileId, expectedContentRevision: contentRevision, profileModIds: downloadIds,
            mode: 'prepare-and-apply', versionPolicy: 'selected',
          }, crypto.randomUUID());
          track(job.jobId, profileId, undefined, isSelected());
          if (!isSelected()) return;
          const { snapshot } = await waitForJobActionOrTerminal(job.jobId, {
            progress, maxAttempts: Infinity, isSelected,
          });
          if (!isSelected()) return;
          if (snapshot.state === 'awaiting-approval') {
            throw new Error('下载需要手动操作。请在下载管理完成操作，下载完成后重新应用配置。');
          }
          if (snapshot.state === 'cancelled') throw new Error('下载已取消，未应用配置。');
          if (snapshot.state !== 'succeeded') throw new Error('下载失败，未应用配置。请在下载管理查看详情。');
          const result = snapshot.result;
          if (snapshot.command !== 'library.download.apply' || !result || typeof result !== 'object'
            || !('applied' in result) || result.applied !== true || !('contentRevision' in result)
            || typeof result.contentRevision !== 'number' || !Number.isSafeInteger(result.contentRevision)
            || result.contentRevision < contentRevision) {
            throw new Error('无法确认下载已写入当前配置，请在下载管理查看详情。');
          }
          // Only the download's committed result may advance the revision for this Apply.
          contentRevision = result.contentRevision;
          detected = await detectAtRevision(true);
          if (!detected) return;
        }
        const blockers = [
          { items: detected.items.filter(item => item.verified && !item.format), issue: '格式无法识别', fix: '请改用受支持的模组文件或禁用这些模组' },
          { items: detected.items.filter(item => !item.verified && sourceOf(item) === 'local'), issue: '本地文件缺失或无法验证', fix: '请重新导入本地文件或禁用这些模组' },
          { items: detected.items.filter(item => !item.verified && sourceOf(item) !== 'local'), issue: '仍缺少可用的下载缓存', fix: '请在下载管理检查下载，或禁用无法下载的模组' },
        ];
        const problems = blockers.filter(({ items }) => items.length).map(({ items, issue, fix }) => {
          const names = items.slice(0, 3).map(item =>
            state.mods?.items.find(mod => mod.profileModId === item.profileModId)?.displayName ?? item.profileModId).join('、');
          return `${items.length} 个已启用模组${issue}（如 ${names}），${fix}。`;
        });
        if (problems.length) throw new Error(problems.join(''));
        progress({ label: '读取游戏设置' });
        const settings = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', {
          scope: { kind: 'game', gameId }, section: 'game',
        });
        if (!isSelected() || !await refreshCurrentContent()) return;
        progress({ label: '提交安装计划' });
        const planJob = await context.call('install.plan', {
          profileId, revision: selected.revision, contentRevision, gameId, installationId,
          options: { variantId, settingsRevision: settings.revision, settings: settings.values,
            inputMetadata: detected.items.map(item => ({
              profileModId: item.profileModId, format: item.format!, audioOnlyHint: item.audioOnlyHint,
            })),
          },
        }, crypto.randomUUID());
        track(planJob.jobId, profileId, undefined, isSelected());
        const { snapshot, action } = await waitForJobActionOrTerminal(planJob.jobId, { progress });
        if (!action || action.kind !== 'install-approval' || action.state !== 'pending') {
          throw new Error(`安装计划未进入待批准状态：${snapshot.error?.messageKey ?? snapshot.state}`);
        }
        const plan = snapshot.result as { planId?: string; planHash?: string } | undefined;
        if (!plan?.planId || plan.planHash !== action.planHash || action.operationId !== plan.planId) {
          throw new Error('安装计划与审批目标不匹配，已拒绝部署');
        }
        const liveContent = isSelected() ? await context.call('profiles.contentState', { profileId }).catch(() => undefined) : undefined;
        const canApprove = isSelected() && matchesContent(liveContent);
        if (!canApprove || !window.confirm(`确认将配置“${selected.displayName}”应用到游戏目录？\n\n启用模组数：${detected.items.length}\n计划哈希：${plan.planHash.slice(0, 20)}…`)) {
          await context.call('actions.reject', {
            actionId: action.actionId, expectedRevision: action.revision, reason: '应用已取消或目标已切换',
          }, crypto.randomUUID());
          if (isSelected()) update({ message: canApprove ? '已取消应用配置。' : '配置内容或锁定状态已变化，请刷新后重新应用。' });
          return;
        }
        progress({ label: '提交人工批准' });
        await context.call('actions.approve', {
          actionId: action.actionId, expectedRevision: action.revision, response: { approved: true },
        }, crypto.randomUUID());
        if (!await refreshCurrentContent()) return;
        progress({ label: '提交部署' });
        const applyJob = await context.call('install.apply', {
          planId: plan.planId, planHash: plan.planHash, approvalRef: action.actionId,
        }, crypto.randomUUID());
        localApply = { jobId: applyJob.jobId, planHash: plan.planHash, selection, contentRevision };
        handedOff = true;
        update({ deploying: { target, jobId: applyJob.jobId, label: '读取部署状态' } });
        track(applyJob.jobId, profileId, target, isSelected());
        await poll();
      } catch (error) {
        if (isSelected()) throw error;
      } finally {
        if (!handedOff) update({ deploying: undefined });
      }
    },
    async quickUninstall() {
      const latestReceipt = state.receipts?.items[0];
      if (!latestReceipt || latestReceipt.installationId !== state.installationId) {
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
  const inventorySubscription = context.onInventoryChanged(async () => {
    epoch++; renderGeneration++; inventoryGeneration++; profileGeneration++; cache.clear();
    context.invalidate('mods/');
    for (const task of tracked.values()) { task.afterSeq = 0; task.actionId = undefined; task.refreshed = false; }
    update({
      mods: undefined, content: undefined, groups: [], versions: {}, pending: false, generation: epoch,
      receipts: undefined, conflicts: [], conflictChecked: false, conflictSkippedIds: [], updateChecks: {},
    });
    await loadInventory(); await loadProfiles(); await poll();
  });
  run(async () => { await loadInventory(); await loadProfiles(); await poll(); });
  return { dispose() {
    persist(); closed = true; epoch++; renderGeneration++; inventoryGeneration++; profileGeneration++;
    clearTimeout(sweepTimer);
    polling.dispose(); inventorySubscription.dispose(); clearTimeout(importMarkTimer); clearTimeout(searchTimer); listeners.clear(); view.dispose();
    root.classList.remove('library');
  } };
}
