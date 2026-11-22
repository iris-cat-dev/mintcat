import type {
  GroupSummary,
  JobEvent,
  JobSnapshot,
  ModSummary,
  DownloadOverlay,
  DownloadOverlayItem,
  ProfilesContentStateResult,
} from '@mintcat/sdk-ts';
import { tr } from '@mintcat/sdk-ui';
import type { LibraryState } from './library';

export interface ModDownloadOverlay {
  operation: DownloadOverlay;
  item: DownloadOverlayItem;
}

export type ModStatusFilter = 'all' | 'enabled' | 'disabled' | 'pending';
export type ModSortMode =
  | 'name-asc'
  | 'name-desc'
  | 'time-desc'
  | 'time-asc'
  | 'approval-asc'
  | 'approval-desc';

export interface ModFacetFilters {
  status: ModStatusFilter;
  sourceId: string;
  approval: string;
  multiplayer: string;
  category: string;
  updatesOnly?: boolean;
}

export const MULTIPLAYER_TAGS = ['RequiredByAll', 'RecommendedForAll', 'Optional'] as const;
type MultiplayerRequirement = typeof MULTIPLAYER_TAGS[number];
const MULTIPLAYER_TAG_SET = new Set<string>(MULTIPLAYER_TAGS);
const APPROVAL_TAGS = new Set(['Verified', 'Auto-Verified', 'Approved', 'Sandbox']);
const VERSION_TAG_PATTERN = /^v?\d+\.\d+(\.\d+)*$/i;
const TERMINAL_STATES = new Set(['succeeded', 'failed', 'cancelled']);

export function indexDownloadOverlays(
  state: ProfilesContentStateResult,
): ReadonlyMap<string, ModDownloadOverlay> {
  const overlays = new Map<string, ModDownloadOverlay>();
  for (const operation of state.operations) {
    for (const item of operation.items) {
      overlays.set(item.profileModId, { operation, item });
    }
  }
  return overlays;
}

export function canEditProfileContent(state: ProfilesContentStateResult | undefined): boolean {
  return state?.writable === true;
}

export type DeploymentStatus = 'unknown' | 'not-deployed' | 'changed' | 'applied';

export function deploymentStatus(state: Pick<LibraryState,
  'installationId' | 'installations' | 'profileId' | 'profiles' | 'variantId' | 'content' | 'receipts'
>): DeploymentStatus {
  const { content, receipts } = state;
  const installation = state.installations.find(item => item.installationId === state.installationId);
  const profile = state.profiles.find(item => item.profileId === state.profileId);
  if (!installation || !profile || !state.variantId || content?.profileId !== profile.profileId
    || content.recoveryRequired || receipts?.installationId !== installation.installationId) return 'unknown';
  if (!receipts.items.length) return 'not-deployed';
  const applied = receipts.deployedProfile;
  if (!applied) return 'unknown'; // Older daemons only identify receipts by plan hash.
  return applied.profileId === profile.profileId
    && applied.profileIncarnationId === profile.incarnationId
    && applied.installationIncarnationId === installation.incarnationId
    && applied.variantId === state.variantId
    && applied.contentRevision === content.contentRevision ? 'applied' : 'changed';
}

export interface DeployProgress { label: string; percent?: number }
export type DeployTarget = Pick<LibraryState, 'gameId' | 'profileId' | 'installationId' | 'variantId'>;

export function sameDeployTarget(a: DeployTarget, b: DeployTarget): boolean {
  return a.gameId === b.gameId && a.profileId === b.profileId
    && a.installationId === b.installationId && a.variantId === b.variantId;
}

export function deployJobProgress(job: JobSnapshot, event?: JobEvent): DeployProgress {
  const planning = job.command === 'install.plan';
  const labels: Record<JobSnapshot['state'], string> = job.command === 'library.download.apply' ? {
    queued: tr('下载排队中', 'Download queued'),
    preparing: tr('准备下载缺失模组', 'Preparing to download missing mods'),
    running: tr('下载缺失模组', 'Downloading missing mods'),
    'awaiting-approval': tr('下载等待手动操作', 'Download needs your attention'),
    committing: tr('写入模组缓存', 'Writing to the mod cache'),
    cancelling: tr('正在取消下载', 'Cancelling download'),
    succeeded: tr('模组下载完成', 'Mod download complete'),
    cancelled: tr('下载已取消', 'Download cancelled'),
    failed: tr('下载失败', 'Download failed'),
    'recovery-required': tr('下载需要恢复', 'Download needs recovery'),
  } : {
    queued: planning ? tr('计划排队中', 'Plan queued') : tr('部署排队中', 'Deployment queued'),
    preparing: planning ? tr('准备安装计划', 'Preparing the install plan') : tr('准备部署', 'Preparing deployment'),
    running: planning ? tr('生成安装计划', 'Creating the install plan') : tr('部署中', 'Deploying'),
    'awaiting-approval': tr('等待人工批准', 'Waiting for approval'),
    committing: tr('写入游戏目录', 'Writing to the game folder'),
    cancelling: tr('正在取消', 'Cancelling'),
    succeeded: planning ? tr('计划已批准', 'Plan approved') : tr('核对部署收据', 'Checking deployment receipts'),
    cancelled: tr('部署已取消', 'Deployment cancelled'),
    failed: tr('部署失败', 'Deployment failed'),
    'recovery-required': tr('需要恢复', 'Recovery required'),
  };
  const label = labels[job.state];
  // Units belong to one event, not to the whole deployment or a previous stage.
  if (!event || event.jobId !== job.jobId || event.generationId !== job.generationId
    || event.state !== job.state || event.seq !== job.lastSeq) return { label };
  const { completedUnits: done, totalUnits: total } = event;
  if (done === undefined || total === undefined || !Number.isSafeInteger(done)
    || !Number.isSafeInteger(total) || total <= 0 || done < 0 || done > total) return { label };
  return { label, percent: Math.floor(100 * done / total) };
}

export function shouldRefreshLibrary(
  cachedContentRevision: number | undefined,
  state: ProfilesContentStateResult,
): boolean {
  return cachedContentRevision !== undefined
    && cachedContentRevision !== state.contentRevision;
}

export function overlayProgressPercent(overlay: ModDownloadOverlay | undefined): number | undefined {
  if (!overlay) return undefined;
  const state = overlay.item.transferState ?? overlay.operation.state;
  if (TERMINAL_STATES.has(state)) return undefined;
  const completed = overlay.item.downloadedBytes ?? overlay.operation.completedUnits;
  const total = overlay.item.totalBytes ?? overlay.operation.totalUnits;
  if (total <= 0) return 0;
  return Math.min(100, Math.floor((completed / total) * 100));
}

export function downloadPhaseLabel(phase?: string, fallbackState?: JobSnapshot['state']): string {
  switch (phase) {
    case 'download-queued':
    case 'queued': return tr('等待调度', 'Queued');
    case 'download-preparing':
    case 'preparing': return tr('正在解析模组元数据', 'Reading mod metadata');
    case 'awaiting-approval': return tr('等待确认', 'Awaiting confirmation');
    case 'running':
    case 'download-running':
    case 'downloading': return tr('正在下载模组文件', 'Downloading mod files');
    case 'verifying': return tr('正在校验文件完整性', 'Verifying files');
    case 'publishing': return tr('正在写入模组缓存', 'Writing to the mod cache');
    case 'commit-started':
    case 'committing': return tr('正在写入配置', 'Saving profile');
    case 'completed': return tr('下载完成', 'Download complete');
    case 'commit-finished':
    case 'recovered-commit-finished':
    case 'download-succeeded':
    case 'succeeded': return tr('已完成', 'Completed');
    case 'download-failed':
    case 'failed': return tr('下载失败', 'Download failed');
    case 'cancel-requested':
    case 'cancelling': return tr('正在取消', 'Cancelling');
    case 'download-cancelled':
    case 'cancelled-at-safe-point':
    case 'cancelled-from-recovery':
    case 'cancelled': return tr('已取消', 'Cancelled');
    case 'download-recovery-required':
    case 'commit-state-unknown':
    case 'recovery-required': return tr('任务已中断，无法自动继续', 'Task interrupted and cannot resume automatically');
    default: return fallbackState ? downloadPhaseLabel(fallbackState) : tr('处理中', 'Processing');
  }
}

export function pendingVersionLabel(overlay: ModDownloadOverlay | undefined): string | undefined {
  if (!overlay?.item.pendingSelection) return undefined;
  const selection = overlay.item.pendingSelection;
  const version = selection.kind === 'auto'
    ? tr(`自动 → ${overlay.item.resolvedVersion}`, `Auto → ${overlay.item.resolvedVersion}`)
    : selection.versionId ?? overlay.item.resolvedVersion;
  const completed = overlay.item.downloadedBytes ?? overlay.operation.completedUnits;
  const total = overlay.item.totalBytes ?? overlay.operation.totalUnits;
  const progress = total === 0
    ? 0
    : Math.min(100, Math.floor((completed / total) * 100));
  const state = downloadPhaseLabel(overlay.item.transferState ?? overlay.operation.state);
  return tr(`待处理 ${version} · ${state} · ${progress}%`, `Pending ${version} · ${state} · ${progress}%`);
}

export function extractGameVersionTags(tags: readonly string[]): string[] {
  return tags
    .map(tag => tag.trim())
    .filter(tag => VERSION_TAG_PATTERN.test(tag));
}

export function modCategoryTags(tags: readonly string[]): string[] {
  return tags.map(tag => tag.trim()).filter(tag =>
    tag.length > 0
    && !MULTIPLAYER_TAG_SET.has(tag)
    && !APPROVAL_TAGS.has(tag)
    && !VERSION_TAG_PATTERN.test(tag),
  );
}

export function modCategoryLabel(category: string): string {
  return category === 'QoL' ? tr('QoL', 'Quality of life') : category;
}

export function modMultiplayerRequirement(tags: readonly string[]): MultiplayerRequirement | undefined {
  return MULTIPLAYER_TAGS.find(tag => tags.includes(tag));
}

export function modMultiplayerLabel(requirement?: MultiplayerRequirement): string {
  switch (requirement) {
    case 'RequiredByAll': return tr('全员必装', 'Required for all');
    case 'RecommendedForAll': return tr('推荐全员', 'Recommended for all');
    case 'Optional': return tr('客户端可选', 'Client optional');
    default: return '—';
  }
}

function isUngradedApproval(approval: string): boolean {
  const status = approval.trim();
  return !status || status === 'unavailable' || status === '—';
}

export function modApprovalLabel(approval: string): string {
  switch (approval.trim()) {
    case 'Verified': return tr('已验证', 'Verified');
    case 'Approved': return tr('已批准', 'Approved');
    case 'Sandbox': return tr('沙盒', 'Sandbox');
    default: return isUngradedApproval(approval) ? '—' : approval.trim();
  }
}

export function formatVersionText(versionId?: string, displayVersion?: string): string {
  const display = displayVersion?.trim();
  if (display) return display;
  if (!versionId) return '—';
  if (versionId === 'legacy-unversioned') return tr('未标明版本', 'Unversioned');
  if (versionId.startsWith('sha256:')) return `${versionId.slice(7, 15)}`;
  return versionId;
}

export function modDisplayVersion(
  mod: Pick<ModSummary, 'appliedDisplayVersion' | 'appliedVersionId' | 'currentDisplayVersion' | 'currentVersionId' | 'versionSelection'>,
): string {
  if (mod.appliedDisplayVersion || mod.appliedVersionId) {
    return formatVersionText(mod.appliedVersionId, mod.appliedDisplayVersion);
  }
  if (mod.currentDisplayVersion || mod.currentVersionId) {
    return formatVersionText(mod.currentVersionId, mod.currentDisplayVersion);
  }
  if (mod.versionSelection.versionId) {
    return formatVersionText(mod.versionSelection.versionId);
  }
  return '—';
}

export function modHasUpdate(mod: Pick<ModSummary, 'mod' | 'currentVersionId' | 'appliedVersionId'>): boolean {
  if (mod.mod.sourceId === 'local') return false;
  return Boolean(mod.currentVersionId && mod.appliedVersionId && mod.appliedVersionId !== mod.currentVersionId);
}

export function modSourceUrl(mod: Pick<ModSummary, 'url' | 'mod'>): string | undefined {
  const direct = mod.url?.trim();
  if (direct && /^https:\/\//i.test(direct)) return direct;
  const { sourceId, remoteGameKey, remoteModId } = mod.mod;
  if (!remoteModId) return undefined;
  if (sourceId === 'modcat') {
    return `https://modcat.top/#/modDetail?ModId=${encodeURIComponent(remoteModId)}`;
  }
  if (sourceId === 'nexusmods' && remoteGameKey) {
    return `https://www.nexusmods.com/${encodeURIComponent(remoteGameKey)}/mods/${encodeURIComponent(remoteModId)}`;
  }
  if (sourceId === 'modio' && remoteGameKey && !/^\d+$/.test(remoteModId)) {
    return `https://mod.io/g/${encodeURIComponent(remoteGameKey)}/m/${encodeURIComponent(remoteModId)}`;
  }
  return undefined;
}

export function parseModUrlLines(raw: string): string[] {
  const seen = new Set<string>();
  const urls: string[] = [];
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    urls.push(trimmed);
  }
  return urls;
}

export function approvalRank(status: string): number {
  switch (status.trim()) {
    case 'Verified': return 0;
    case 'Approved': return 1;
    case 'Sandbox': return 2;
    default: return 3;
  }
}

export function compareModsForSort(a: ModSummary, b: ModSummary, mode: ModSortMode): number {
  switch (mode) {
    case 'name-asc':
      return a.displayName.localeCompare(b.displayName);
    case 'name-desc':
      return b.displayName.localeCompare(a.displayName);
    case 'time-desc':
      return (b.createdAtMs ?? b.updatedAtMs ?? 0) - (a.createdAtMs ?? a.updatedAtMs ?? 0)
        || a.displayName.localeCompare(b.displayName);
    case 'time-asc':
      return (a.createdAtMs ?? a.updatedAtMs ?? 0) - (b.createdAtMs ?? b.updatedAtMs ?? 0)
        || a.displayName.localeCompare(b.displayName);
    case 'approval-asc':
      return approvalRank(a.approvalStatus) - approvalRank(b.approvalStatus)
        || a.displayName.localeCompare(b.displayName);
    case 'approval-desc':
      return approvalRank(b.approvalStatus) - approvalRank(a.approvalStatus)
        || a.displayName.localeCompare(b.displayName);
  }
}

export function matchesModFacets(
  mod: ModSummary,
  facets: ModFacetFilters,
  pendingIds: ReadonlySet<string>,
  updatableIds: ReadonlySet<string>,
): boolean {
  if (facets.status === 'enabled' && !mod.enabled) return false;
  if (facets.status === 'disabled' && mod.enabled) return false;
  if (facets.status === 'pending' && !pendingIds.has(mod.profileModId)) return false;
  if (facets.updatesOnly && !updatableIds.has(mod.profileModId)) return false;
  if (facets.sourceId && mod.mod.sourceId !== facets.sourceId) return false;
  if (facets.approval) {
    if (facets.approval === 'none') {
      if (!isUngradedApproval(mod.approvalStatus)) return false;
    } else if (mod.approvalStatus !== facets.approval) {
      return false;
    }
  }
  if (facets.multiplayer && modMultiplayerRequirement(mod.tags) !== facets.multiplayer) return false;
  if (facets.category && !modCategoryTags(mod.tags).includes(facets.category)) return false;
  return true;
}

type SortMod = Pick<ModSummary, 'profileModId' | 'groupId' | 'sortOrder'> & {
  mod: Pick<ModSummary['mod'], 'sourceId'>;
};
type SortGroup = Pick<GroupSummary, 'groupId' | 'parentGroupId' | 'sortOrder'>;

export function modDropTarget(
  items: readonly SortMod[], dragged: SortMod, target: SortMod,
  side: 'before' | 'after', groupBy: 'source' | 'profile',
): { groupId?: string; position: number } | undefined {
  if (dragged.profileModId === target.profileModId) return undefined;
  if (groupBy === 'source' && (dragged.groupId !== target.groupId || dragged.mod.sourceId !== target.mod.sourceId)) return undefined;
  let position = side === 'after' ? 1 : 0;
  for (const item of items) {
    if (item.groupId === target.groupId && item.profileModId !== dragged.profileModId
      && (item.sortOrder < target.sortOrder || (item.sortOrder === target.sortOrder && item.profileModId < target.profileModId))) position++;
  }
  return { groupId: target.groupId, position };
}

export function groupDropTarget(
  groups: readonly SortGroup[], dragged: SortGroup, target: SortGroup,
  side: 'before' | 'after', nest: boolean,
): { parentGroupId?: string; position: number } | undefined {
  if (dragged.groupId === target.groupId) return undefined;
  const parentGroupId = nest ? target.groupId : target.parentGroupId;
  const ancestors = new Set<string>();
  for (let ancestor = parentGroupId; ancestor && !ancestors.has(ancestor);) {
    ancestors.add(ancestor); ancestor = groups.find(item => item.groupId === ancestor)?.parentGroupId;
  }
  if (ancestors.has(dragged.groupId)) return undefined;
  let position = nest ? 0 : side === 'after' ? 1 : 0;
  if (!nest) for (const group of groups) {
    if (group.parentGroupId === parentGroupId && group.groupId !== dragged.groupId
      && (group.sortOrder < target.sortOrder || (group.sortOrder === target.sortOrder && group.groupId < target.groupId))) position++;
  }
  return { parentGroupId, position };
}
