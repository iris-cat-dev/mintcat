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

export function modMetadataLabels(tags: readonly string[], approvalStatus: string): {
  category: string; multiplayer: string; approval: string;
} {
  const category = tags.filter(tag => tag !== 'RequiredByAll' && tag !== 'RecommendedForAll' && tag !== 'Optional').join(' / ');
  const multiplayer = tags.includes('RequiredByAll') ? '全员必装'
    : tags.includes('RecommendedForAll') ? '推荐全员'
    : tags.includes('Optional') ? '客户端可选' : '—';
  const approval = approvalStatus === 'Verified' ? '已验证'
    : approvalStatus === 'Approved' ? '已批准'
    : approvalStatus === 'Sandbox' ? '沙盒'
    : approvalStatus || '—';
  return { category: category || '—', multiplayer, approval };
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
