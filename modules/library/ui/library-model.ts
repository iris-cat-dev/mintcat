import type {
  GroupSummary,
  ModSummary,
  DownloadOverlay,
  DownloadOverlayItem,
  ProfilesContentStateResult,
} from '@mintcat/sdk-ts';

export interface ModDownloadOverlay {
  operation: DownloadOverlay;
  item: DownloadOverlayItem;
}

export type ModStatusFilter = 'all' | 'enabled' | 'disabled' | 'pending' | 'updatable';
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
}

const MULTIPLAYER_TAGS = new Set(['RequiredByAll', 'RecommendedForAll', 'Optional']);
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

export function pendingVersionLabel(overlay: ModDownloadOverlay | undefined): string | undefined {
  if (!overlay?.item.pendingSelection) return undefined;
  const selection = overlay.item.pendingSelection;
  const version = selection.kind === 'auto'
    ? `auto → ${overlay.item.resolvedVersion}`
    : selection.versionId ?? overlay.item.resolvedVersion;
  const completed = overlay.item.downloadedBytes ?? overlay.operation.completedUnits;
  const total = overlay.item.totalBytes ?? overlay.operation.totalUnits;
  const progress = total === 0
    ? 0
    : Math.min(100, Math.floor((completed / total) * 100));
  const state = overlay.item.transferState ?? overlay.operation.state;
  return `Pending ${version} · ${state} · ${progress}%`;
}

export function extractGameVersionTags(tags: readonly string[]): string[] {
  return tags
    .map(tag => tag.trim())
    .filter(tag => VERSION_TAG_PATTERN.test(tag));
}

export function modMetadataLabels(tags: readonly string[], approvalStatus: string): {
  category: string; multiplayer: string; approval: string;
} {
  const category = tags
    .map(tag => tag.trim())
    .filter(tag =>
      tag.length > 0
      && !MULTIPLAYER_TAGS.has(tag)
      && !APPROVAL_TAGS.has(tag)
      && !VERSION_TAG_PATTERN.test(tag),
    )
    .join(' / ');
  const multiplayer = tags.includes('RequiredByAll') ? '全员必装'
    : tags.includes('RecommendedForAll') ? '推荐全员'
    : tags.includes('Optional') ? '客户端可选' : '—';
  const normalizedApproval = approvalStatus.trim();
  const approval = normalizedApproval === 'Verified' ? '已验证'
    : normalizedApproval === 'Approved' ? '已批准'
    : normalizedApproval === 'Sandbox' ? '沙盒'
    : !normalizedApproval || normalizedApproval === 'unavailable' ? '—'
    : normalizedApproval;
  return { category: category || '—', multiplayer, approval };
}

export function formatVersionText(versionId?: string, displayVersion?: string): string {
  const display = displayVersion?.trim();
  if (display) return display;
  if (!versionId) return '—';
  if (versionId === 'legacy-unversioned') return '未标明版本';
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
  if (facets.status === 'updatable' && !updatableIds.has(mod.profileModId)) return false;
  if (facets.sourceId && mod.mod.sourceId !== facets.sourceId) return false;
  const meta = modMetadataLabels(mod.tags, mod.approvalStatus);
  if (facets.approval) {
    if (facets.approval === 'none') {
      if (meta.approval !== '—') return false;
    } else if (mod.approvalStatus !== facets.approval) {
      return false;
    }
  }
  if (facets.multiplayer && meta.multiplayer !== facets.multiplayer) return false;
  if (facets.category) {
    const categories = meta.category === '—' ? [] : meta.category.split(' / ');
    if (!categories.includes(facets.category)) return false;
  }
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
