import type * as React from 'react';
import type { ReactElement, ReactNode } from 'react';
import type { MenuProps } from 'antd';
import type { GroupSummary, ModConflictItem, ModSummary, ProfileSummary, SourceSearchItem } from '@mintcat/sdk-ts';
import { getSharedRuntime, navigateUi, pickLocalPath, presentUiError } from '@mintcat/sdk-ui';
import {
  canEditProfileContent,
  extractGameVersionTags,
  formatVersionText,
  groupDropTarget,
  indexDownloadOverlays,
  matchesModFacets,
  modDisplayVersion,
  modDropTarget,
  modHasUpdate,
  modMetadataLabels,
  modSourceUrl,
  overlayProgressPercent,
  parseModUrlLines,
  pendingVersionLabel,
} from './library-model';
import type { ModFacetFilters, ModSortMode, ModStatusFilter } from './library-model';
import type { LibraryActions, LibraryState, LibraryTask, UrlBatchAddReport } from './library';

export type LibraryPage = 'mods' | 'downloads';
type ImportTab = 'local' | 'url' | 'subscribed';
interface LibraryViewProps {
  subscribe(listener: () => void): () => void;
  getSnapshot(): LibraryState;
  actions: LibraryActions;
  page: LibraryPage;
  toolbar?: HTMLElement;
}
type Dialog = { title: string; value: string; submit(value: string): Promise<void> };
type ContextMenuState = { x: number; y: number; items: NonNullable<MenuProps['items']>; onClick: (key: string) => void };
interface GroupTreeOption {
  value: string;
  title: ReactNode;
  children?: GroupTreeOption[];
}

const ROOT_GROUP_VALUE = '__root__';
const DEFAULT_FACETS: ModFacetFilters = {
  status: 'all',
  sourceId: '',
  approval: '',
  multiplayer: '',
  category: '',
};

export function LibraryView({ subscribe, getSnapshot, actions, page, toolbar }: LibraryViewProps): ReactElement {
  const { React, Antd, Icons, createPortal } = getSharedRuntime();
  const h = React.createElement;
  const state = React.useSyncExternalStore(subscribe, getSnapshot);
  const [multiSelect, setMultiSelect] = React.useState(false);
  const [selected, setSelected] = React.useState<string[]>([]);
  const [expanded, setExpanded] = React.useState<string[]>([]);
  const [settingsOpen, setSettingsOpen] = React.useState(false);
  const [profilesOpen, setProfilesOpen] = React.useState(false);
  const [importModalOpen, setImportModalOpen] = React.useState(false);
  const [importTab, setImportTab] = React.useState<ImportTab>('local');
  const [importTargetGroupId, setImportTargetGroupId] = React.useState<string | undefined>(undefined);
  const [urlInput, setUrlInput] = React.useState('');
  const [urlSubmitting, setUrlSubmitting] = React.useState(false);
  const [urlReport, setUrlReport] = React.useState<UrlBatchAddReport | undefined>();
  const [subscribedSelected, setSubscribedSelected] = React.useState<string[]>([]);
  const [conflictsOpen, setConflictsOpen] = React.useState(false);
  const [rootPath, setRootPath] = React.useState('');
  const [dialog, setDialog] = React.useState<Dialog | undefined>();
  const [dialogValue, setDialogValue] = React.useState('');
  const [selectionPaths, setSelectionPaths] = React.useState<Record<string, string>>({});
  const [facets, setFacets] = React.useState<ModFacetFilters>(DEFAULT_FACETS);
  const [facetsOpen, setFacetsOpen] = React.useState(false);
  const [groupBy, setGroupBy] = React.useState<'source' | 'profile'>(() => {
    try { return localStorage.getItem('mintcat.library.groupBy') === 'profile' ? 'profile' : 'source'; } catch { return 'source'; }
  });
  const setGroupByPersisted = (next: 'source' | 'profile'): void => {
    setGroupBy(next);
    try { localStorage.setItem('mintcat.library.groupBy', next); } catch { /* ignore storage errors */ }
  };
  const [collapsedSources, setCollapsedSources] = React.useState<string[]>([]);
  const [activeSort, setActiveSort] = React.useState<ModSortMode | undefined>(undefined);
  const [contextMenu, setContextMenu] = React.useState<ContextMenuState | undefined>(undefined);
  const lastClickedId = React.useRef<string | undefined>(undefined);
  const pointerDrag = React.useRef<{
    key: string;
    label: string;
    startX: number;
    startY: number;
    active: boolean;
    drop?: {
      kind: 'mod' | 'group';
      targetKey: string;
      side: 'before' | 'after' | 'inside';
      modTarget?: { groupId?: string; position: number };
      groupTarget?: { parentGroupId?: string; position: number };
    };
  } | undefined>(undefined);
  const suppressClickUntil = React.useRef(0);
  const [draggingKey, setDraggingKey] = React.useState<string | undefined>(undefined);
  const [dragGhost, setDragGhost] = React.useState<{ label: string; x: number; y: number } | undefined>(undefined);
  const [dropTarget, setDropTarget] = React.useState<{ key: string; side: 'before' | 'after' | 'inside' }>();
  const searchInput = React.useRef<HTMLInputElement>(null);
  const { modal } = Antd.App.useApp();
  const writable = canEditProfileContent(state.content) && !state.pending && !!state.mods;
  const items = state.mods?.items ?? [];
  const profile = state.profiles.find(item => item.profileId === state.profileId);
  const currentGame = state.games.find(game => game.gameId === state.gameId);
  const currentVariant = currentGame?.variants.find(item => item.variantId === state.variantId) ?? currentGame?.variants[0];
  const importSupported = !!currentGame?.localRemoteGameKey;
  const modioSupported = !!currentGame?.sourceIds.includes('modio');
  const canLaunchSteam = Boolean(currentVariant?.storeAppId);
  const overlays = state.content ? indexDownloadOverlays(state.content) : undefined;
  const terminalStates = ['succeeded', 'failed', 'cancelled', 'recovery-required'];
  const pendingIds = new Set(items.filter(mod => {
    const overlay = overlays?.get(mod.profileModId);
    return overlay && !terminalStates.includes(overlay.item.transferState ?? overlay.operation.state);
  }).map(mod => mod.profileModId));
  const updatableIds = new Set(items.filter(mod => {
    const check = state.updateChecks[mod.profileModId];
    return check ? check.hasUpdate : modHasUpdate(mod);
  }).map(mod => mod.profileModId));
  const conflictMap = new Map<string, ModConflictItem[]>();
  for (const conflict of state.conflicts) {
    for (const id of [conflict.winnerProfileModId, ...conflict.shadowedProfileModIds]) {
      const list = conflictMap.get(id) ?? [];
      list.push(conflict);
      conflictMap.set(id, list);
    }
  }
  const availableCategories = [...new Set(items.flatMap(mod => {
    const meta = modMetadataLabels(mod.tags, mod.approvalStatus);
    return meta.category === '—' ? [] : meta.category.split(' / ').filter(Boolean);
  }))].sort((a, b) => a.localeCompare(b));
  const availableSources = [...new Set(items.map(mod => mod.mod.sourceId))];
  const activeFacetCount = [facets.sourceId, facets.approval, facets.multiplayer, facets.category].filter(Boolean).length;
  const hasFacetFilter = activeFacetCount > 0;
  const visibleItems = items.filter(mod => matchesModFacets(mod, facets, pendingIds, updatableIds));
  const latestReceipt = state.receipts[0];
  const hasUnsavedChanges = Boolean(
    profile && state.installationId && items.some(mod => mod.enabled)
    && (!latestReceipt || latestReceipt.profileRevision !== profile.revision),
  );
  const toggleSelected = (id: string): void => setSelected(current => current.includes(id) ? current.filter(item => item !== id) : [...current, id]);
  const openContextMenu = (event: React.MouseEvent, menu: { items: NonNullable<MenuProps['items']>; onClick: (info: { key: string }) => void }): void => {
    event.preventDefault();
    event.stopPropagation();
    const menuWidth = 236;
    const estimatedHeight = menu.items.reduce((sum, item) => sum + (item && 'type' in item && item.type === 'divider' ? 9 : 34), 12);
    const menuHeight = Math.min(window.innerHeight - 24, Math.max(160, estimatedHeight));
    const x = Math.max(8, Math.min(event.clientX, window.innerWidth - menuWidth - 8));
    const y = Math.max(8, Math.min(event.clientY, window.innerHeight - menuHeight - 8));
    setContextMenu({ x, y, items: menu.items, onClick: key => { setContextMenu(undefined); menu.onClick({ key }); } });
  };
  React.useEffect(() => {
    if (!contextMenu) return;
    const onMouseDown = (event: MouseEvent): void => {
      const target = event.target;
      if (target instanceof Element && (target.closest('.library-context-menu') || target.closest('.ant-menu-submenu-popup'))) {
        return;
      }
      setContextMenu(undefined);
    };
    const onScroll = (event: Event): void => {
      const target = event.target;
      if (target instanceof Element && (target.closest('.library-context-menu') || target.closest('.ant-menu-submenu-popup'))) {
        return;
      }
      setContextMenu(undefined);
    };
    const onKey = (event: KeyboardEvent): void => { if (event.key === 'Escape') setContextMenu(undefined); };
    const dismiss = (): void => setContextMenu(undefined);
    window.addEventListener('mousedown', onMouseDown, true);
    window.addEventListener('scroll', onScroll, true);
    window.addEventListener('keydown', onKey);
    window.addEventListener('resize', dismiss);
    return () => {
      window.removeEventListener('mousedown', onMouseDown, true);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('resize', dismiss);
    };
  }, [contextMenu]);
  React.useEffect(() => {
    setSelected([]); setMultiSelect(false); setDialog(undefined); setSettingsOpen(false); setProfilesOpen(false);
    setImportModalOpen(false); setConflictsOpen(false); setContextMenu(undefined);
    setSelectionPaths({}); setFacets(DEFAULT_FACETS); setFacetsOpen(false);
    lastClickedId.current = undefined;
    pointerDrag.current = undefined; setDraggingKey(undefined); setDragGhost(undefined); setDropTarget(undefined);
  }, [state.profileId, state.gameId, state.generation]);
  React.useEffect(() => {
    if (page !== 'mods') return;
    const onOpenImport = (event: Event): void => {
      const detail = (event as CustomEvent<{ tab?: ImportTab; groupId?: string }>).detail;
      const nextTab = detail?.tab ?? 'local';
      setImportTab(nextTab);
      setImportTargetGroupId(detail?.groupId);
      setUrlInput('');
      setUrlReport(undefined);
      setSubscribedSelected([]);
      setImportModalOpen(true);
      if (nextTab === 'subscribed') actions.run(() => actions.loadSubscribed(false));
    };
    window.addEventListener('mintcat-open-import-modal', onOpenImport);
    return () => window.removeEventListener('mintcat-open-import-modal', onOpenImport);
  }, [page]);
  React.useEffect(() => { setExpanded(state.groups.filter(group => group.expanded).map(group => group.groupId)); }, [state.groups]);
  React.useEffect(() => { setRootPath(''); setCollapsedSources([]); }, [state.gameId]);
  React.useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault(); searchInput.current?.focus();
        return;
      }
      const target = event.target as HTMLElement | null;
      const inEditable = Boolean(target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable));
      if (!inEditable && page === 'mods' && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'a' && visibleItems.length > 0) {
        event.preventDefault();
        setMultiSelect(true);
        setSelected(visibleItems.map(item => item.profileModId));
      } else if (event.key === 'Escape' && !inEditable) {
        setContextMenu(undefined);
        if (selected.length > 0) setSelected([]);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [page, visibleItems, selected.length]);
  const icon = (name: keyof typeof Icons): ReactElement => h(Icons[name] as typeof Icons.HomeOutlined);
  const button = (title: string, name: keyof typeof Icons, action: (() => void) | undefined, disabled = false): ReactElement =>
    h(Antd.Tooltip, { title }, h(Antd.Button, { type: 'text', icon: icon(name), onClick: action, disabled, 'aria-label': title }));
  const ask = (title: string, value: string, submit: (value: string) => Promise<void>): void => {
    setDialog({ title, value, submit }); setDialogValue(value);
  };
  const confirm = (title: string, content: string, action: () => Promise<void>): void => {
    modal.confirm({ title, content, okText: '确定', cancelText: '取消', onOk: () => new Promise<void>((resolve, reject) => {
      actions.run(async () => { try { await action(); resolve(); } catch (error) { reject(error); throw error; } });
    }) });
  };
  const openImportModal = (tab: ImportTab = 'local', targetGroupId?: string): void => {
    setImportTab(tab);
    setImportTargetGroupId(targetGroupId);
    setUrlInput('');
    setUrlReport(undefined);
    setSubscribedSelected([]);
    setImportModalOpen(true);
    if (tab === 'subscribed') actions.run(() => actions.loadSubscribed(false));
  };
  const openUrlBatchModal = (targetGroupId?: string): void => openImportModal('url', targetGroupId);
  const groupTreeData = React.useMemo<GroupTreeOption[]>(() => {
    const visited = new Set<string>();
    const buildNodes = (parentId?: string): GroupTreeOption[] => state.groups
      .filter(group => group.parentGroupId === parentId)
      .sort((a, b) => a.sortOrder - b.sortOrder)
      .flatMap(group => {
        if (visited.has(group.groupId)) return [];
        visited.add(group.groupId);
        const children = buildNodes(group.groupId);
        return [{
          value: group.groupId,
          title: h('span', { className: 'library-tree-option' }, icon('FolderOutlined'), ' ', group.name),
          ...(children.length ? { children } : {}),
        }];
      });
    return [{
      value: ROOT_GROUP_VALUE,
      title: h('span', { className: 'library-tree-option' }, icon('HomeOutlined'), ' 根目录（未分组）'),
      children: buildNodes(undefined),
    }];
  }, [state.groups]);
  const sortMenuItems: NonNullable<MenuProps['items']> = [
    { key: 'name-asc', label: '名称 A → Z', icon: icon('SortAscendingOutlined') },
    { key: 'name-desc', label: '名称 Z → A', icon: icon('SortDescendingOutlined') },
    { type: 'divider' as const },
    { key: 'time-desc', label: '添加时间 新 → 旧', icon: icon('ClockCircleOutlined') },
    { key: 'time-asc', label: '添加时间 旧 → 新', icon: icon('ClockCircleOutlined') },
    { type: 'divider' as const },
    { key: 'approval-asc', label: '认证状态 已验证 → 沙盒', icon: icon('SafetyCertificateOutlined') },
    { key: 'approval-desc', label: '认证状态 沙盒 → 已验证', icon: icon('SafetyCertificateOutlined') },
  ];
  const triggerSort = (mode: ModSortMode): void => {
    setActiveSort(mode);
    actions.run(() => actions.sortMods(mode));
  };
  const groupChoices = [{ key: 'move:', label: '未分组', icon: icon('FolderOutlined') },
    ...state.groups.map(group => ({ key: `move:${group.groupId}`, label: group.name, icon: icon('FolderOutlined') }))];
  const moveWithin = (mod: ModSummary, offset: number): void => {
    const siblings = items.filter(item => item.groupId === mod.groupId).sort((a, b) => a.sortOrder - b.sortOrder);
    const position = siblings.findIndex(item => item.profileModId === mod.profileModId) + offset;
    if (position >= 0 && position < siblings.length) actions.run(() => actions.moveMod(mod, position, mod.groupId));
  };
  const modMenu = (mod: ModSummary) => {
    const url = modSourceUrl(mod);
    const isRemote = mod.mod.sourceId !== 'local';
    const currentSelectedIds = selected.filter(id => items.some(item => item.profileModId === id));
    const currentSelectedMods = items.filter(item => currentSelectedIds.includes(item.profileModId));
    const isBatchContext = currentSelectedIds.length >= 2 && currentSelectedIds.includes(mod.profileModId);
    const batchItems: NonNullable<MenuProps['items']> = isBatchContext ? [
      { key: 'batch-enable', label: `批量启用 (${currentSelectedIds.length})`, icon: icon('PlayCircleOutlined'), disabled: !writable },
      { key: 'batch-disable', label: `批量禁用 (${currentSelectedIds.length})`, icon: icon('PauseCircleOutlined'), disabled: !writable },
      { key: 'batch-update', label: `批量更新 (${currentSelectedMods.filter(m => m.mod.sourceId !== 'local').length})`, icon: icon('CloudDownloadOutlined'), disabled: !writable || !currentSelectedMods.some(m => m.mod.sourceId !== 'local') },
      { key: 'batch-move', label: `批量移动到分组 (${currentSelectedIds.length})`, icon: icon('DragOutlined'), disabled: !writable || !!state.query.trim(), children: groupChoices.map(choice => ({ ...choice, key: `batch-${choice.key}` })) },
      { key: 'batch-copy', label: `批量复制链接 (${currentSelectedIds.length})`, icon: icon('CopyOutlined'), disabled: !currentSelectedMods.some(m => Boolean(modSourceUrl(m))) },
      { key: 'batch-remove', label: `批量删除 (${currentSelectedIds.length})`, icon: icon('DeleteOutlined'), danger: true, disabled: !writable },
      { type: 'divider' as const },
    ] : [];
    return {
      items: [
        ...batchItems,
        { key: 'new-group', label: '新建分组', icon: icon('FolderAddOutlined'), disabled: !writable },
        ...(url ? [
          { key: 'open-url', label: '打开源主页', icon: icon('LinkOutlined') },
          { key: 'copy-url', label: '复制链接', icon: icon('CopyOutlined') },
        ] : []),
        { type: 'divider' as const },
        ...(isRemote ? [
          { key: 'check-update', label: '检查更新', icon: icon('SyncOutlined') },
          { key: 'update-latest', label: mod.appliedVersionId ? '更新到最新版' : '下载缓存最新版', icon: icon('CloudDownloadOutlined'), disabled: !writable },
        ] : []),
        { key: 'export', label: mod.appliedArtifactDigest ? '导出缓存文件' : '导出缓存文件（未下载）', icon: icon('ExportOutlined'), disabled: !mod.appliedArtifactDigest },
        { type: 'divider' as const },
        { key: 'rename', label: '重命名', icon: icon('EditOutlined') },
        { key: 'top', label: '置顶', icon: icon('VerticalAlignTopOutlined'), disabled: !writable || !!state.query.trim() },
        { key: 'up', label: '上移', icon: icon('ArrowUpOutlined'), disabled: !writable || !!state.query.trim() },
        { key: 'down', label: '下移', icon: icon('ArrowDownOutlined'), disabled: !writable || !!state.query.trim() },
        { key: 'move', label: '移动到', icon: icon('DragOutlined'), children: groupChoices, disabled: !writable || !!state.query.trim() },
        { key: 'remove', label: '删除', icon: icon('DeleteOutlined'), danger: true, disabled: !writable },
      ],
      onClick: ({ key }: { key: string }) => {
        if (key === 'batch-enable') actions.run(() => actions.setBatchEnabled(currentSelectedIds, true));
        else if (key === 'batch-disable') actions.run(() => actions.setBatchEnabled(currentSelectedIds, false));
        else if (key === 'batch-update') actions.run(() => actions.updateMods(currentSelectedMods.filter(m => m.mod.sourceId !== 'local').map(m => m.profileModId)));
        else if (key.startsWith('batch-move:')) { setGroupByPersisted('profile'); actions.run(() => actions.moveModsBatch(currentSelectedIds, key.slice(11) || undefined)); }
        else if (key === 'batch-copy') actions.run(() => actions.copyModLinks(currentSelectedMods));
        else if (key === 'batch-remove') confirm('批量删除 Mod', `删除选中的 ${currentSelectedIds.length} 个 Mod？`, () => actions.removeMods(currentSelectedIds));
        else if (key === 'new-group') { setGroupByPersisted('profile'); ask('新建分组', '', value => actions.createGroup(value)); }
        else if (key === 'open-url') actions.run(() => actions.openModPage(mod));
        else if (key === 'copy-url') actions.run(() => actions.copyModLinks([mod]));
        else if (key === 'check-update') actions.run(() => actions.checkUpdates([mod.profileModId]));
        else if (key === 'update-latest') actions.run(() => actions.updateMods([mod.profileModId]));
        else if (key === 'export') actions.run(() => actions.exportMod(mod));
        else if (key === 'rename') ask('重命名 Mod（共享名称）', mod.displayName, value => actions.renameMod(mod, value));
        else if (key === 'remove') confirm('删除 Mod', `从当前配置中删除 ${mod.displayName}？`, () => actions.removeMods([mod.profileModId]));
        else if (key === 'top') actions.run(() => actions.moveMod(mod, 0, mod.groupId));
        else if (key === 'up') moveWithin(mod, -1);
        else if (key === 'down') moveWithin(mod, 1);
        else if (key.startsWith('move:')) actions.run(() => actions.moveMod(mod, 0, key.slice(5) || undefined));
      },
    };
  };
  const sourceGroupMenu = (source: string, sourceMods: ModSummary[]) => {
    const ids = sourceMods.map(item => item.profileModId);
    const isLocal = source === 'local';
    const hasUrls = sourceMods.some(item => Boolean(modSourceUrl(item)));
    return {
      items: [
        ...(isLocal
          ? [
            { key: 'import-file', label: '导入本地文件', icon: icon('FileAddOutlined'), disabled: !writable || !importSupported },
            { key: 'import-dir', label: '导入本地目录', icon: icon('FolderOpenOutlined'), disabled: !writable || !importSupported },
          ]
          : [{ key: 'add-url', label: '通过链接批量添加 Mod', icon: icon('LinkOutlined'), disabled: !writable }]),
        { key: 'new-group', label: '新建配置分组', icon: icon('FolderAddOutlined'), disabled: !writable },
        { type: 'divider' as const },
        { key: 'enable-all', label: `全部启用 (${ids.length})`, icon: icon('PlayCircleOutlined'), disabled: !writable || !ids.length },
        { key: 'disable-all', label: `全部禁用 (${ids.length})`, icon: icon('PauseCircleOutlined'), disabled: !writable || !ids.length },
        ...(!isLocal ? [
          { key: 'check-all', label: `检查本组更新 (${ids.length})`, icon: icon('SyncOutlined'), disabled: !ids.length },
          { key: 'update-all', label: `更新本组全部 (${ids.length})`, icon: icon('CloudDownloadOutlined'), disabled: !writable || !ids.length },
        ] : []),
        ...(hasUrls ? [{ key: 'copy-all', label: '复制本组全部链接', icon: icon('CopyOutlined') }] : []),
        { type: 'divider' as const },
        { key: 'select-group', label: `选中本组全部 (${ids.length})`, icon: icon('CheckSquareOutlined'), disabled: !ids.length },
        { key: 'switch-profile', label: '切换到自定义配置分组视图', icon: icon('FolderOutlined') },
        { key: 'sort', label: '排序', icon: icon('SortAscendingOutlined'), disabled: !writable || !!state.query.trim(), children: sortMenuItems.map(item => item && 'key' in item && item.key ? { ...item, key: `sort:${String(item.key)}` } : item) },
      ],
      onClick: ({ key }: { key: string }) => {
        if (key === 'import-file') actions.run(() => actions.importLocal('file'));
        else if (key === 'import-dir') actions.run(() => actions.importLocal('directory'));
        else if (key === 'add-url') openUrlBatchModal(undefined);
        else if (key === 'new-group') { setGroupByPersisted('profile'); ask('新建分组', '', value => actions.createGroup(value)); }
        else if (key === 'enable-all') actions.run(() => actions.setBatchEnabled(ids, true));
        else if (key === 'disable-all') actions.run(() => actions.setBatchEnabled(ids, false));
        else if (key === 'check-all') actions.run(() => actions.checkUpdates(ids));
        else if (key === 'update-all') actions.run(() => actions.updateMods(ids));
        else if (key === 'copy-all') actions.run(() => actions.copyModLinks(sourceMods));
        else if (key === 'select-group') { setMultiSelect(true); setSelected(current => [...new Set([...current, ...ids])]); }
        else if (key === 'switch-profile') setGroupByPersisted('profile');
        else if (key.startsWith('sort:')) triggerSort(key.slice(5) as ModSortMode);
      },
    };
  };
  const tableAreaMenu = () => ({
    items: [
      { key: 'add-url', label: '通过链接批量添加 Mod', icon: icon('LinkOutlined'), disabled: !writable },
      { key: 'import-file', label: '导入本地文件', icon: icon('FileAddOutlined'), disabled: !writable || !importSupported },
      { key: 'import-dir', label: '导入本地目录', icon: icon('FolderOpenOutlined'), disabled: !writable || !importSupported },
      { type: 'divider' as const },
      { key: 'new-group', label: '新建配置分组', icon: icon('FolderAddOutlined'), disabled: !writable },
      { key: 'toggle-multi', label: multiSelect ? '退出批量多选' : '开启批量多选', icon: icon('CheckSquareOutlined') },
      { key: 'sort', label: '排序', icon: icon('SortAscendingOutlined'), disabled: !writable || !!state.query.trim(), children: sortMenuItems.map(item => item && 'key' in item && item.key ? { ...item, key: `sort:${String(item.key)}` } : item) },
      { type: 'divider' as const },
      { key: 'refresh', label: '刷新列表', icon: icon('SyncOutlined') },
    ],
    onClick: ({ key }: { key: string }) => {
      if (key === 'add-url') openUrlBatchModal(undefined);
      else if (key === 'import-file') actions.run(() => actions.importLocal('file'));
      else if (key === 'import-dir') actions.run(() => actions.importLocal('directory'));
      else if (key === 'new-group') { setGroupByPersisted('profile'); ask('新建分组', '', value => actions.createGroup(value)); }
      else if (key === 'toggle-multi') { setMultiSelect(value => !value); setSelected([]); }
      else if (key.startsWith('sort:')) triggerSort(key.slice(5) as ModSortMode);
      else if (key === 'refresh') actions.run(actions.refresh);
    },
  });
  const finishDrag = (): void => {
    pointerDrag.current = undefined;
    setDraggingKey(undefined);
    setDragGhost(undefined);
    setDropTarget(undefined);
  };
  const beginPointerDrag = (event: React.PointerEvent, key: string, label: string, fromHandle = false): void => {
    if (!writable || !!state.query.trim() || event.button !== 0) return;
    if (!fromHandle) {
      const target = event.target as HTMLElement | null;
      if (target?.closest('button, a, input, select, textarea, .ant-select, .ant-switch, .ant-checkbox-wrapper, .mod-row-menu')) {
        return;
      }
    }
    if (fromHandle) {
      event.preventDefault();
      event.stopPropagation();
    }
    pointerDrag.current = { key, label, startX: event.clientX, startY: event.clientY, active: false };
  };
  React.useEffect(() => {
    const onPointerMove = (event: PointerEvent): void => {
      const drag = pointerDrag.current;
      if (!drag) return;
      if (!drag.active) {
        if (Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) < 6) return;
        drag.active = true;
        setDraggingKey(drag.key);
        setContextMenu(undefined);
      }
      setDragGhost({ label: drag.label, x: event.clientX, y: event.clientY });
      const element = document.elementFromPoint(event.clientX, event.clientY);
      const modRow = element?.closest<HTMLTableRowElement>('tr[data-mod-id]');
      const groupRow = element?.closest<HTMLTableRowElement>('tr[data-group-id]');
      if (modRow) {
        const targetId = modRow.dataset.modId;
        const targetMod = items.find(item => item.profileModId === targetId);
        const draggedMod = items.find(item => `mod:${item.profileModId}` === drag.key);
        if (targetMod && draggedMod) {
          const bounds = modRow.getBoundingClientRect();
          const side: 'before' | 'after' = event.clientY > bounds.top + bounds.height / 2 ? 'after' : 'before';
          const resolved = modDropTarget(items, draggedMod, targetMod, side, groupBy);
          if (resolved) {
            const targetKey = `mod:${targetMod.profileModId}`;
            drag.drop = { kind: 'mod', targetKey, side, modTarget: resolved };
            setDropTarget(current => current?.key === targetKey && current.side === side ? current : { key: targetKey, side });
            return;
          }
        }
      } else if (groupRow) {
        const targetGroupId = groupRow.dataset.groupId;
        const targetGroup = state.groups.find(g => g.groupId === targetGroupId);
        const draggedMod = items.find(item => `mod:${item.profileModId}` === drag.key);
        const draggedGroup = state.groups.find(g => `group:${g.groupId}` === drag.key);
        if (targetGroup && draggedMod) {
          const targetKey = `group:${targetGroup.groupId}`;
          drag.drop = { kind: 'mod', targetKey, side: 'inside', modTarget: { groupId: targetGroup.groupId, position: 0 } };
          setDropTarget(current => current?.key === targetKey && current.side === 'inside' ? current : { key: targetKey, side: 'inside' });
          return;
        }
        if (targetGroup && draggedGroup) {
          const bounds = groupRow.getBoundingClientRect();
          const side: 'before' | 'after' = event.clientY > bounds.top + bounds.height / 2 ? 'after' : 'before';
          const resolved = groupDropTarget(state.groups, draggedGroup, targetGroup, side, event.shiftKey);
          if (resolved) {
            const visualSide = event.shiftKey ? 'inside' : side;
            const targetKey = `group:${targetGroup.groupId}`;
            drag.drop = { kind: 'group', targetKey, side: visualSide, groupTarget: resolved };
            setDropTarget(current => current?.key === targetKey && current.side === visualSide ? current : { key: targetKey, side: visualSide });
            return;
          }
        }
      }
      drag.drop = undefined;
      setDropTarget(undefined);
    };
    const onPointerUp = (): void => {
      const drag = pointerDrag.current;
      if (!drag) return;
      const wasActive = drag.active;
      const drop = drag.drop;
      const dragKey = drag.key;
      finishDrag();
      if (!wasActive) return;
      suppressClickUntil.current = Date.now() + 250;
      if (!drop || !writable || state.query.trim()) return;
      if (drop.kind === 'mod' && drop.modTarget) {
        const draggedMod = items.find(item => `mod:${item.profileModId}` === dragKey);
        if (draggedMod) actions.run(() => actions.moveMod(draggedMod, drop.modTarget!.position, drop.modTarget!.groupId));
      } else if (drop.kind === 'group' && drop.groupTarget) {
        const draggedGroup = state.groups.find(item => `group:${item.groupId}` === dragKey);
        if (draggedGroup) actions.run(() => actions.moveGroup(draggedGroup, drop.groupTarget!.parentGroupId, drop.groupTarget!.position));
      }
    };
    window.addEventListener('pointermove', onPointerMove);
    window.addEventListener('pointerup', onPointerUp);
    window.addEventListener('pointercancel', onPointerUp);
    return () => {
      window.removeEventListener('pointermove', onPointerMove);
      window.removeEventListener('pointerup', onPointerUp);
      window.removeEventListener('pointercancel', onPointerUp);
    };
  }, [items, state.groups, groupBy, writable, state.query]);
  const sourceDisplayNames: Record<string, string> = {
    modio: 'mod.io',
    nexusmods: 'Nexus Mods',
    modcat: 'ModCat',
    local: '本地导入',
  };
  const renderMod = (mod: ModSummary, order: number, visibleOrderIds: readonly string[], depth = 0): ReactElement => {
    const overlay = overlays?.get(mod.profileModId);
    const pending = pendingVersionLabel(overlay);
    const progressPct = overlayProgressPercent(overlay);
    const details = state.versions[mod.profileModId];
    const version = mod.versionSelection.kind === 'pinned' ? `pinned:${mod.versionSelection.versionId}` : 'auto';
    const metadata = modMetadataLabels(mod.tags, mod.approvalStatus);
    const gameVersions = extractGameVersionTags(mod.tags);
    const categories = metadata.category === '—' ? [] : metadata.category.split(' / ').filter(Boolean);
    const isLocalSource = mod.mod.sourceId === 'local';
    const rawDisplayVer = modDisplayVersion(mod);
    const friendlyVersionLabel = (versionId?: string, displayVersion?: string): string => {
      if (isLocalSource && !displayVersion?.trim() && (!versionId || versionId.startsWith('sha256:') || /^[a-f0-9]{8,}$/i.test(versionId))) {
        return '本地包';
      }
      return formatVersionText(versionId, displayVersion);
    };
    const displayVer = isLocalSource && !mod.appliedDisplayVersion?.trim() && !mod.currentDisplayVersion?.trim()
      && (!rawDisplayVer || /^[a-f0-9]{8,}$/i.test(rawDisplayVer))
      ? '本地包'
      : rawDisplayVer;
    const options = [{ value: 'auto', label: displayVer }, ...(details?.versions ?? []).map(item => ({
      value: `pinned:${item.versionId}`,
      label: `${friendlyVersionLabel(item.versionId, item.displayVersion)}${item.versionId === mod.appliedVersionId ? ' · 已应用' : item.current ? ' · 最新' : ''}`,
    }))];
    if (version !== 'auto' && !options.some(option => option.value === version)) {
      options.push({ value: version, label: friendlyVersionLabel(mod.versionSelection.versionId) });
    }
    const url = modSourceUrl(mod);
    const modConflicts = conflictMap.get(mod.profileModId) ?? [];
    const hasUpdate = updatableIds.has(mod.profileModId);
    const isOffline = !isLocalSource && mod.onlineAvailable === false;
    const isLocalMissing = isLocalSource && mod.localMissing === true;
    const rowStyle = progressPct !== undefined
      ? ({ '--library-progress': `${progressPct}%` } as React.CSSProperties)
      : undefined;
    const sourceKey = ['modio', 'nexusmods', 'modcat', 'local'].includes(mod.mod.sourceId) ? mod.mod.sourceId : 'other';
    return h('tr', {
      key: mod.profileModId,
      className: `library-mod-row${order % 2 === 0 ? ' library-row-alt' : ''}${!mod.enabled ? ' library-row-disabled' : ''}${selected.includes(mod.profileModId) ? ' library-row-selected' : ''}${pendingIds.has(mod.profileModId) ? ' library-row-busy' : ''}${progressPct !== undefined ? ' library-row-progress' : ''}${draggingKey === `mod:${mod.profileModId}` ? ' library-row-dragging' : ''}${depth > 0 ? ' library-row-nested' : ''}`,
      style: rowStyle,
      tabIndex: 0, 'data-mod-id': mod.profileModId, 'aria-selected': selected.includes(mod.profileModId),
      'data-drop-side': dropTarget?.key === `mod:${mod.profileModId}` ? dropTarget.side : undefined,
      onContextMenu: (event: React.MouseEvent) => {
        if (!selected.includes(mod.profileModId)) {
          setSelected([mod.profileModId]);
          lastClickedId.current = mod.profileModId;
        }
        openContextMenu(event, modMenu(mod));
      },
      onClick: (event: React.MouseEvent) => {
        if (Date.now() < suppressClickUntil.current) return;
        if (event.shiftKey && lastClickedId.current) {
          const from = visibleOrderIds.indexOf(lastClickedId.current);
          const to = visibleOrderIds.indexOf(mod.profileModId);
          if (from >= 0 && to >= 0) {
            const [start, end] = from < to ? [from, to] : [to, from];
            const range = visibleOrderIds.slice(start, end + 1);
            setSelected(current => [...new Set([...current, ...range])]);
            return;
          }
        }
        lastClickedId.current = mod.profileModId;
        if (multiSelect || event.ctrlKey || event.metaKey) toggleSelected(mod.profileModId);
        else setSelected([mod.profileModId]);
      },
      onPointerDown: (event: React.PointerEvent) => beginPointerDrag(event, `mod:${mod.profileModId}`, mod.displayName, false),
      onKeyDown: (event: React.KeyboardEvent<HTMLTableRowElement>) => {
        if (event.target !== event.currentTarget) return;
        if (event.key === ' ') {
          event.preventDefault();
          if (multiSelect) toggleSelected(mod.profileModId);
          else if (writable) actions.run(() => actions.setEnabled(mod, !mod.enabled));
        } else if (event.key === 'Enter') { event.preventDefault(); toggleSelected(mod.profileModId); }
        else if (event.key === 'Delete' && writable) {
          event.preventDefault(); confirm('删除 Mod', `从当前配置中删除 ${mod.displayName}？`, () => actions.removeMods([mod.profileModId]));
        } else if (event.altKey && writable && !state.query.trim() && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
          event.preventDefault(); moveWithin(mod, event.key === 'ArrowUp' ? -1 : 1);
        } else if (!event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
          event.preventDefault();
          const rows = Array.from(event.currentTarget.closest('table')?.querySelectorAll<HTMLTableRowElement>('tr[data-mod-id]') ?? []);
          rows[rows.indexOf(event.currentTarget) + (event.key === 'ArrowUp' ? -1 : 1)]?.focus();
        }
      },
    },
    h('td', { className: 'library-order-cell' }, h('div', { className: 'library-order-wrap' },
      writable && !state.query.trim()
        ? h('span', {
          className: 'library-drag-handle',
          title: '按住拖动排序',
          'aria-label': `拖动排序 ${mod.displayName}`,
          onPointerDown: (event: React.PointerEvent) => beginPointerDrag(event, `mod:${mod.profileModId}`, mod.displayName, true),
        }, icon('HolderOutlined'))
        : null,
      multiSelect
        ? h(Antd.Checkbox, { checked: selected.includes(mod.profileModId),
          onClick: (event: React.MouseEvent) => event.stopPropagation(), onChange: () => toggleSelected(mod.profileModId) },
          h('span', { className: 'library-sr-only' }, `选择 ${mod.displayName}`))
        : h('span', { className: 'mod-order' }, order))),
    h('td', { className: 'library-name-cell' },
      h('div', {
        className: 'library-mod-name',
        style: depth > 0 ? { paddingLeft: Math.min(depth, 4) * 18 } : undefined,
      },
        h(Antd.Tooltip, { title: `来源：${sourceDisplayNames[mod.mod.sourceId] ?? mod.mod.sourceId}` },
          h('span', { className: `library-mod-source-dot library-source-${sourceKey}`, 'aria-hidden': true })),
        url
          ? h(Antd.Tooltip, { title: `${mod.displayName} · 点击打开 ${sourceDisplayNames[mod.mod.sourceId] ?? mod.mod.sourceId} 主页` },
            h('button', {
              type: 'button',
              className: 'mod-name mod-name-link',
              onClick: (event: React.MouseEvent) => {
                event.stopPropagation();
                actions.run(() => actions.openModPage(mod));
              },
            }, h('span', { className: 'mod-name-text' }, mod.displayName), h('span', { className: 'mod-name-ext', 'aria-hidden': true }, icon('ExportOutlined'))))
          : h(Antd.Tooltip, { title: pending ?? `${mod.displayName} · ${sourceDisplayNames[mod.mod.sourceId] ?? mod.mod.sourceId}` },
            h('span', { className: 'mod-name' }, mod.displayName)),
        h('span', { className: 'library-row-indicators', onClick: (event: React.MouseEvent) => event.stopPropagation() },
          gameVersions.length > 0
            ? h(Antd.Tooltip, { title: `标注适配游戏版本：${gameVersions.join(', ')}` },
              h('span', { className: 'library-game-version-chip' }, gameVersions[0])) : null,
          modConflicts.length > 0
            ? h(Antd.Tooltip, { title: `存在 ${modConflicts.length} 处文件冲突（点击查看详情）：${modConflicts.slice(0, 3).map(c => c.relativePath).join('、')}` },
              h('button', {
                type: 'button',
                className: 'library-warn-icon library-warn-conflict',
                'aria-label': `${mod.displayName} 存在文件冲突`,
                onClick: () => setConflictsOpen(true),
              }, icon('WarningOutlined'))) : null,
          hasUpdate
            ? h(Antd.Tooltip, { title: `有新版本可用（${formatVersionText(mod.currentVersionId, mod.currentDisplayVersion)}），点击更新` },
              h('button', {
                type: 'button',
                className: 'library-warn-icon library-warn-update',
                'aria-label': `更新 ${mod.displayName}`,
                disabled: !writable,
                onClick: () => actions.run(() => actions.updateMods([mod.profileModId])),
              }, icon('CloudDownloadOutlined'))) : null,
          isOffline
            ? h(Antd.Tooltip, { title: '在线源不可用或已下架' },
              h('span', { className: 'library-warn-icon library-warn-offline', 'aria-label': '在线源不可用' }, icon('DisconnectOutlined'))) : null,
          isLocalMissing
            ? h(Antd.Tooltip, { title: '本地文件缺失' },
              h('span', { className: 'library-warn-icon library-warn-missing', 'aria-label': '本地文件缺失' }, icon('ExclamationCircleOutlined'))) : null,
          progressPct !== undefined
            ? h(Antd.Tooltip, { title: pending ?? `下载中 ${progressPct}%` },
              h('span', { className: 'library-progress-badge', 'aria-label': `进度 ${progressPct}%` }, `${progressPct}%`))
            : pending
              ? h(Antd.Tooltip, { title: pending }, h('span', { className: 'library-pending-dot', 'aria-label': pending })) : null))),
    h('td', { className: 'library-version-cell', onClick: (event: React.MouseEvent) => event.stopPropagation() },
      h(Antd.Select<string>, { size: 'small', variant: 'borderless', className: `library-version-select${mod.versionSelection.kind === 'pinned' ? ' is-pinned' : ''}`, value: version, options,
        popupMatchSelectWidth: false, disabled: !writable, loading: pendingIds.has(mod.profileModId),
        suffixIcon: !isLocalSource && mod.versionSelection.kind === 'pinned' ? icon('LockOutlined') : undefined,
        'aria-label': `${mod.displayName} 版本`,
        onOpenChange: (open: boolean) => { if (open) actions.run(() => actions.loadVersions(mod, true)); },
        title: mod.appliedVersionId
          ? `已应用 ${formatVersionText(mod.appliedVersionId, mod.appliedDisplayVersion)}`
          : mod.currentVersionId
            ? `库中最新 ${formatVersionText(mod.currentVersionId, mod.currentDisplayVersion)}；尚未下载或应用`
            : '没有已知版本',
        popupRender: (menu: ReactNode) => h('div', null, h('div', { className: 'library-version-hint' }, '自动最新 / 固定版本（展开时刷新远端）'), menu),
        onChange: (value: string) => actions.run(() => actions.setVersion(mod, value)) })),
    h('td', { className: 'library-category-cell' }, isLocalSource
      ? h('span', { className: 'library-category library-category-local', 'data-category': 'local' }, '本地')
      : !categories.length ? h('span', { className: 'library-empty-dash' }, '—')
      : h(Antd.Tooltip, { title: categories.join(' · ') },
        h('div', { className: 'library-category-list' },
          h('span', { className: 'library-category', 'data-category': categories[0] }, categories[0]),
          categories.length > 1 ? h('span', { className: 'library-category library-category-more' }, `+${categories.length - 1}`) : null))),
    h('td', { className: 'library-metadata', title: metadata.multiplayer }, metadata.multiplayer === '—' ? h('span', { className: 'library-empty-dash' }, '—')
      : h('span', { className: 'library-metadata-badge', 'data-requirement': metadata.multiplayer }, metadata.multiplayer)),
    h('td', { className: 'library-metadata', title: metadata.approval === '—' ? undefined : mod.approvalStatus }, metadata.approval === '—' ? h('span', { className: 'library-empty-dash' }, '—')
      : h('span', { className: 'library-metadata-badge', 'data-approval': mod.approvalStatus }, metadata.approval)),
    h('td', { className: 'library-switch-cell', onClick: (event: React.MouseEvent) => event.stopPropagation() },
      h(Antd.Switch, { size: 'small', checked: mod.enabled, disabled: !writable, ...{ 'aria-label': `启用 ${mod.displayName}` },
        onChange: (enabled: boolean) => actions.run(() => actions.setEnabled(mod, enabled)) })),
    h('td', { className: 'library-actions-cell', onClick: (event: React.MouseEvent) => event.stopPropagation() },
      h('span', { className: 'mod-row-menu' },
        h(Antd.Dropdown, { menu: modMenu(mod), trigger: ['click'] }, h(Antd.Button, { size: 'small', type: 'text', icon: icon('MoreOutlined'), 'aria-label': `${mod.displayName} 操作` })))));
  };
  const renderGroup = (group: GroupSummary, depth: number): ReactElement => {
    const descendants = new Set<string>([group.groupId]);
    for (let changed = true; changed;) {
      changed = false;
      for (const item of state.groups) if (item.parentGroupId && descendants.has(item.parentGroupId) && !descendants.has(item.groupId)) { descendants.add(item.groupId); changed = true; }
    }
    const destinations = [{ key: 'parent:', label: '根分组' }, ...state.groups.filter(item => !descendants.has(item.groupId))
      .map(item => ({ key: `parent:${item.groupId}`, label: item.name }))];
    const groupModIds = visibleItems.filter(mod => mod.groupId && descendants.has(mod.groupId)).map(mod => mod.profileModId);
    const menu = {
      items: [
        { key: 'add-url', label: '导入 / 添加模组到此分组', icon: icon('PlusOutlined'), disabled: !writable },
        { key: 'add-root', label: '新建根分组', icon: icon('FolderAddOutlined'), disabled: !writable },
        { key: 'add', label: '添加子分组', icon: icon('FolderAddOutlined'), disabled: !writable },
        { type: 'divider' as const },
        { key: 'enable-group', label: `全部启用本组 (${groupModIds.length})`, icon: icon('PlayCircleOutlined'), disabled: !writable || !groupModIds.length },
        { key: 'disable-group', label: `全部禁用本组 (${groupModIds.length})`, icon: icon('PauseCircleOutlined'), disabled: !writable || !groupModIds.length },
        { key: 'select-group', label: `选中本组全部 (${groupModIds.length})`, icon: icon('CheckSquareOutlined'), disabled: !groupModIds.length },
        { type: 'divider' as const },
        { key: 'rename', label: '重命名分组', icon: icon('EditOutlined'), disabled: !writable },
        { key: 'move', label: '移动到', icon: icon('DragOutlined'), children: destinations, disabled: !writable },
        { key: 'up', label: '上移', icon: icon('ArrowUpOutlined'), disabled: !writable },
        { key: 'down', label: '下移', icon: icon('ArrowDownOutlined'), disabled: !writable },
        { key: 'remove', label: '删除分组', icon: icon('DeleteOutlined'), danger: true, disabled: !writable },
      ],
      onClick: ({ key }: { key: string }) => {
        if (key === 'add-url') openImportModal('local', group.groupId);
        else if (key === 'add-root') ask('新建分组', '', value => actions.createGroup(value));
        else if (key === 'add') ask('添加子分组', '', value => actions.createGroup(value, group.groupId));
        else if (key === 'enable-group') actions.run(() => actions.setBatchEnabled(groupModIds, true));
        else if (key === 'disable-group') actions.run(() => actions.setBatchEnabled(groupModIds, false));
        else if (key === 'select-group') { setMultiSelect(true); setSelected(current => [...new Set([...current, ...groupModIds])]); }
        else if (key === 'rename') ask('重命名分组', group.name, value => actions.renameGroup(group, value));
        else if (key === 'remove') confirm('删除分组', `删除 ${group.name}？分组内容的处理由服务端规则决定。`, () => actions.deleteGroup(group));
        else if (key.startsWith('parent:')) actions.run(() => actions.moveGroup(group, key.slice(7) || undefined, 0));
        else if (key === 'up' || key === 'down') {
          const siblings = state.groups.filter(item => item.parentGroupId === group.parentGroupId).sort((a, b) => a.sortOrder - b.sortOrder);
          const position = siblings.findIndex(item => item.groupId === group.groupId) + (key === 'up' ? -1 : 1);
          if (position >= 0 && position < siblings.length) actions.run(() => actions.moveGroup(group, group.parentGroupId, position));
        }
      },
    };
    const open = expanded.includes(group.groupId);
    const groupMods = visibleItems.filter(mod => mod.groupId && descendants.has(mod.groupId));
    const groupEnabledCount = groupMods.filter(mod => mod.enabled).length;
    return h('tr', {
      key: `group:${group.groupId}`,
      className: `library-group-row${open ? ' is-open' : ''}${draggingKey === `group:${group.groupId}` ? ' library-row-dragging' : ''}`,
      'data-group-id': group.groupId,
      'data-drop-side': dropTarget?.key === `group:${group.groupId}` ? dropTarget.side : undefined,
      title: '右键打开分组菜单；按住拖动调整顺序；Shift 拖动分组移入目标分组',
      onContextMenu: (event: React.MouseEvent) => openContextMenu(event, menu),
      onPointerDown: (event: React.PointerEvent) => beginPointerDrag(event, `group:${group.groupId}`, group.name, false),
    }, h('td', { colSpan: 8 },
      h('div', { className: 'library-group-heading', style: { paddingLeft: depth * 20 } },
        writable && !state.query.trim()
          ? h('span', {
            className: 'library-drag-handle',
            title: '按住拖动排序分组（Shift 拖入可嵌套）',
            onPointerDown: (event: React.PointerEvent) => beginPointerDrag(event, `group:${group.groupId}`, group.name, true),
          }, icon('HolderOutlined'))
          : null,
        h('button', { type: 'button', className: 'library-group-toggle', 'aria-expanded': open, onClick: () => {
          if (Date.now() < suppressClickUntil.current) return;
          setExpanded(current => open ? current.filter(id => id !== group.groupId) : [...current, group.groupId]);
          if (writable) actions.run(() => actions.expandGroup(group, !open));
        } },
        h('span', { className: `library-chevron${open ? ' is-open' : ''}`, 'aria-hidden': true }, icon('RightOutlined')),
        h('span', { className: 'library-group-icon-badge', 'aria-hidden': true }, icon(open ? 'FolderOpenOutlined' : 'FolderOutlined')),
        h('span', { className: 'library-group-name' }, group.name)),
        h('span', { className: 'mod-group-count' }, groupModIds.length),
        h('span', { className: 'library-group-spacer' }),
        h('span', { className: `library-group-summary${groupModIds.length === 0 ? ' is-empty' : ''}` },
          groupModIds.length === 0 ? '空分组' : `${groupEnabledCount} / ${groupModIds.length} 已启用`),
        h('span', { className: 'mod-row-menu', onClick: (event: React.MouseEvent) => event.stopPropagation() }, h(Antd.Dropdown, { menu, trigger: ['click'] },
          h(Antd.Button, { type: 'text', size: 'small', icon: icon('MoreOutlined'), 'aria-label': `${group.name} 分组操作` }))))));
  };
  const sortedItems = [...visibleItems].sort((a, b) => a.sortOrder - b.sortOrder);
  const sourceOrder = ['modio', 'nexusmods', 'modcat', 'local'];
  const sources = [...new Set(sortedItems.map(mod => mod.mod.sourceId))].sort((a, b) => {
    const rankA = sourceOrder.indexOf(a), rankB = sourceOrder.indexOf(b);
    return (rankA < 0 ? sourceOrder.length : rankA) - (rankB < 0 ? sourceOrder.length : rankB) || a.localeCompare(b);
  });
  const collectOrderIds = (): string[] => {
    if (groupBy === 'source') {
      return sources.flatMap(source => collapsedSources.includes(source) ? [] : sortedItems.filter(mod => mod.mod.sourceId === source).map(mod => mod.profileModId));
    }
    const seen = new Set<string>();
    const ids: string[] = [];
    const walk = (parentGroupId?: string): void => {
      for (const group of state.groups.filter(g => g.parentGroupId === parentGroupId).sort((a, b) => a.sortOrder - b.sortOrder)) {
        if (seen.has(group.groupId)) continue;
        seen.add(group.groupId);
        if (expanded.includes(group.groupId)) walk(group.groupId);
      }
      for (const mod of sortedItems.filter(m => m.groupId === parentGroupId)) ids.push(mod.profileModId);
    };
    walk();
    return ids;
  };
  const visibleOrderIds = collectOrderIds();
  let displayOrder = 0;
  const visited = new Set<string>();
  const rowsFor = (parentGroupId?: string, depth = 0): ReactElement[] => {
    const rows: ReactElement[] = [];
    const childGroups = state.groups.filter(group => group.parentGroupId === parentGroupId).sort((a, b) => a.sortOrder - b.sortOrder);
    for (const group of childGroups) {
      if (visited.has(group.groupId)) continue;
      visited.add(group.groupId);
      rows.push(renderGroup(group, depth));
      if (expanded.includes(group.groupId)) rows.push(...rowsFor(group.groupId, depth + 1));
    }
    const directMods = sortedItems.filter(mod => mod.groupId === parentGroupId);
    if (!parentGroupId && childGroups.length > 0 && directMods.length > 0) {
      rows.push(h('tr', { key: 'ungrouped-divider', className: 'library-ungrouped-divider' },
        h('td', { colSpan: 8 },
          h('div', { className: 'library-ungrouped-inner' },
            h('span', { className: 'library-ungrouped-pill' }, icon('AppstoreOutlined'), `未分组模组 · ${directMods.length}`),
            h('span', { className: 'library-ungrouped-line', 'aria-hidden': true })))));
    }
    rows.push(...directMods.map(mod => renderMod(mod, ++displayOrder, visibleOrderIds, depth)));
    return rows;
  };
  const tableRows = groupBy === 'profile' ? rowsFor() : sources.flatMap(source => {
    const mods = sortedItems.filter(mod => mod.mod.sourceId === source);
    const count = items.filter(mod => mod.mod.sourceId === source).length;
    const enabledInSource = mods.filter(mod => mod.enabled).length;
    const collapsed = collapsedSources.includes(source);
    const menu = sourceGroupMenu(source, mods);
    return [h('tr', {
      key: `source:${source}`,
      className: `library-group-row${!collapsed ? ' is-open' : ''}`,
      title: '右键打开来源分组操作菜单',
      onContextMenu: (event: React.MouseEvent) => openContextMenu(event, menu),
    }, h('td', { colSpan: 8 },
      h('div', { className: 'library-group-heading' },
        h('button', { type: 'button', className: 'library-group-toggle', 'aria-expanded': !collapsed,
          onClick: () => setCollapsedSources(current => collapsed ? current.filter(id => id !== source) : [...current, source]) },
        h('span', { className: `library-chevron${!collapsed ? ' is-open' : ''}`, 'aria-hidden': true }, icon('RightOutlined')),
        h('span', { className: `library-source-dot library-source-${sourceOrder.includes(source) ? source : 'other'}` }),
        h('span', { className: 'library-group-name' }, sourceDisplayNames[source] ?? source)),
        h('span', { className: 'mod-group-count' }, mods.length === count ? count : `${mods.length} / ${count}`),
        h('span', { className: 'library-group-spacer' }),
        h('span', { className: 'library-group-summary' }, `${enabledInSource} / ${mods.length} 已启用`),
        h('span', { className: 'mod-row-menu', onClick: (event: React.MouseEvent) => event.stopPropagation() },
          h(Antd.Dropdown, { menu, trigger: ['click'] },
            h(Antd.Button, { type: 'text', size: 'small', icon: icon('MoreOutlined'), 'aria-label': `${sourceDisplayNames[source] ?? source} 分组操作` })))))),
      ...(collapsed ? [] : mods.map(mod => renderMod(mod, ++displayOrder, visibleOrderIds, 1)))];
  });
  const selectedIds = selected.filter(id => items.some(mod => mod.profileModId === id));
  const selectedMods = items.filter(mod => selectedIds.includes(mod.profileModId));
  const allSelected = visibleItems.length > 0 && visibleItems.every(mod => selectedIds.includes(mod.profileModId));
  const batchButton = (label: string, name: keyof typeof Icons, action: () => void, danger = false, disabledExtra = false): ReactElement => h(Antd.Button,
    { type: 'text', size: 'small', danger, icon: icon(name), disabled: !writable || !selectedIds.length || disabledExtra, onClick: action }, label);
  const formatByteSize = (bytes?: number): string => {
    if (typeof bytes !== 'number' || !Number.isFinite(bytes) || bytes < 0) return '';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  };
  const formatTaskTime = (iso?: string): string => {
    if (!iso) return '';
    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) return '';
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  };
  const describeCommand = (command: string): { label: string; iconName: keyof typeof Icons } => {
    const map: Record<string, { label: string; iconName: keyof typeof Icons }> = {
      'mods.importLocal': { label: '导入本地模组', iconName: 'ImportOutlined' },
      'mods.add': { label: '添加在线模组', iconName: 'CloudDownloadOutlined' },
      'mods.updateBatch': { label: '批量更新模组', iconName: 'CloudSyncOutlined' },
      'mods.setVersion': { label: '切换模组版本', iconName: 'SwapOutlined' },
      'mods.refreshVersions': { label: '刷新远端版本', iconName: 'ReloadOutlined' },
      'install.apply': { label: '应用配置到游戏', iconName: 'DeploymentUnitOutlined' },
      'install.uninstall': { label: '卸载已部署模组', iconName: 'DeleteOutlined' },
    };
    return map[command] ?? { label: `后台任务（${command}）`, iconName: 'ThunderboltOutlined' };
  };
  const describeJobState = (jobState: string): { label: string; color: string; tone: 'active' | 'success' | 'warning' | 'error' | 'muted' } => {
    switch (jobState) {
      case 'queued': return { label: '排队中', color: 'default', tone: 'active' };
      case 'preparing': return { label: '准备中', color: 'processing', tone: 'active' };
      case 'awaiting-approval': return { label: '等待确认', color: 'warning', tone: 'warning' };
      case 'running': return { label: '进行中', color: 'processing', tone: 'active' };
      case 'committing': return { label: '正在写入配置', color: 'processing', tone: 'active' };
      case 'succeeded': return { label: '已完成', color: 'success', tone: 'success' };
      case 'cancelling': return { label: '正在取消', color: 'warning', tone: 'warning' };
      case 'cancelled': return { label: '已取消', color: 'default', tone: 'muted' };
      case 'failed': return { label: '执行失败', color: 'error', tone: 'error' };
      case 'recovery-required': return { label: '已中断（需重试）', color: 'warning', tone: 'warning' };
      default: return { label: jobState, color: 'default', tone: 'muted' };
    }
  };
  const describePhase = (phase?: string): string => {
    if (!phase) return '处理中';
    const map: Record<string, string> = {
      queued: '等待调度',
      preparing: '正在解析模组元数据',
      'download-running': '正在下载模组文件',
      downloading: '正在下载模组文件',
      verifying: '正在校验文件完整性',
      committing: '正在写入配置',
      completed: '下载完成',
      succeeded: '已完成',
      failed: '下载失败',
      cancelled: '已取消',
      'recovery-required': '任务已中断，无法自动继续',
    };
    return map[phase] ?? phase;
  };
  const profileLabelOf = (profileId: string): string => {
    const matched = state.profiles.find(item => item.profileId === profileId);
    if (matched) return matched.displayName;
    return profileId.length > 18 ? `${profileId.slice(0, 14)}…` : profileId;
  };
  const renderTask = (task: LibraryTask): ReactElement => {
    const cmd = describeCommand(task.job.command);
    const st = describeJobState(task.job.state);
    const isTerminal = terminalStates.includes(task.job.state);
    const errorInfo = task.job.error ? presentUiError(task.job.error, 'host') : undefined;
    const timeText = formatTaskTime(task.job.updatedAt ?? task.job.createdAt);
    const techLines = [
      `任务命令：${task.job.command}`,
      `任务编号：${task.job.jobId}`,
      `目标配置：${task.profileId}`,
      `状态：${task.job.state}`,
      ...(errorInfo?.diagnostics ?? []),
    ];
    return h('article', { className: `library-task-card library-task-card--${st.tone}`, key: task.job.jobId },
      h('div', { className: 'library-task-head' },
        h('div', { className: 'library-task-title-wrap' },
          h('span', { className: `library-task-icon library-task-icon--${st.tone}`, 'aria-hidden': true }, icon(cmd.iconName)),
          h('div', { className: 'library-task-meta' },
            h('div', { className: 'library-task-title-row' },
              h('span', { className: 'library-task-title' }, cmd.label),
              h(Antd.Tag, { color: st.color, className: 'library-task-tag' }, st.label)),
            h('div', { className: 'library-task-sub' },
              h('span', null, `目标配置：${profileLabelOf(task.profileId)}`),
              timeText ? h('span', { className: 'library-task-time' }, `· ${timeText}`) : null))),
        h('div', { className: 'library-task-actions' },
          !isTerminal
            ? h(React.Fragment, null,
              h(Antd.Button, { size: 'small', onClick: () => actions.run(() => actions.cancelTask(task)) },
                task.job.state === 'cancelling' ? '强制结束' : '取消任务'),
              task.job.state === 'cancelling'
                ? h(Antd.Button, { size: 'small', type: 'text', icon: icon('CloseOutlined'),
                  onClick: () => actions.dismissTask(task.job.jobId) }, '移除记录')
                : null)
            : h(React.Fragment, null,
              task.job.command === 'mods.importLocal' && (task.job.state === 'failed' || task.job.state === 'recovery-required') && writable && importSupported
                ? h(Antd.Button, { size: 'small', type: 'primary', ghost: true, icon: icon('FileAddOutlined'),
                  onClick: () => { actions.dismissTask(task.job.jobId); actions.run(() => actions.importLocal('file')); } }, '重新导入')
                : null,
              h(Antd.Button, { size: 'small', type: 'text', icon: icon('CloseOutlined'),
                onClick: () => actions.dismissTask(task.job.jobId) }, '移除记录')))),
      errorInfo ? h('div', { className: `library-task-alert library-task-alert--${st.tone === 'error' ? 'error' : 'warning'}` },
        h('div', { className: 'library-task-alert-title' }, icon('InfoCircleOutlined'), ' ', errorInfo.title),
        h('p', { className: 'library-task-alert-desc' }, errorInfo.description)) : null,
      task.job.command === 'mods.importLocal' && task.action?.state === 'pending' && ['select-file', 'select-directory'].includes(task.action.kind)
        ? h('div', { className: 'library-input-selection' },
          h('div', { className: 'library-selection-title' },
            `等待您选择要导入的本地${task.action.kind === 'select-file' ? '模组文件（.zip / .pak）' : '模组目录'}：`),
          h(Antd.Space.Compact, { style: { width: '100%' } },
            h(Antd.Input, { placeholder: '点击右侧浏览，或粘贴本地绝对路径', 'aria-label': '导入选择路径', value: selectionPaths[task.job.jobId] ?? '',
              onChange: (event: React.ChangeEvent<HTMLInputElement>) => setSelectionPaths({ ...selectionPaths, [task.job.jobId]: event.target.value }) }),
            h(Antd.Button, { icon: icon('FolderOpenOutlined'), onClick: () => actions.run(async () => {
              const kind = task.action?.kind === 'select-directory' ? 'directory' : 'file';
              const picked = await pickLocalPath({ kind, title: kind === 'directory' ? '选择本地模组目录' : '选择本地模组文件' });
              if (picked) setSelectionPaths(prev => ({ ...prev, [task.job.jobId]: picked }));
            }) }, '浏览…')),
          h(Antd.Space, null,
            h(Antd.Button, { size: 'small', type: 'primary', className: 'glass-primary-action', disabled: !selectionPaths[task.job.jobId]?.trim(),
              onClick: () => actions.run(() => actions.approveSelection(task, selectionPaths[task.job.jobId])) }, '确认导入'),
            h(Antd.Button, { size: 'small', onClick: () => actions.run(() => actions.rejectSelection(task)) }, '取消选择'))) : null,
      h('details', { className: 'library-task-details' },
        h('summary', null, '查看技术详情（排障用）'),
        h('pre', null, techLines.join('\n'))));
  };
  const operations = state.content?.operations ?? [];
  const hasTerminalTasks = state.tasks.some(task => terminalStates.includes(task.job.state));
  const taskContent = h('div', { className: 'library-task-list' }, ...operations.map(operation => {
    const st = describeJobState(operation.state);
    const percent = operation.totalUnits ? Math.min(100, Math.floor(operation.completedUnits / operation.totalUnits * 100)) : 0;
    return h('article', { key: `operation:${operation.operationId}`, className: `library-task-card library-task-card--${st.tone}` },
      h('div', { className: 'library-task-head' },
        h('div', { className: 'library-task-title-wrap' },
          h('span', { className: `library-task-icon library-task-icon--${st.tone}`, 'aria-hidden': true }, icon('CloudDownloadOutlined')),
          h('div', { className: 'library-task-meta' },
            h('div', { className: 'library-task-title-row' },
              h('span', { className: 'library-task-title' }, describePhase(operation.phase)),
              h(Antd.Tag, { color: st.color, className: 'library-task-tag' }, st.label)),
            h('div', { className: 'library-task-sub' }, `进度：${operation.completedUnits} / ${operation.totalUnits} 个模组`))),
        operation.jobId && !terminalStates.includes(operation.state)
          ? h(Antd.Button, { size: 'small', onClick: () => actions.run(() => actions.cancelDownload(operation)) }, '取消下载') : null),
      h(Antd.Progress, { size: 'small', percent, status: operation.state === 'failed' ? 'exception' : undefined }),
      operation.items.length ? h('div', { className: 'library-operation-items' },
        ...operation.items.map(item => {
          const downloaded = formatByteSize(item.downloadedBytes);
          const total = formatByteSize(item.totalBytes);
          const sizeHint = downloaded && total ? `${downloaded} / ${total}` : downloaded;
          return h('div', { key: item.profileModId, className: 'library-operation-item' },
            h('span', { className: 'library-operation-item-name' }, items.find(mod => mod.profileModId === item.profileModId)?.displayName ?? item.profileModId),
            h('span', { className: 'library-operation-item-meta' },
              describePhase(item.transferState ?? operation.state),
              sizeHint ? ` · ${sizeHint}` : '',
              item.resolvedVersion ? ` · ${item.resolvedVersion}` : ''));
        })) : null);
  }), ...state.tasks.map(renderTask));
  const hasTasks = state.tasks.length > 0 || operations.length > 0;
  const activeTaskCount = state.tasks.filter(task => !terminalStates.includes(task.job.state) && task.job.state !== 'awaiting-approval').length
    + operations.filter(op => !terminalStates.includes(op.state)).length;
  const awaitingTaskCount = state.tasks.filter(task => task.job.state === 'awaiting-approval').length;
  const succeededTaskCount = state.tasks.filter(task => task.job.state === 'succeeded').length;
  const problemTaskCount = state.tasks.filter(task => task.job.state === 'failed' || task.job.state === 'recovery-required').length;
  const activeJobs = new Set([
    ...state.tasks.filter(task => !terminalStates.includes(task.job.state)).map(task => task.job.jobId),
    ...operations.filter(operation => !terminalStates.includes(operation.state)).map(operation => operation.jobId ?? operation.operationId),
  ]);
  const toolbarNode = h('div', { className: 'library-toolbar' },
    h('div', { className: 'library-game-control' }, h('span', { className: 'library-game-cover', 'aria-hidden': true }, icon('AppstoreOutlined')),
      h(Antd.Select<string>, { variant: 'borderless', className: 'library-game-select', 'aria-label': '游戏', value: state.gameId || undefined,
        popupMatchSelectWidth: false,
        options: state.games.map(game => ({ value: game.gameId, label: game.displayName })), onChange: (id: string) => actions.run(() => actions.selectGame(id)), placeholder: '选择游戏' })),
    h('label', { className: 'library-search' }, icon('SearchOutlined'),
      h('input', { ref: searchInput, type: 'search', 'aria-label': '搜索 Mod', placeholder: '搜索当前配置模组…', value: state.query,
        onChange: (event: React.ChangeEvent<HTMLInputElement>) => actions.search(event.target.value) }),
      h('kbd', { 'aria-hidden': true }, '⌘K')),
    h('div', { className: 'library-profile-control' }, h('span', { className: 'library-profile-label' }, icon('ProfileOutlined'), '配置'),
      h(Antd.Select<string>, { variant: 'borderless', className: 'library-profile-select', 'aria-label': '配置', value: state.profileId || undefined,
        popupMatchSelectWidth: false,
        options: state.profiles.map(item => ({ value: item.profileId, label: item.displayName })), onChange: (id: string) => actions.run(() => actions.selectProfile(id)), placeholder: '选择配置' })),
    h('div', { className: 'library-toolbar-actions' },
      h('div', { className: 'library-toolbar-icon-group' },
        button('导入 / 添加模组', 'PlusOutlined', () => openImportModal(importSupported ? 'local' : 'url'), !state.profileId || !writable),
        button('检查更新', 'CloudSyncOutlined', () => actions.run(() => actions.checkUpdates()), !state.profileId || state.checkingUpdates),
        button('冲突检测', 'SafetyOutlined', () => actions.run(async () => {
          await actions.checkConflicts();
        }), !state.profileId || state.checkingConflicts),
        button('刷新列表', 'SyncOutlined', () => actions.run(actions.refresh)),
        h(Antd.Dropdown, { trigger: ['click'], menu: {
          items: [
            { key: 'new-group', label: '新建分组', icon: icon('FolderAddOutlined'), disabled: !writable },
            { type: 'divider' as const },
            { key: 'copy-list', label: '复制列表全部链接', icon: icon('CopyOutlined'), disabled: !items.length },
            { key: 'cleanup', label: '清理缺失的本地 Mod', icon: icon('ClearOutlined'), disabled: !writable },
          ],
          onClick: ({ key }: { key: string }) => {
            if (key === 'new-group') { setGroupByPersisted('profile'); ask('新建分组', '', value => actions.createGroup(value)); }
            else if (key === 'copy-list') actions.run(() => actions.copyModLinks(visibleItems));
            else if (key === 'cleanup') confirm('清理缺失的本地 Mod', '检查并清理当前配置中已丢失的本地 Mod（不会影响在线 Mod）。', actions.cleanupMissing);
          },
        } }, button('更多工具', 'EllipsisOutlined', undefined, !state.profileId)),
        canLaunchSteam ? button('启动游戏', 'CaretRightOutlined', () => actions.run(actions.launchGame), !state.gameId) : null),
      h(Antd.Dropdown, {
        trigger: ['click'],
        menu: {
          items: [
            { key: 'quick-install', label: hasUnsavedChanges ? '保存并应用更改到游戏（有未保存变更）' : '应用配置到游戏', icon: icon('CheckCircleOutlined'), disabled: !writable || !profile || !state.installationId || !state.variantId },
            { key: 'install-page', label: '打开安装与打包页面', icon: icon('DeploymentUnitOutlined') },
            { type: 'divider' as const },
            { key: 'quick-uninstall', label: '卸载已部署模组', icon: icon('DeleteOutlined'), danger: true, disabled: !state.installationId || !state.receipts.length },
          ],
          onClick: ({ key }: { key: string }) => {
            if (key === 'quick-install') actions.run(actions.quickInstall);
            else if (key === 'install-page') navigateUi('install');
            else if (key === 'quick-uninstall') actions.run(actions.quickUninstall);
          },
        },
      }, h(Antd.Button, {
        type: 'primary',
        className: `library-install-button glass-primary-action${hasUnsavedChanges ? ' library-install-unsaved' : ''}`,
        icon: icon('DeploymentUnitOutlined'),
        disabled: !profile || !state.variantId,
      }, hasUnsavedChanges ? '应用更改' : '安装模组', hasUnsavedChanges ? h('span', { className: 'library-unsaved-dot', 'aria-hidden': true }) : null))));
  const showBannerMessage = Boolean(state.message && (state.games.length > 0 && (page !== 'mods' || state.profileId || state.candidates.length > 0)));
  const statusPills: Array<{ value: ModStatusFilter; label: string; count: number }> = [
    { value: 'all', label: '全部', count: items.length },
    { value: 'enabled', label: '已启用', count: items.filter(mod => mod.enabled).length },
    { value: 'disabled', label: '已禁用', count: items.filter(mod => !mod.enabled).length },
    { value: 'pending', label: '待处理', count: pendingIds.size },
    ...(updatableIds.size > 0 ? [{ value: 'updatable' as const, label: '有更新', count: updatableIds.size }] : []),
  ];
  return h('div', { className: `mod-list-page-card library-page-${page}` },
    toolbar ? createPortal(toolbarNode, toolbar, `library-toolbar:${state.generation}`) : toolbarNode,
    showBannerMessage ? h(Antd.Alert, { type: 'info', message: state.message, showIcon: true, closable: true, className: 'library-status' }) : null,
    state.content && !canEditProfileContent(state.content) ? h(Antd.Alert, { type: 'warning', showIcon: true, className: 'library-status',
      message: state.content.recoveryRequired ? '配置需要恢复，内容修改已锁定' : `后台任务 ${state.content.busyOperationId ?? ''} 正在修改内容`,
      description: '搜索、切换配置和共享重命名仍然可用。' }) : null,
    page === 'mods' ? h(React.Fragment, null,
      state.games.length ? h('div', { className: 'library-view-bar' },
        h('div', { className: 'library-view-left' },
          h('div', { className: 'library-filter-pills', role: 'group', 'aria-label': '模组筛选' },
            ...statusPills.map(item =>
              h('button', { key: item.value, type: 'button', className: `library-filter-pill${facets.status === item.value ? ' library-filter-active' : ''}`,
                'aria-pressed': facets.status === item.value, onClick: () => setFacets(current => ({ ...current, status: item.value })) }, item.label, h('span', null, item.count)))),
          items.length > 0 ? h('button', {
            type: 'button',
            className: `library-facet-toggle${facetsOpen || hasFacetFilter ? ' library-facet-toggle--active' : ''}`,
            'aria-expanded': facetsOpen || hasFacetFilter,
            onClick: () => setFacetsOpen(open => !open),
          }, icon('FilterOutlined'), '筛选', activeFacetCount > 0 ? h('span', { className: 'library-facet-badge' }, activeFacetCount) : null) : null),
        h('div', { className: 'library-view-controls' },
          updatableIds.size > 0 ? h(Antd.Button, {
            size: 'small', type: 'primary', ghost: true, icon: icon('CloudDownloadOutlined'), disabled: !writable,
            onClick: () => actions.run(() => actions.updateMods([...updatableIds])),
          }, `一键更新 (${updatableIds.size})`) : null,
          state.conflicts.length > 0 ? h(Antd.Button, {
            size: 'small', danger: true, icon: icon('WarningOutlined'),
            onClick: () => setConflictsOpen(true),
          }, `冲突 (${state.conflicts.length})`) : null,
          h('div', { className: 'library-groupby-segmented', role: 'group', 'aria-label': '分组方式' },
            h('button', {
              type: 'button',
              className: `library-groupby-opt${groupBy === 'source' ? ' is-active' : ''}`,
              'aria-pressed': groupBy === 'source',
              title: '按模组来源自动归类，支持同组拖动排序',
              onClick: () => { finishDrag(); setGroupByPersisted('source'); },
            }, '按来源'),
            h('button', {
              type: 'button',
              className: `library-groupby-opt${groupBy === 'profile' ? ' is-active' : ''}`,
              'aria-pressed': groupBy === 'profile',
              title: '自定义文件夹分组，支持拖动排序与 Shift 嵌套',
              onClick: () => { finishDrag(); setGroupByPersisted('profile'); },
            }, '配置分组')),
          items.length > 0 ? h(Antd.Dropdown, {
            trigger: ['click'],
            disabled: !writable || !!state.query.trim(),
            menu: {
              items: sortMenuItems,
              onClick: ({ key }: { key: string }) => triggerSort(key as ModSortMode),
            },
          }, h(Antd.Button, {
            size: 'small',
            icon: icon('SortAscendingOutlined'),
            disabled: !writable || !!state.query.trim(),
          }, '排序')) : null,
          h('span', { className: 'library-controls-divider', 'aria-hidden': true }),
          button('管理配置', 'EditOutlined', () => setProfilesOpen(true)),
          button('游戏设置与安装目录', 'SettingOutlined', () => setSettingsOpen(true)),
          button('下载管理', 'CloudDownloadOutlined', () => navigateUi('downloads')))) : null,
      state.games.length && items.length > 0 && (facetsOpen || hasFacetFilter) ? h('div', { className: 'library-facet-drawer', role: 'region', 'aria-label': '高级筛选条件' },
        h('span', { className: 'library-facet-drawer-label' }, icon('FilterOutlined'), '条件筛选'),
        h('div', { className: 'library-facet-filters' },
          h(Antd.Select<string>, {
            size: 'small', variant: 'borderless', allowClear: true, placeholder: '来源', className: 'library-facet-select',
            'aria-label': '按来源筛选', popupMatchSelectWidth: false,
            value: facets.sourceId || undefined,
            options: availableSources.map(src => ({ value: src, label: sourceDisplayNames[src] ?? src })),
            onChange: value => setFacets(current => ({ ...current, sourceId: value ?? '' })),
          }),
          h(Antd.Select<string>, {
            size: 'small', variant: 'borderless', allowClear: true, placeholder: '认证', className: 'library-facet-select',
            'aria-label': '按认证筛选', popupMatchSelectWidth: false,
            value: facets.approval || undefined,
            options: [
              { value: 'Verified', label: '已验证' },
              { value: 'Approved', label: '已批准' },
              { value: 'Sandbox', label: '沙盒' },
              { value: 'none', label: '未分级' },
            ],
            onChange: value => setFacets(current => ({ ...current, approval: value ?? '' })),
          }),
          h(Antd.Select<string>, {
            size: 'small', variant: 'borderless', allowClear: true, placeholder: '联机', className: 'library-facet-select',
            'aria-label': '按联机要求筛选', popupMatchSelectWidth: false,
            value: facets.multiplayer || undefined,
            options: [
              { value: '全员必装', label: '全员必装' },
              { value: '推荐全员', label: '推荐全员' },
              { value: '客户端可选', label: '客户端可选' },
            ],
            onChange: value => setFacets(current => ({ ...current, multiplayer: value ?? '' })),
          }),
          availableCategories.length > 0 ? h(Antd.Select<string>, {
            size: 'small', variant: 'borderless', allowClear: true, showSearch: true, placeholder: '类别', className: 'library-facet-select',
            'aria-label': '按类别筛选', popupMatchSelectWidth: false,
            value: facets.category || undefined,
            options: availableCategories.map(cat => ({ value: cat, label: cat })),
            onChange: value => setFacets(current => ({ ...current, category: value ?? '' })),
          }) : null),
        hasFacetFilter ? h(Antd.Button, {
          size: 'small', type: 'link', className: 'library-facet-reset',
          onClick: () => setFacets(current => ({ ...DEFAULT_FACETS, status: current.status })),
        }, '清空条件') : null,
        h('span', { className: 'library-footer-spacer' }),
        h(Antd.Button, {
          size: 'small', type: 'text', className: 'library-facet-close',
          onClick: () => setFacetsOpen(false),
        }, '收起')) : null,
      hasTasks ? h(Antd.Collapse, { size: 'small', className: 'library-downloads', items: [{
        key: 'tasks',
        label: `后台任务 ${state.tasks.length} · 下载操作 ${operations.length}`,
        extra: hasTerminalTasks ? h(Antd.Button, {
          size: 'small', type: 'link',
          onClick: (event: React.MouseEvent) => {
            event.stopPropagation();
            actions.clearCompletedTasks();
          },
        }, '清除已完成') : undefined,
        children: taskContent,
      }] }) : null,
      h('div', {
        className: `library-table-area${!tableRows.length ? ' library-table-area--empty' : ''}${items.length > 0 && (multiSelect || selectedIds.length >= 2) ? ' library-table-area--dock-open' : ''}`,
        onContextMenu: (event: React.MouseEvent) => {
          if (state.profileId) openContextMenu(event, tableAreaMenu());
        },
      },
        state.loading ? h('div', { className: 'library-loading', role: 'status' }, h(Antd.Spin, { size: 'small' }), ' 加载中…') : null,
        tableRows.length ? h('table', { className: 'library-mod-table', 'aria-label': '模组列表' },
          h('colgroup', null, ...['order', 'name', 'version', 'category', 'multiplayer', 'approval', 'enabled', 'actions'].map(name => h('col', { key: name, className: `library-col-${name}` }))),
          h('thead', null, h('tr', null,
            h('th', { scope: 'col' }, multiSelect
              ? h(Antd.Checkbox, {
                checked: allSelected,
                indeterminate: selectedIds.length > 0 && !allSelected,
                onChange: () => setSelected(allSelected ? [] : visibleItems.map(mod => mod.profileModId)),
              }, h('span', { className: 'library-sr-only' }, '全选可见模组'))
              : '#'),
            h('th', { scope: 'col' }, h('button', {
              type: 'button',
              className: 'library-sortable-th',
              disabled: !writable || !!state.query.trim(),
              title: '点击按名称排序',
              onClick: () => triggerSort(activeSort === 'name-asc' ? 'name-desc' : 'name-asc'),
            }, '名称', icon(activeSort === 'name-desc' ? 'SortDescendingOutlined' : 'SortAscendingOutlined'))),
            h('th', { scope: 'col' }, '版本'),
            h('th', { scope: 'col' }, '类别'),
            h('th', { scope: 'col' }, '多人联机'),
            h('th', { scope: 'col' }, h('button', {
              type: 'button',
              className: 'library-sortable-th',
              disabled: !writable || !!state.query.trim(),
              title: '点击按认证状态排序',
              onClick: () => triggerSort(activeSort === 'approval-asc' ? 'approval-desc' : 'approval-asc'),
            }, '认证', icon(activeSort === 'approval-desc' ? 'SortDescendingOutlined' : 'SortAscendingOutlined'))),
            h('th', { scope: 'col', className: 'library-th-center' }, '启用'),
            h('th', { scope: 'col', 'aria-label': '操作' }))),
          h('tbody', null, ...tableRows)) : null,
        !tableRows.length && !state.loading ? h('div', { className: 'mc-empty-hero' },
          h('div', { className: 'mc-empty-hero-card' },
            h('div', { className: 'mc-empty-hero-orb', 'aria-hidden': true }, icon(!state.games.length ? 'AppstoreAddOutlined' : state.profileId ? 'InboxOutlined' : 'FolderOpenOutlined')),
            h('h3', { className: 'mc-empty-hero-title' }, !state.games.length ? '尚未安装游戏支持插件' : state.profileId ? (facets.status === 'all' && !hasFacetFilter ? '当前配置暂无模组' : '没有符合筛选的模组') : '开始配置您的游戏目录'),
            h('p', { className: 'mc-empty-hero-desc' }, !state.games.length
              ? '尚未安装游戏支持插件。安装游戏支持后即可注册游戏目录并管理模组配置；备份和 Agent 仍可使用。'
              : state.profileId
                ? (facets.status === 'all' && !hasFacetFilter ? '您可以从在线模组来源一键添加、粘贴 URL 批量添加，或直接拖拽/导入本地模组文件。' : '当前筛选条件下没有匹配的模组，请切换或重置筛选条件查看。')
                : (state.installations.length ? '已注册游戏安装目录，请新建或选择一个配置开始管理模组。' : '请先在游戏设置中发现或注册真实游戏安装目录，再创建模组配置。')),
            h('div', { className: 'mc-empty-hero-actions' },
              !state.games.length ? h(React.Fragment, null,
                h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: icon('RobotOutlined'), onClick: () => navigateUi('robot') }, '打开 Agent 助手'),
                h(Antd.Button, { icon: icon('CloudOutlined'), onClick: () => navigateUi('cloud') }, '备份与恢复')) : null,
              state.games.length && state.profileId ? h(React.Fragment, null,
                h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: icon('CompassOutlined'), onClick: () => navigateUi('search') }, '浏览在线模组'),
                h(Antd.Button, { icon: icon('PlusOutlined'), disabled: !writable, onClick: () => openImportModal(importSupported ? 'local' : 'url') }, '导入 / 添加模组')) : null,
              state.games.length && !state.profileId ? h(React.Fragment, null,
                h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: icon('SettingOutlined'), onClick: () => setSettingsOpen(true) }, '配置游戏目录'),
                h(Antd.Button, { icon: icon('EditOutlined'), onClick: () => setProfilesOpen(true) }, '管理配置')) : null),
            !state.games.length ? h('div', { className: 'mc-empty-bento' },
              h('div', { className: 'mc-empty-bento-item' },
                h('div', { className: 'mc-empty-bento-head' }, icon('AppstoreOutlined'), '多配置隔离管理'),
                h('span', { className: 'mc-empty-bento-text' }, '安装游戏插件后支持多套配置切换、拖拽排序与版本固定。')),
              h('button', { type: 'button', className: 'mc-empty-bento-item', onClick: () => navigateUi('cloud') },
                h('div', { className: 'mc-empty-bento-head' }, icon('CloudUploadOutlined'), '备份与旧版迁移'),
                h('span', { className: 'mc-empty-bento-text' }, '可从旧版数据库或离线便携归档恢复已有模组清单。')),
              h('button', { type: 'button', className: 'mc-empty-bento-item', onClick: () => navigateUi('robot') },
                h('div', { className: 'mc-empty-bento-head' }, icon('RobotOutlined'), 'Agent 协同管理'),
                h('span', { className: 'mc-empty-bento-text' }, '辅助整理模组方案，所有部署动作均由人工最终确认。'))) : null)) : null)) :
      h('div', { className: 'library-management-page' },
        h('div', { className: 'library-management-heading' },
          h('div', null, h('h2', null, '下载管理'),
            h('p', null, '查看模组下载、更新与本地导入的实时进度，支持取消进行中的任务或清理历史记录。')),
          state.games.length ? h('div', { className: 'library-view-controls' }, button('管理配置', 'EditOutlined', () => setProfilesOpen(true)),
            button('游戏设置与安装目录', 'SettingOutlined', () => setSettingsOpen(true))) : null),
        h('div', { className: 'library-task-stats' },
          h('div', { className: 'library-task-stat' },
            h('span', { className: 'library-task-stat-label' }, '正在进行'),
            h('strong', { className: 'library-task-stat-value library-task-stat-value--active' }, activeTaskCount)),
          h('div', { className: 'library-task-stat' },
            h('span', { className: 'library-task-stat-label' }, '等待确认'),
            h('strong', { className: 'library-task-stat-value library-task-stat-value--warning' }, awaitingTaskCount)),
          h('div', { className: 'library-task-stat' },
            h('span', { className: 'library-task-stat-label' }, '已完成'),
            h('strong', { className: 'library-task-stat-value library-task-stat-value--success' }, succeededTaskCount)),
          h('div', { className: 'library-task-stat' },
            h('span', { className: 'library-task-stat-label' }, '异常 / 已中断'),
            h('strong', { className: 'library-task-stat-value library-task-stat-value--error' }, problemTaskCount))),
        h('section', { className: 'library-jobs-panel' },
          h('div', { className: 'library-panel-header' },
            h('h3', null, `后台任务 (${state.tasks.length}) · 活跃下载 (${operations.length})`),
            hasTerminalTasks ? h(Antd.Button, { size: 'small', icon: icon('ClearOutlined'), onClick: () => actions.clearCompletedTasks() }, '清空已结束记录') : null),
          hasTasks ? taskContent : h('div', { className: 'library-tasks-empty' },
            h('div', { className: 'library-tasks-empty-orb', 'aria-hidden': true }, icon('CloudDownloadOutlined')),
            h('div', { className: 'library-tasks-empty-title' }, '当前没有进行中的下载或后台任务'),
            h('div', { className: 'library-tasks-empty-desc' }, '在线添加、批量更新或本地导入的进度会在这里实时显示。'),
            h(Antd.Space, { size: 8 },
              h(Antd.Button, { size: 'small', type: 'primary', className: 'glass-primary-action', icon: icon('CompassOutlined'), onClick: () => navigateUi('search') }, '浏览在线模组'),
              h(Antd.Button, { size: 'small', icon: icon('ImportOutlined'), onClick: () => navigateUi('local') }, '导入本地模组'))))),
    page === 'mods' && items.length > 0 && (multiSelect || selectedIds.length >= 2) ? h('div', { className: 'library-floating-dock-wrap' },
      h('div', { className: 'library-floating-dock', role: 'toolbar', 'aria-label': '批量操作栏' },
        h('span', { className: 'library-dock-count' }, `已选 ${selectedIds.length} / ${visibleItems.length}`),
        h(Antd.Button, { size: 'small', type: 'text', icon: icon(allSelected ? 'MinusSquareOutlined' : 'CheckSquareOutlined'),
          onClick: () => setSelected(allSelected ? [] : visibleItems.map(mod => mod.profileModId)) }, allSelected ? '取消全选' : '全选'),
        h(Antd.Button, { size: 'small', type: 'text',
          onClick: () => setSelected(visibleItems.filter(mod => !selectedIds.includes(mod.profileModId)).map(mod => mod.profileModId)) }, '反选'),
        h('span', { className: 'library-controls-divider', 'aria-hidden': true }),
        batchButton('启用', 'PlayCircleOutlined', () => actions.run(() => actions.setBatchEnabled(selectedIds, true))),
        batchButton('禁用', 'PauseCircleOutlined', () => actions.run(() => actions.setBatchEnabled(selectedIds, false))),
        batchButton('更新', 'CloudDownloadOutlined', () => actions.run(() => actions.updateMods(selectedMods.filter(m => m.mod.sourceId !== 'local').map(m => m.profileModId))), false, !selectedMods.some(m => m.mod.sourceId !== 'local')),
        h(Antd.Dropdown, {
          trigger: ['click'],
          disabled: !writable || !selectedIds.length || !!state.query.trim(),
          menu: {
            items: groupChoices,
            onClick: ({ key }: { key: string }) => {
              if (key.startsWith('move:')) {
                setGroupByPersisted('profile');
                actions.run(() => actions.moveModsBatch(selectedIds, key.slice(5) || undefined));
              }
            },
          },
        }, h(Antd.Button, { type: 'text', size: 'small', icon: icon('DragOutlined'), disabled: !writable || !selectedIds.length || !!state.query.trim() }, '移动到分组')),
        batchButton('复制链接', 'CopyOutlined', () => actions.run(() => actions.copyModLinks(selectedMods)), false, !selectedMods.some(mod => Boolean(modSourceUrl(mod)))),
        batchButton('删除', 'DeleteOutlined', () => confirm('批量删除 Mod', `删除选中的 ${selectedIds.length} 个 Mod？`, () => actions.removeMods(selectedIds)), true),
        h('span', { className: 'library-controls-divider', 'aria-hidden': true }),
        h(Antd.Button, { size: 'small', type: 'text', icon: icon('CloseOutlined'), onClick: () => { setMultiSelect(false); setSelected([]); } }, '退出'))) : null,
    page === 'mods' && state.games.length ? h('div', { className: 'home-footer-bar' },
      items.length ? h(Antd.Checkbox, { checked: multiSelect, onChange: (event: { target: { checked: boolean } }) => { setMultiSelect(event.target.checked); setSelected([]); } }, '批量选择') : null,
      items.length ? h('span', { className: 'library-footer-tip' }, '支持右键菜单 · 拖动排序 · ⌘A 全选') : null,
      h('span', { className: 'library-footer-spacer' }),
      hasUnsavedChanges ? h('span', { className: 'library-unsaved-badge', role: 'status' }, '有未应用变更') : null,
      h('span', { className: 'library-footer-status', role: 'status' },
        state.loading ? '正在加载' : state.pending ? '正在保存' : state.checkingUpdates ? '正在检查更新' : state.checkingConflicts ? '正在检测冲突' : activeJobs.size ? `${activeJobs.size} 个后台任务进行中` : state.content?.recoveryRequired ? '配置需要恢复' : state.mods ? '配置已加载' : '等待选择配置'),
      items.length > 0 ? h('span', { className: 'library-enabled-meter', title: `已启用 ${items.filter(mod => mod.enabled).length} / ${items.length}` },
        h('span', { className: 'library-enabled-meter-fill', style: { width: `${Math.round(items.filter(mod => mod.enabled).length / items.length * 100)}%` } })) : null,
      h('span', { className: 'count-label' }, `${items.filter(mod => mod.enabled).length} 已启用 · ${state.mods?.total ?? 0} 个模组`,
        state.query || facets.status !== 'all' || hasFacetFilter ? ` · 显示 ${visibleItems.length}` : '')) : null,
    h(Antd.Modal, { title: '游戏设置与安装目录', open: settingsOpen, footer: null, onCancel: () => setSettingsOpen(false), width: 640 },
      h(Antd.Space, { orientation: 'vertical', className: 'library-modal-fields' },
        h(Antd.Typography.Text, { strong: true }, '游戏版本变体'),
        h(Antd.Select<string>, { value: state.variantId || undefined, options: currentGame?.variants.map(variant => ({ value: variant.variantId, label: variant.displayName })),
          onChange: actions.selectVariant, 'aria-label': '游戏版本' }),
        h(Antd.Typography.Text, { strong: true }, '安装目录定位与注册'),
        h(Antd.Space.Compact, { style: { width: '100%' } },
          h(Antd.Input, { value: rootPath, placeholder: '真实游戏安装目录（绝对路径）', 'aria-label': '安装目录', onChange: (event: React.ChangeEvent<HTMLInputElement>) => setRootPath(event.target.value) }),
          h(Antd.Button, { icon: icon('FolderOpenOutlined'), onClick: () => actions.run(async () => {
            const picked = await actions.browseGameDirectory();
            if (picked) setRootPath(picked);
          }) }, '浏览目录')),
        h(Antd.Space, null, h(Antd.Button, { disabled: !state.variantId, onClick: () => actions.run(() => actions.discover(rootPath)) }, '发现安装目录'),
          h(Antd.Button, { type: 'primary', className: 'glass-primary-action', disabled: !rootPath.trim() || !state.variantId, onClick: () => actions.run(() => actions.register(rootPath)) }, '验证并注册')),
        state.candidates.length ? h(Antd.Select<string>, { placeholder: '选择发现的安装目录', 'aria-label': '发现的安装目录',
          options: state.candidates.map(candidate => ({ value: `${candidate.variantId}\u0000${candidate.rootPath}`, label: `${candidate.rootPath} · ${candidate.variantId}` })),
          onChange: (value: string) => { const [variantId, path] = value.split('\u0000'); setRootPath(path); actions.selectVariant(variantId); } }) : null,
        h(Antd.Typography.Text, { strong: true }, '已注册安装目录'),
        h(Antd.Select<string>, { value: state.installationId || undefined, 'aria-label': '已注册安装目录', options: state.installations.map(item => ({ value: item.installationId, label: item.rootPath ?? item.installationId })), onChange: actions.selectInstallation }),
        h(Antd.Typography.Text, { type: 'secondary' }, '安装页面会重新验证并明确选择配置、安装目录与游戏版本。'))),
    h(Antd.Modal, { title: '管理配置', open: profilesOpen, footer: null, onCancel: () => setProfilesOpen(false) },
      h(Antd.List<ProfileSummary>, { dataSource: state.profiles, size: 'small', className: 'library-profile-list', renderItem: (item: ProfileSummary) => h(Antd.List.Item, {
        actions: [item.profileId === state.profileId
          ? h(Antd.Tag, { key: 'active', color: 'processing' }, '当前配置')
          : h(Antd.Button, { key: 'select', type: 'link', size: 'small', onClick: () => actions.run(() => actions.selectProfile(item.profileId)) }, '切换')],
      }, h(Antd.List.Item.Meta, { title: item.displayName, description: item.description || undefined })) }),
      h(Antd.Space, { wrap: true, style: { marginTop: 12 } }, h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: icon('PlusOutlined'), disabled: !state.installationId,
        onClick: () => ask('新建配置', '', actions.createProfile) }, '新建配置'),
      h(Antd.Button, { icon: icon('EditOutlined'), disabled: !profile,
        onClick: () => ask('重命名配置', profile?.displayName ?? '', value => actions.editProfile(value, profile?.description ?? '')) }, '重命名'),
      h(Antd.Button, { disabled: !profile, onClick: () => ask('配置说明', profile?.description ?? '', value => actions.editProfile(profile?.displayName ?? '', value)) }, '编辑说明'),
      h(Antd.Button, { icon: icon('CopyOutlined'), disabled: !profile || !writable,
        onClick: () => ask('复制配置', `${profile?.displayName ?? ''} 副本`, actions.cloneProfile) }, '复制'),
      h(Antd.Button, { icon: icon('DeleteOutlined'), danger: true, disabled: !profile,
        onClick: () => confirm('删除配置', `删除 ${profile?.displayName}？`, actions.deleteProfile) }, '删除'))),
    h(Antd.Modal, {
      title: '导入 / 添加模组',
      open: importModalOpen,
      footer: importTab === 'local' ? null : undefined,
      okText: importTab === 'url' ? '解析并添加' : `导入所选 (${subscribedSelected.length})`,
      cancelText: '取消',
      confirmLoading: importTab === 'url' ? urlSubmitting : false,
      okButtonProps: {
        className: 'glass-primary-action',
        disabled: importTab === 'url'
          ? parseModUrlLines(urlInput).length === 0 || !writable
          : subscribedSelected.length === 0 || !writable,
      },
      onCancel: () => setImportModalOpen(false),
      onOk: () => {
        if (importTab === 'url') {
          const urls = parseModUrlLines(urlInput);
          if (!urls.length) return;
          setUrlSubmitting(true);
          actions.run(async () => {
            try {
              const report = await actions.addModsFromUrls(urls, importTargetGroupId);
              setUrlReport(report);
              if (importTargetGroupId && report.added > 0) setGroupByPersisted('profile');
              if (report.failed.length === 0) setImportModalOpen(false);
            } finally {
              setUrlSubmitting(false);
            }
          });
          return;
        }
        if (importTab === 'subscribed') {
          const chosen = state.subscribedItems.filter(item => subscribedSelected.includes(item.reference.remoteModId));
          if (!chosen.length) return;
          actions.run(async () => {
            await actions.addSubscribedMods(chosen, importTargetGroupId);
            if (importTargetGroupId) setGroupByPersisted('profile');
            setImportModalOpen(false);
          });
        }
      },
      width: 580,
    },
      h('div', { className: 'library-import-modal' },
        h('div', { className: 'library-import-tabs', role: 'tablist', 'aria-label': '导入方式' },
          h('button', {
            type: 'button', role: 'tab', 'aria-selected': importTab === 'local',
            className: `library-import-tab${importTab === 'local' ? ' is-active' : ''}`,
            onClick: () => setImportTab('local'),
          }, icon('FileAddOutlined'), '本地文件'),
          h('button', {
            type: 'button', role: 'tab', 'aria-selected': importTab === 'url',
            className: `library-import-tab${importTab === 'url' ? ' is-active' : ''}`,
            onClick: () => setImportTab('url'),
          }, icon('LinkOutlined'), '链接批量添加'),
          modioSupported ? h('button', {
            type: 'button', role: 'tab', 'aria-selected': importTab === 'subscribed',
            className: `library-import-tab${importTab === 'subscribed' ? ' is-active' : ''}`,
            onClick: () => {
              setImportTab('subscribed');
              if (!state.subscribedItems.length && !state.loadingSubscribed) actions.run(() => actions.loadSubscribed(false));
            },
          }, icon('StarOutlined'), 'mod.io 订阅') : null),
        h('div', { className: 'library-import-target-row' },
          h('span', { className: 'library-import-target-label' }, icon('FolderOutlined'), '导入到目录'),
          h(Antd.TreeSelect<string>, {
            className: 'library-group-treeselect',
            'aria-label': '导入目标目录',
            value: importTargetGroupId ?? ROOT_GROUP_VALUE,
            treeData: groupTreeData,
            treeDefaultExpandAll: true,
            showSearch: true,
            treeNodeFilterProp: 'title',
            placeholder: '选择目标分组目录',
            onChange: (value: string) => setImportTargetGroupId(!value || value === ROOT_GROUP_VALUE ? undefined : value),
          })),
        importTab === 'local' ? h('div', { className: 'library-import-tab-body' },
          h('div', {
            className: `library-dropzone library-dropzone--modal${!writable || !importSupported ? ' library-dropzone--disabled' : ''}`,
            role: 'button',
            tabIndex: writable && importSupported ? 0 : -1,
            onClick: () => {
              if (!writable || !importSupported) return;
              const targetGroup = importTargetGroupId;
              setImportModalOpen(false);
              actions.run(async () => {
                await actions.importLocal('file', undefined, targetGroup);
                if (targetGroup) setGroupByPersisted('profile');
              });
            },
            onKeyDown: (event: React.KeyboardEvent) => {
              if ((event.key === 'Enter' || event.key === ' ') && writable && importSupported) {
                event.preventDefault();
                const targetGroup = importTargetGroupId;
                setImportModalOpen(false);
                actions.run(async () => {
                  await actions.importLocal('file', undefined, targetGroup);
                  if (targetGroup) setGroupByPersisted('profile');
                });
              }
            },
          },
          h('div', { className: 'library-dropzone-orb', 'aria-hidden': true }, icon('InboxOutlined')),
          h('div', { className: 'library-dropzone-title' }, '点击选择 .zip / .pak 文件，或直接拖拽到主窗口')),
          h('div', { className: 'library-import-local-actions' },
            h(Antd.Button, {
              type: 'primary', className: 'glass-primary-action', icon: icon('FileAddOutlined'),
              disabled: !writable || !importSupported,
              onClick: () => {
                const targetGroup = importTargetGroupId;
                setImportModalOpen(false);
                actions.run(async () => {
                  await actions.importLocal('file', undefined, targetGroup);
                  if (targetGroup) setGroupByPersisted('profile');
                });
              },
            }, '选择文件'),
            h(Antd.Button, {
              icon: icon('FolderOpenOutlined'),
              disabled: !writable || !importSupported,
              onClick: () => {
                const targetGroup = importTargetGroupId;
                setImportModalOpen(false);
                actions.run(async () => {
                  await actions.importLocal('directory', undefined, targetGroup);
                  if (targetGroup) setGroupByPersisted('profile');
                });
              },
            }, '选择目录')),
          !importSupported && state.gameId ? h(Antd.Typography.Text, { type: 'secondary' }, '当前游戏暂不支持本地导入。') : null) : null,
        importTab === 'url' ? h('div', { className: 'library-import-tab-body' },
          h(Antd.Input.TextArea, {
            rows: 5,
            value: urlInput,
            placeholder: '每行粘贴一个 mod.io / ModCat / Nexus Mods 链接',
            'aria-label': '模组链接列表',
            onChange: (event: React.ChangeEvent<HTMLTextAreaElement>) => setUrlInput(event.target.value),
          }),
          urlReport ? h(Antd.Alert, {
            type: urlReport.failed.length ? 'warning' : 'success',
            showIcon: true,
            message: `已添加 ${urlReport.added} 个模组${urlReport.failed.length ? `，${urlReport.failed.length} 个失败` : ''}`,
            description: urlReport.failed.length
              ? h('ul', { style: { margin: 0, paddingLeft: 18 } },
                ...urlReport.failed.map(item => h('li', { key: item.url }, `${item.url}：${item.reason}`)))
              : undefined,
          }) : null) : null,
        importTab === 'subscribed' ? h('div', { className: 'library-import-tab-body' },
          h('div', { className: 'library-subscribed-toolbar' },
            h(Antd.Button, {
              size: 'small',
              disabled: !state.subscribedItems.length,
              onClick: () => {
                const allIds = state.subscribedItems.map(item => item.reference.remoteModId);
                setSubscribedSelected(subscribedSelected.length === allIds.length ? [] : allIds);
              },
            }, subscribedSelected.length === state.subscribedItems.length && state.subscribedItems.length > 0 ? '取消全选' : '全选'),
            h(Antd.Button, { size: 'small', icon: icon('ReloadOutlined'), loading: state.loadingSubscribed, onClick: () => actions.run(() => actions.loadSubscribed(false)) }, '刷新')),
          state.loadingSubscribed && !state.subscribedItems.length
            ? h('div', { className: 'library-loading' }, h(Antd.Spin, { size: 'small' }), ' 正在读取订阅…')
            : h(Antd.List<SourceSearchItem>, {
              size: 'small',
              className: 'library-subscribed-list',
              dataSource: state.subscribedItems,
              locale: { emptyText: '暂无订阅模组（请确认已配置 mod.io 凭据）' },
              renderItem: (item: SourceSearchItem) => {
                const id = item.reference.remoteModId;
                const alreadyInProfile = items.some(mod => mod.mod.sourceId === item.reference.sourceId && mod.mod.remoteModId === id);
                const checked = subscribedSelected.includes(id);
                return h(Antd.List.Item, {
                  actions: alreadyInProfile ? [h(Antd.Tag, { key: 'exists' }, '已在配置中')] : undefined,
                },
                  h(Antd.Checkbox, {
                    checked,
                    disabled: alreadyInProfile,
                    onChange: () => setSubscribedSelected(current => checked ? current.filter(v => v !== id) : [...current, id]),
                  }, h('span', null, item.displayName)));
              },
            }),
          state.subscribedNextCursor ? h(Antd.Button, {
            block: true,
            loading: state.loadingSubscribed,
            onClick: () => actions.run(() => actions.loadSubscribed(true)),
          }, '加载更多') : null) : null)),
    h(Antd.Modal, {
      title: `文件覆盖冲突详情（${state.conflicts.length} 处）`,
      open: conflictsOpen,
      footer: null,
      onCancel: () => setConflictsOpen(false),
      width: 680,
    },
      h(Antd.Space, { orientation: 'vertical', className: 'library-modal-fields' },
        state.conflictSkippedIds.length > 0
          ? h(Antd.Alert, { type: 'info', showIcon: true, message: `已跳过 ${state.conflictSkippedIds.length} 个尚未下载本地缓存的模组。` }) : null,
        h(Antd.List<ModConflictItem>, {
          size: 'small',
          className: 'library-conflict-list',
          dataSource: state.conflicts,
          locale: { emptyText: '未检测到文件冲突' },
          renderItem: (conflict: ModConflictItem) => {
            const nameOf = (id: string) => items.find(mod => mod.profileModId === id)?.displayName ?? id;
            return h(Antd.List.Item, null,
              h(Antd.List.Item.Meta, {
                title: conflict.relativePath,
                description: `生效模组：${nameOf(conflict.winnerProfileModId)} · 被覆盖：${conflict.shadowedProfileModIds.map(nameOf).join('、')}`,
              }));
          },
        }))),
    h(Antd.Modal, { title: dialog?.title, open: !!dialog, okText: '确定', cancelText: '取消', onCancel: () => setDialog(undefined),
      okButtonProps: { disabled: !dialogValue.trim() }, onOk: () => {
        const current = dialog; if (!current) return;
        actions.run(async () => { await current.submit(dialogValue.trim()); setDialog(undefined); });
      } }, h(Antd.Input, { value: dialogValue, autoFocus: true, 'aria-label': dialog?.title, onChange: (event: React.ChangeEvent<HTMLInputElement>) => setDialogValue(event.target.value) })),
    contextMenu ? createPortal(
      h('div', {
        className: 'library-context-menu',
        style: { left: contextMenu.x, top: contextMenu.y },
        role: 'menu',
        onClick: (event: React.MouseEvent) => event.stopPropagation(),
        onContextMenu: (event: React.MouseEvent) => event.preventDefault(),
      }, h(Antd.Menu, {
        mode: 'vertical',
        selectable: false,
        items: contextMenu.items,
        onClick: ({ key }: { key: string }) => contextMenu.onClick(key),
      })),
      document.body,
      'library-context-menu',
    ) : null,
    dragGhost ? createPortal(
      h('div', {
        className: 'library-drag-ghost',
        style: { left: dragGhost.x + 14, top: dragGhost.y + 12 },
      },
      h('span', { className: 'library-drag-ghost-icon', 'aria-hidden': true }, icon('HolderOutlined')),
      h('span', { className: 'library-drag-ghost-label' }, dragGhost.label),
      dropTarget?.side === 'inside' ? h('span', { className: 'library-drag-ghost-hint' }, '移入分组') : null),
      document.body,
      'library-drag-ghost',
    ) : null);
}
