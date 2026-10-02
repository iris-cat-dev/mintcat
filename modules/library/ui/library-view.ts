import type * as React from 'react';
import type { ReactElement, ReactNode } from 'react';
import type { GroupSummary, ModSummary, ProfileSummary } from '@mintcat/sdk-ts';
import { getSharedRuntime, navigateUi } from '@mintcat/sdk-ui';
import { canEditProfileContent, groupDropTarget, indexDownloadOverlays, modDropTarget, modMetadataLabels, pendingVersionLabel } from './library-model';
import type { LibraryActions, LibraryState, LibraryTask } from './library';

export type LibraryPage = 'mods' | 'local' | 'downloads';
interface LibraryViewProps {
  subscribe(listener: () => void): () => void;
  getSnapshot(): LibraryState;
  actions: LibraryActions;
  page: LibraryPage;
  toolbar?: HTMLElement;
}
type Dialog = { title: string; value: string; submit(value: string): Promise<void> };

export function LibraryView({ subscribe, getSnapshot, actions, page, toolbar }: LibraryViewProps): ReactElement {
  const { React, Antd, Icons, createPortal } = getSharedRuntime();
  const h = React.createElement;
  const state = React.useSyncExternalStore(subscribe, getSnapshot);
  const [multiSelect, setMultiSelect] = React.useState(false);
  const [selected, setSelected] = React.useState<string[]>([]);
  const [expanded, setExpanded] = React.useState<string[]>([]);
  const [settingsOpen, setSettingsOpen] = React.useState(false);
  const [profilesOpen, setProfilesOpen] = React.useState(false);
  const [rootPath, setRootPath] = React.useState('');
  const [dialog, setDialog] = React.useState<Dialog | undefined>();
  const [dialogValue, setDialogValue] = React.useState('');
  const [selectionPaths, setSelectionPaths] = React.useState<Record<string, string>>({});
  const [filter, setFilter] = React.useState<'all' | 'enabled' | 'pending'>('all');
  const [groupBy, setGroupBy] = React.useState<'source' | 'profile'>('source');
  const [collapsedSources, setCollapsedSources] = React.useState<string[]>([]);
  const dragging = React.useRef<string | undefined>(undefined);
  const [dropTarget, setDropTarget] = React.useState<{ key: string; side: 'before' | 'after' | 'inside' }>();
  const searchInput = React.useRef<HTMLInputElement>(null);
  const { modal } = Antd.App.useApp();
  const writable = canEditProfileContent(state.content) && !state.pending && !!state.mods;
  const items = state.mods?.items ?? [];
  const profile = state.profiles.find(item => item.profileId === state.profileId);
  const overlays = state.content ? indexDownloadOverlays(state.content) : undefined;
  const terminalStates = ['succeeded', 'failed', 'cancelled'];
  const pendingIds = new Set(items.filter(mod => {
    const overlay = overlays?.get(mod.profileModId);
    return overlay && !terminalStates.includes(overlay.item.transferState ?? overlay.operation.state);
  }).map(mod => mod.profileModId));
  const visibleItems = items.filter(mod => filter === 'all' || (filter === 'enabled' ? mod.enabled : pendingIds.has(mod.profileModId)));
  const toggleSelected = (id: string): void => setSelected(current => current.includes(id) ? current.filter(item => item !== id) : [...current, id]);
  React.useEffect(() => {
    setSelected([]); setMultiSelect(false); setDialog(undefined); setSettingsOpen(false); setProfilesOpen(false);
    setSelectionPaths({});
    dragging.current = undefined; setDropTarget(undefined);
  }, [state.profileId, state.gameId, state.generation]);
  React.useEffect(() => { setExpanded(state.groups.filter(group => group.expanded).map(group => group.groupId)); }, [state.groups]);
  React.useEffect(() => { setRootPath(''); setCollapsedSources([]); }, [state.gameId]);
  React.useEffect(() => {
    const focusSearch = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault(); searchInput.current?.focus();
      }
    };
    window.addEventListener('keydown', focusSearch);
    return () => window.removeEventListener('keydown', focusSearch);
  }, []);
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
  const groupChoices = [{ key: 'move:', label: '未分组', icon: icon('FolderOutlined') },
    ...state.groups.map(group => ({ key: `move:${group.groupId}`, label: group.name, icon: icon('FolderOutlined') }))];
  const moveWithin = (mod: ModSummary, offset: number): void => {
    const siblings = items.filter(item => item.groupId === mod.groupId).sort((a, b) => a.sortOrder - b.sortOrder);
    const position = siblings.findIndex(item => item.profileModId === mod.profileModId) + offset;
    if (position >= 0 && position < siblings.length) actions.run(() => actions.moveMod(mod, position, mod.groupId));
  };
  const modMenu = (mod: ModSummary) => ({
    items: [
      { key: 'rename', label: '重命名', icon: icon('EditOutlined') },
      { key: 'top', label: '置顶', icon: icon('VerticalAlignTopOutlined'), disabled: !writable || !!state.query.trim() },
      { key: 'up', label: '上移', icon: icon('ArrowUpOutlined'), disabled: !writable || !!state.query.trim() },
      { key: 'down', label: '下移', icon: icon('ArrowDownOutlined'), disabled: !writable || !!state.query.trim() },
      { key: 'move', label: '移动到', icon: icon('DragOutlined'), children: groupChoices, disabled: !writable || !!state.query.trim() },
      { key: 'remove', label: '删除', icon: icon('DeleteOutlined'), danger: true, disabled: !writable },
    ],
    onClick: ({ key }: { key: string }) => {
      if (key === 'rename') ask('重命名 Mod（共享名称）', mod.displayName, value => actions.renameMod(mod, value));
      else if (key === 'remove') confirm('删除 Mod', `从当前配置中删除 ${mod.displayName}？`, () => actions.removeMods([mod.profileModId]));
      else if (key === 'top') actions.run(() => actions.moveMod(mod, 0, mod.groupId));
      else if (key === 'up') moveWithin(mod, -1);
      else if (key === 'down') moveWithin(mod, 1);
      else if (key.startsWith('move:')) actions.run(() => actions.moveMod(mod, 0, key.slice(5) || undefined));
    },
  });
  const startDrag = (event: React.DragEvent, key: string): void => {
    dragging.current = key;
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('application/x-mintcat-library', key);
    setDropTarget(undefined);
  };
  const finishDrag = (): void => { dragging.current = undefined; setDropTarget(undefined); };
  const dropSide = (event: React.DragEvent<HTMLTableRowElement>): 'before' | 'after' => {
    const bounds = event.currentTarget.getBoundingClientRect();
    return event.clientY > bounds.top + bounds.height / 2 ? 'after' : 'before';
  };
  const showDrop = (event: React.DragEvent, key: string, side: 'before' | 'after' | 'inside', allowed: boolean): void => {
    event.dataTransfer.dropEffect = allowed ? 'move' : 'none';
    if (!allowed) { setDropTarget(undefined); return; }
    event.preventDefault();
    setDropTarget(current => current?.key === key && current.side === side ? current : { key, side });
  };
  const leaveDrop = (event: React.DragEvent<HTMLTableRowElement>): void => {
    if (!(event.relatedTarget instanceof Node) || !event.currentTarget.contains(event.relatedTarget)) setDropTarget(undefined);
  };
  const renderMod = (mod: ModSummary, order: number): ReactElement => {
    const overlay = overlays?.get(mod.profileModId);
    const pending = pendingVersionLabel(overlay);
    const details = state.versions[mod.profileModId];
    const version = mod.versionSelection.kind === 'pinned' ? `pinned:${mod.versionSelection.versionId}` : 'auto';
    const metadata = modMetadataLabels(mod.tags, mod.approvalStatus);
    const options = [{ value: 'auto', label: mod.appliedVersionId ?? mod.currentVersionId ?? '—' }, ...(details?.versions ?? []).map(item => ({
      value: `pinned:${item.versionId}`, label: `${item.versionId === 'legacy-unversioned' ? '未标明版本' : item.versionId}${item.versionId === mod.appliedVersionId ? ' · 已应用' : item.current ? ' · 库中当前' : ''}`,
    }))];
    if (version !== 'auto' && !options.some(option => option.value === version)) options.push({ value: version, label: mod.versionSelection.versionId === 'legacy-unversioned' ? '未标明版本' : mod.versionSelection.versionId ?? '—' });
    return h(Antd.Dropdown, { key: mod.profileModId, trigger: ['contextMenu'], menu: modMenu(mod) },
      h('tr', {
        className: `library-mod-row${order % 2 === 0 ? ' library-row-alt' : ''}${selected.includes(mod.profileModId) ? ' library-row-selected' : ''}${pendingIds.has(mod.profileModId) ? ' library-row-busy' : ''}`,
        tabIndex: 0, 'data-mod-id': mod.profileModId, 'aria-selected': selected.includes(mod.profileModId),
        'data-drop-side': dropTarget?.key === `mod:${mod.profileModId}` ? dropTarget.side : undefined,
        onClick: (event: React.MouseEvent) => {
          if (multiSelect || event.ctrlKey || event.metaKey) toggleSelected(mod.profileModId);
          else setSelected([mod.profileModId]);
        },
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
        draggable: writable && !state.query.trim(),
        onDragStart: (event: React.DragEvent) => startDrag(event, `mod:${mod.profileModId}`),
        onDragEnd: finishDrag,
        onDragLeave: leaveDrop,
        onDragOver: (event: React.DragEvent<HTMLTableRowElement>) => {
          const dragged = items.find(item => `mod:${item.profileModId}` === dragging.current);
          const side = dropSide(event);
          showDrop(event, `mod:${mod.profileModId}`, side, writable && !state.query.trim() && !!dragged && !!modDropTarget(items, dragged, mod, side, groupBy));
        },
        onDrop: (event: React.DragEvent<HTMLTableRowElement>) => {
          event.preventDefault();
          const source = event.dataTransfer.getData('application/x-mintcat-library');
          const dragged = source === dragging.current ? items.find(item => `mod:${item.profileModId}` === source) : undefined;
          const target = writable && !state.query.trim() && dragged ? modDropTarget(items, dragged, mod, dropSide(event), groupBy) : undefined;
          finishDrag();
          if (dragged && target) actions.run(() => actions.moveMod(dragged, target.position, target.groupId));
        },
      },
      h('td', { className: 'library-order-cell' }, multiSelect
        ? h(Antd.Checkbox, { checked: selected.includes(mod.profileModId),
          onClick: (event: React.MouseEvent) => event.stopPropagation(), onChange: () => toggleSelected(mod.profileModId) },
          h('span', { className: 'library-sr-only' }, `选择 ${mod.displayName}`))
        : h('span', { className: 'mod-order' }, order)),
      h('td', { className: 'library-name-cell' },
        h('div', { className: 'library-mod-name' },
          h(Antd.Tooltip, { title: pending ?? `${mod.displayName} · ${mod.mod.sourceId}` }, h('span', { className: 'mod-name' }, mod.displayName)),
          pending ? h(Antd.Tooltip, { title: pending }, h('span', { className: 'library-pending-dot', 'aria-label': pending })) : null,
          h('span', { className: 'mod-row-menu', onClick: (event: React.MouseEvent) => event.stopPropagation() },
            h(Antd.Dropdown, { menu: modMenu(mod), trigger: ['click'] }, h(Antd.Button, { size: 'small', type: 'text', icon: icon('MoreOutlined'), 'aria-label': `${mod.displayName} 操作` }))))),
      h('td', { onClick: (event: React.MouseEvent) => event.stopPropagation() },
        h(Antd.Select<string>, { size: 'small', variant: 'borderless', className: 'library-version-select', value: version, options,
          popupMatchSelectWidth: false, disabled: !writable, loading: pendingIds.has(mod.profileModId),
          suffixIcon: mod.versionSelection.kind === 'pinned' ? icon('LockOutlined') : undefined,
          'aria-label': `${mod.displayName} 版本`, onOpenChange: (open: boolean) => { if (open) actions.run(() => actions.loadVersions(mod)); },
          title: mod.appliedVersionId ? `已应用 ${mod.appliedVersionId}` : mod.currentVersionId ? `库中版本 ${mod.currentVersionId}；尚未验证或应用` : '没有已知版本',
          popupRender: (menu: ReactNode) => h('div', null, h('div', { className: 'library-version-hint' }, '自动最新 / 固定版本'), menu),
          onChange: (value: string) => actions.run(() => actions.setVersion(mod, value)) })),
      h('td', null, mod.mod.sourceId === 'local' ? h('span', { className: 'library-category library-category-local' }, '本地')
        : metadata.category === '—' ? '—' : h('span', { className: 'library-category', 'data-category': metadata.category, title: mod.tags.join(' · ') }, metadata.category)),
      h('td', { className: 'library-metadata', title: metadata.multiplayer }, metadata.multiplayer === '—' ? '—'
        : h('span', { className: 'library-metadata-badge', 'data-requirement': metadata.multiplayer }, metadata.multiplayer)),
      h('td', { className: 'library-metadata' }, '—'),
      h('td', { className: 'library-metadata', title: mod.approvalStatus || undefined }, metadata.approval === '—' ? '—'
        : h('span', { className: 'library-metadata-badge', 'data-approval': mod.approvalStatus }, metadata.approval)),
      h('td', { onClick: (event: React.MouseEvent) => event.stopPropagation() },
        h(Antd.Switch, { size: 'small', checked: mod.enabled, disabled: !writable, ...{ 'aria-label': `启用 ${mod.displayName}` },
          onChange: (enabled: boolean) => actions.run(() => actions.setEnabled(mod, enabled)) }))));
  };
  const renderGroup = (group: GroupSummary, depth: number): ReactElement => {
    const descendants = new Set<string>([group.groupId]);
    for (let changed = true; changed;) {
      changed = false;
      for (const item of state.groups) if (item.parentGroupId && descendants.has(item.parentGroupId) && !descendants.has(item.groupId)) { descendants.add(item.groupId); changed = true; }
    }
    const destinations = [{ key: 'parent:', label: '根分组' }, ...state.groups.filter(item => !descendants.has(item.groupId))
      .map(item => ({ key: `parent:${item.groupId}`, label: item.name }))];
    const menu = {
      items: [
        { key: 'add', label: '添加子分组', icon: icon('FolderAddOutlined'), disabled: !writable },
        { key: 'rename', label: '重命名分组', icon: icon('EditOutlined'), disabled: !writable },
        { key: 'move', label: '移动到', icon: icon('DragOutlined'), children: destinations, disabled: !writable },
        { key: 'up', label: '上移', icon: icon('ArrowUpOutlined'), disabled: !writable },
        { key: 'down', label: '下移', icon: icon('ArrowDownOutlined'), disabled: !writable },
        { key: 'remove', label: '删除分组', icon: icon('DeleteOutlined'), danger: true, disabled: !writable },
      ],
      onClick: ({ key }: { key: string }) => {
        if (key === 'add') ask('添加子分组', '', value => actions.createGroup(value, group.groupId));
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
    return h(Antd.Dropdown, { key: `group:${group.groupId}`, trigger: ['contextMenu'], menu },
      h('tr', {
        className: 'library-group-row', draggable: writable && !state.query.trim(),
        'data-group-id': group.groupId,
        'data-drop-side': dropTarget?.key === `group:${group.groupId}` ? dropTarget.side : undefined,
        title: '拖动分组调整顺序；Shift 拖动分组移入目标分组',
        onDragStart: (event: React.DragEvent) => startDrag(event, `group:${group.groupId}`),
        onDragEnd: finishDrag,
        onDragLeave: leaveDrop,
        onDragOver: (event: React.DragEvent<HTMLTableRowElement>) => {
          const mod = items.find(item => `mod:${item.profileModId}` === dragging.current);
          const dragged = state.groups.find(item => `group:${item.groupId}` === dragging.current);
          const side = dropSide(event);
          const target = dragged ? groupDropTarget(state.groups, dragged, group, side, event.shiftKey) : undefined;
          showDrop(event, `group:${group.groupId}`, mod || event.shiftKey ? 'inside' : side,
            writable && !state.query.trim() && (!!mod || !!target));
        },
        onDrop: (event: React.DragEvent<HTMLTableRowElement>) => {
          event.preventDefault();
          const source = event.dataTransfer.getData('application/x-mintcat-library');
          if (!writable || state.query.trim() || source !== dragging.current) { finishDrag(); return; }
          const mod = items.find(item => `mod:${item.profileModId}` === source);
          const dragged = state.groups.find(item => `group:${item.groupId}` === source);
          const target = dragged ? groupDropTarget(state.groups, dragged, group, dropSide(event), event.shiftKey) : undefined;
          finishDrag();
          if (mod) actions.run(() => actions.moveMod(mod, 0, group.groupId));
          else if (dragged && target) actions.run(() => actions.moveGroup(dragged, target.parentGroupId, target.position));
        },
      }, h('td', { colSpan: 8 },
        h('div', { className: 'library-group-heading', style: { paddingLeft: depth * 18 } },
          h('button', { type: 'button', className: 'library-group-toggle', 'aria-expanded': open, onClick: () => {
            setExpanded(current => open ? current.filter(id => id !== group.groupId) : [...current, group.groupId]);
            if (writable) actions.run(() => actions.expandGroup(group, !open));
          } }, icon(open ? 'DownOutlined' : 'RightOutlined'), icon('FolderOutlined'), group.name),
          h('span', { className: 'mod-group-count' }, visibleItems.filter(mod => mod.groupId && descendants.has(mod.groupId)).length),
          h('span', { className: 'mod-row-menu' }, h(Antd.Dropdown, { menu, trigger: ['click'] },
            h(Antd.Button, { type: 'text', size: 'small', icon: icon('MoreOutlined'), 'aria-label': `${group.name} 分组操作` })))))));
  };
  const sortedItems = [...visibleItems].sort((a, b) => a.sortOrder - b.sortOrder);
  let displayOrder = 0;
  const visited = new Set<string>();
  const rowsFor = (parentGroupId?: string, depth = 0): ReactElement[] => {
    const rows: ReactElement[] = [];
    for (const group of state.groups.filter(group => group.parentGroupId === parentGroupId).sort((a, b) => a.sortOrder - b.sortOrder)) {
      if (visited.has(group.groupId)) continue;
      visited.add(group.groupId);
      rows.push(renderGroup(group, depth));
      if (expanded.includes(group.groupId)) rows.push(...rowsFor(group.groupId, depth + 1));
    }
    rows.push(...sortedItems.filter(mod => mod.groupId === parentGroupId).map(mod => renderMod(mod, ++displayOrder)));
    return rows;
  };
  const sourceOrder = ['modio', 'nexusmods', 'modcat', 'local'];
  const sources = [...new Set(sortedItems.map(mod => mod.mod.sourceId))].sort((a, b) => {
    const rankA = sourceOrder.indexOf(a), rankB = sourceOrder.indexOf(b);
    return (rankA < 0 ? sourceOrder.length : rankA) - (rankB < 0 ? sourceOrder.length : rankB) || a.localeCompare(b);
  });
  const tableRows = groupBy === 'profile' ? rowsFor() : sources.flatMap(source => {
    const mods = sortedItems.filter(mod => mod.mod.sourceId === source);
    const count = items.filter(mod => mod.mod.sourceId === source).length;
    const collapsed = collapsedSources.includes(source);
    return [h('tr', { key: `source:${source}`, className: 'library-group-row' }, h('td', { colSpan: 8 },
      h('div', { className: 'library-group-heading' },
        h('button', { type: 'button', className: 'library-group-toggle', 'aria-expanded': !collapsed,
          onClick: () => setCollapsedSources(current => collapsed ? current.filter(id => id !== source) : [...current, source]) },
        icon(collapsed ? 'RightOutlined' : 'DownOutlined'), h('span', { className: `library-source-dot library-source-${sourceOrder.includes(source) ? source : 'other'}` }), source),
        h('span', { className: 'mod-group-count' }, mods.length === count ? count : `${mods.length} / ${count}`)))),
      ...(collapsed ? [] : mods.map(mod => renderMod(mod, ++displayOrder)))];
  });
  const selectedIds = selected.filter(id => items.some(mod => mod.profileModId === id));
  const allSelected = visibleItems.length > 0 && visibleItems.every(mod => selectedIds.includes(mod.profileModId));
  const batchButton = (label: string, name: keyof typeof Icons, action: () => void): ReactElement => h(Antd.Button,
    { type: 'text', size: 'small', icon: icon(name), disabled: !writable || !selectedIds.length, onClick: action }, label);
  const renderTask = (task: LibraryTask): ReactElement => h('div', { className: 'library-task', key: task.job.jobId },
    h(Antd.Space, { wrap: true }, h(Antd.Tag, { color: task.job.state === 'failed' ? 'error' : 'processing' }, task.job.state),
      h('span', null, `${task.job.command} · ${task.job.jobId}`), h('span', { className: 'library-task-profile' }, `配置 ${task.profileId}`),
      !terminalStates.includes(task.job.state)
        ? h(Antd.Button, { size: 'small', onClick: () => actions.run(() => actions.cancelTask(task)) }, '取消任务') : null),
    task.job.error ? h('pre', null, JSON.stringify(task.job.error, null, 2)) : null,
    task.job.command === 'mods.importLocal' && task.action?.state === 'pending' && ['select-file', 'select-directory'].includes(task.action.kind)
      ? h('div', { className: 'library-input-selection' },
        h(Antd.Typography.Paragraph, null, `请选择${task.action.kind === 'select-file' ? '文件' : '目录'}。此选择仅授权当前后台任务。`),
        h('pre', null, JSON.stringify(task.action.target, null, 2)),
        h(Antd.Input, { placeholder: '本地绝对路径', 'aria-label': '导入选择路径', value: selectionPaths[task.job.jobId] ?? '',
          onChange: (event: React.ChangeEvent<HTMLInputElement>) => setSelectionPaths({ ...selectionPaths, [task.job.jobId]: event.target.value }) }),
        h(Antd.Space, null, h(Antd.Button, { size: 'small', type: 'primary', disabled: !selectionPaths[task.job.jobId]?.trim(),
          onClick: () => actions.run(() => actions.approveSelection(task, selectionPaths[task.job.jobId])) }, '确认选择'),
        h(Antd.Button, { size: 'small', onClick: () => actions.run(() => actions.rejectSelection(task)) }, '取消选择'))) : null);
  const operations = state.content?.operations ?? [];
  const taskContent = h('div', { className: 'library-task-list' }, ...state.tasks.map(renderTask), ...operations.map(operation =>
    h('div', { key: `operation:${operation.operationId}`, className: 'library-task' },
      h('div', { className: 'library-operation-heading' }, h('strong', null, `${operation.state} · ${operation.phase}`),
        h('span', { className: 'library-task-profile' }, operation.operationId),
        operation.jobId && !terminalStates.includes(operation.state)
          ? h(Antd.Button, { size: 'small', onClick: () => actions.run(() => actions.cancelDownload(operation)) }, '取消下载') : null),
      h(Antd.Progress, { size: 'small', percent: operation.totalUnits ? Math.min(100, Math.floor(operation.completedUnits / operation.totalUnits * 100)) : 0 }),
      h('span', { className: 'library-task-profile' }, `${operation.completedUnits} / ${operation.totalUnits}`),
      ...operation.items.map(item => h('div', { key: item.profileModId, className: 'library-operation-item' },
        h('span', null, items.find(mod => mod.profileModId === item.profileModId)?.displayName ?? item.profileModId),
        h('span', null, `${item.transferState ?? operation.state} · ${item.resolvedVersion ?? '—'}`))))));
  const hasTasks = state.tasks.length > 0 || operations.length > 0;
  const activeJobs = new Set([
    ...state.tasks.filter(task => !terminalStates.includes(task.job.state)).map(task => task.job.jobId),
    ...operations.filter(operation => !terminalStates.includes(operation.state)).map(operation => operation.jobId ?? operation.operationId),
  ]);
  const importSupported = !!state.games.find(game => game.gameId === state.gameId)?.localRemoteGameKey;
  const toolbarNode = h('div', { className: 'library-toolbar' },
    h('div', { className: 'library-game-control' }, h('span', { className: 'library-game-cover', 'aria-hidden': true }, icon('PictureOutlined')),
      h(Antd.Select<string>, { variant: 'borderless', className: 'library-game-select', 'aria-label': '游戏', value: state.gameId || undefined,
        options: state.games.map(game => ({ value: game.gameId, label: game.displayName })), onChange: (id: string) => actions.run(() => actions.selectGame(id)), placeholder: '选择游戏' })),
    h('label', { className: 'library-search' }, icon('SearchOutlined'),
      h('input', { ref: searchInput, type: 'search', 'aria-label': '搜索 Mod', placeholder: '搜索模组…', value: state.query,
        onChange: (event: React.ChangeEvent<HTMLInputElement>) => actions.search(event.target.value) }),
      h('kbd', { 'aria-hidden': true }, '⌘ / Ctrl K')),
    h('div', { className: 'library-profile-control' }, h('span', null, 'Profile'),
      h(Antd.Select<string>, { variant: 'borderless', className: 'library-profile-select', 'aria-label': '配置', value: state.profileId || undefined,
        options: state.profiles.map(item => ({ value: item.profileId, label: item.displayName })), onChange: (id: string) => actions.run(() => actions.selectProfile(id)), placeholder: '选择配置' })),
    h('div', { className: 'library-toolbar-actions' },
      h(Antd.Dropdown, { menu: {
        items: [{ key: 'file', label: '导入本地文件', disabled: !writable || !importSupported },
          { key: 'directory', label: '导入本地目录', disabled: !writable || !importSupported },
          { key: 'group', label: '添加配置分组', disabled: !writable },
          { key: 'cleanup', label: '清理缺失的本地 Mod', disabled: !writable }],
        onClick: ({ key }: { key: string }) => {
          if (key === 'file' || key === 'directory') actions.run(() => actions.importLocal(key));
          else if (key === 'group') { setGroupBy('profile'); ask('添加分组', '', value => actions.createGroup(value)); }
          else if (key === 'cleanup') confirm('清理缺失的本地 Mod', '服务端将检查并清理当前配置中缺失的本地 Mod。', actions.cleanupMissing);
        },
      }, trigger: ['click'] }, button('添加 / 导入', 'PlusOutlined', undefined)),
      button('刷新列表', 'SyncOutlined', () => actions.run(actions.refresh)),
      h(Antd.Dropdown, { trigger: ['click'], disabled: !writable || !!state.query.trim(), menu: {
        items: [{ key: 'asc', label: '名称 A → Z', icon: icon('SortAscendingOutlined') }, { key: 'desc', label: '名称 Z → A', icon: icon('SortDescendingOutlined') }],
        onClick: ({ key }: { key: string }) => actions.run(() => actions.sortMods(key as 'asc' | 'desc')),
      } }, button('排序', 'SortAscendingOutlined', undefined, !writable || !!state.query.trim())),
      h(Antd.Button, { type: 'primary', className: 'library-install-button', icon: icon('SaveOutlined'), disabled: !profile,
        onClick: () => navigateUi('install') }, '安装模组')));
  return h('div', { className: `mod-list-page-card library-page-${page}` },
    toolbar ? createPortal(toolbarNode, toolbar, `library-toolbar:${state.generation}`) : toolbarNode,
    state.message ? h(Antd.Alert, { type: 'info', message: state.message, showIcon: true, closable: true, className: 'library-status' }) : null,
    state.content && !canEditProfileContent(state.content) ? h(Antd.Alert, { type: 'warning', showIcon: true,
      message: state.content.recoveryRequired ? '配置需要恢复，内容修改已锁定' : `后台任务 ${state.content.busyOperationId ?? ''} 正在修改内容`,
      description: '搜索、切换配置和共享重命名仍然可用。' }) : null,
    page === 'mods' ? h(React.Fragment, null,
      h('div', { className: 'library-view-bar' },
        h('div', { className: 'library-filter-pills', role: 'group', 'aria-label': '模组筛选' },
          ...([{ value: 'all', label: '全部', count: items.length }, { value: 'enabled', label: '已启用', count: items.filter(mod => mod.enabled).length },
            { value: 'pending', label: '待处理', count: pendingIds.size }] as const).map(item =>
            h('button', { key: item.value, type: 'button', className: `library-filter-pill${filter === item.value ? ' library-filter-active' : ''}`,
              'aria-pressed': filter === item.value, onClick: () => setFilter(item.value) }, item.label, h('span', null, item.count)))),
        h('div', { className: 'library-view-controls' },
          h(Antd.Tooltip, { title: groupBy === 'source' ? '来源视图只排序同一来源、同一配置分组的模组；跨组移动请切换配置分组' : '拖动调整顺序；Shift 拖动分组可嵌套' },
            h(Antd.Select<'source' | 'profile'>, { size: 'small', variant: 'borderless', 'aria-label': '分组方式', value: groupBy,
              options: [{ value: 'source', label: '按来源分组' }, { value: 'profile', label: '配置分组' }], onChange: value => { finishDrag(); setGroupBy(value); } })),
          groupBy === 'profile' ? button('添加分组', 'FolderAddOutlined', () => ask('添加分组', '', value => actions.createGroup(value)), !writable) : null,
          button('管理配置', 'EditOutlined', () => setProfilesOpen(true)),
          button('游戏设置与安装目录', 'SettingOutlined', () => setSettingsOpen(true)),
          button('下载管理', 'DownloadOutlined', () => navigateUi('downloads')))),
      hasTasks ? h(Antd.Collapse, { size: 'small', className: 'library-downloads', items: [{ key: 'tasks',
        label: `后台任务 ${state.tasks.length} · 下载操作 ${operations.length}`, children: taskContent }] }) : null,
      h('div', { className: 'library-table-area' },
        state.loading ? h('div', { className: 'library-loading', role: 'status' }, h(Antd.Spin, { size: 'small' }), ' 加载中…') : null,
        h('table', { className: 'library-mod-table', 'aria-label': '模组列表' },
          h('colgroup', null, ...['order', 'name', 'version', 'category', 'multiplayer', 'game-version', 'approval', 'enabled'].map(name => h('col', { key: name, className: `library-col-${name}` }))),
          h('thead', null, h('tr', null, ...['#', '名称', '版本', '类别', '多人游戏', '游戏版本', '认证', '启用'].map(label => h('th', { key: label, scope: 'col' }, label)))),
          h('tbody', null, ...tableRows)),
        !tableRows.length && !state.loading ? h(Antd.Empty, { image: Antd.Empty.PRESENTED_IMAGE_SIMPLE,
          description: state.profileId ? (filter === 'all' ? '暂无匹配的模组' : '没有符合筛选的模组') : '请选择或创建配置' }) : null)) :
      h('div', { className: 'library-management-page' },
        h('div', { className: 'library-management-heading' },
          h('div', null, h('h2', null, page === 'local' ? '本地导入' : '下载管理'),
            h('p', null, page === 'local' ? '选择文件或目录，等待后台请求后明确授权本次导入。关闭页面不会取消已启动任务。' : '查看持久化后台任务、下载进度与待授权选择。')),
          h('div', { className: 'library-view-controls' }, button('管理配置', 'EditOutlined', () => setProfilesOpen(true)),
            button('游戏设置与安装目录', 'SettingOutlined', () => setSettingsOpen(true)))),
        page === 'local' ? h('section', { className: 'library-import-panel' },
          h('h3', null, '导入到当前 Profile'),
          h('p', null, profile?.displayName ?? '请先选择或创建配置'),
          h(Antd.Space, { wrap: true },
            h(Antd.Button, { type: 'primary', icon: icon('FileAddOutlined'), disabled: !writable || !importSupported,
              onClick: () => actions.run(() => actions.importLocal('file')) }, '选择文件'),
            h(Antd.Button, { icon: icon('FolderOpenOutlined'), disabled: !writable || !importSupported,
              onClick: () => actions.run(() => actions.importLocal('directory')) }, '选择目录'),
            h(Antd.Button, { icon: icon('ClearOutlined'), disabled: !writable,
              onClick: () => confirm('清理缺失的本地 Mod', '服务端将检查并清理当前配置中缺失的本地 Mod。', actions.cleanupMissing) }, '清理缺失')),
          !importSupported && state.gameId ? h('p', { className: 'library-task-profile' }, '当前签名游戏未声明本地导入绑定。') : null) : null,
        h('section', { className: 'library-jobs-panel' },
          h('h3', null, `后台任务 ${state.tasks.length} · 下载操作 ${operations.length}`),
          hasTasks ? taskContent : h(Antd.Empty, { image: Antd.Empty.PRESENTED_IMAGE_SIMPLE, description: '暂无已观察到的后台任务' }))),
    h('div', { className: 'home-footer-bar' },
      page === 'mods' ? h(Antd.Checkbox, { checked: multiSelect, onChange: (event: { target: { checked: boolean } }) => { setMultiSelect(event.target.checked); setSelected([]); } }, '批量选择') : null,
      page === 'mods' && multiSelect ? h(Antd.Space, { size: 4 },
        h(Antd.Button, { size: 'small', type: 'text', icon: icon(allSelected ? 'MinusSquareOutlined' : 'CheckSquareOutlined'),
          onClick: () => setSelected(allSelected ? [] : visibleItems.map(mod => mod.profileModId)) }, allSelected ? '取消全选' : '全选'),
        batchButton('删除', 'CloseCircleOutlined', () => confirm('删除 Mod', `删除选中的 ${selectedIds.length} 个 Mod？`, () => actions.removeMods(selectedIds))),
        batchButton('启用', 'PlayCircleOutlined', () => actions.run(() => actions.setBatchEnabled(selectedIds, true))),
        batchButton('禁用', 'PauseCircleOutlined', () => actions.run(() => actions.setBatchEnabled(selectedIds, false)))) : null,
      h('span', { className: 'library-footer-spacer' }),
      h('span', { className: 'library-footer-status', role: 'status' },
        state.loading ? '正在加载' : state.pending ? '正在保存' : activeJobs.size ? `${activeJobs.size} 个后台任务进行中` : state.content?.recoveryRequired ? '配置需要恢复' : state.mods ? '配置已加载' : '等待选择配置'),
      h('span', { className: 'count-label' }, `${items.filter(mod => mod.enabled).length} 已启用 · ${state.mods?.total ?? 0} 个模组`,
        state.query || filter !== 'all' ? ` · 显示 ${visibleItems.length}` : '')),
    h(Antd.Modal, { title: '游戏设置与安装目录', open: settingsOpen, footer: null, onCancel: () => setSettingsOpen(false), width: 640 },
      h(Antd.Space, { orientation: 'vertical', className: 'library-modal-fields' },
        h(Antd.Typography.Text, null, '游戏版本'),
        h(Antd.Select<string>, { value: state.variantId || undefined, options: state.games.find(game => game.gameId === state.gameId)?.variants.map(variant => ({ value: variant.variantId, label: variant.displayName })),
          onChange: actions.selectVariant, 'aria-label': '游戏版本' }),
        h(Antd.Input, { value: rootPath, placeholder: '真实游戏安装目录（绝对路径）', 'aria-label': '安装目录', onChange: (event: React.ChangeEvent<HTMLInputElement>) => setRootPath(event.target.value) }),
        h(Antd.Space, null, h(Antd.Button, { disabled: !state.gameId, onClick: () => actions.run(() => actions.discover(rootPath)) }, '发现安装目录'),
          h(Antd.Button, { type: 'primary', disabled: !rootPath.trim() || !state.variantId, onClick: () => actions.run(() => actions.register(rootPath)) }, '验证并注册')),
        state.candidates.length ? h(Antd.Select<string>, { placeholder: '选择发现的安装目录', 'aria-label': '发现的安装目录',
          options: state.candidates.map(candidate => ({ value: `${candidate.variantId}\u0000${candidate.rootPath}`, label: `${candidate.rootPath} · ${candidate.variantId}` })),
          onChange: (value: string) => { const [variantId, path] = value.split('\u0000'); setRootPath(path); actions.selectVariant(variantId); } }) : null,
        h(Antd.Typography.Text, null, '已注册安装目录'),
        h(Antd.Select<string>, { value: state.installationId || undefined, 'aria-label': '已注册安装目录', options: state.installations.map(item => ({ value: item.installationId, label: item.rootPath ?? item.installationId })), onChange: actions.selectInstallation }),
        h(Antd.Typography.Text, { type: 'secondary' }, '安装页面会重新验证并明确选择配置、安装目录与游戏版本。'))),
    h(Antd.Modal, { title: '管理配置', open: profilesOpen, footer: null, onCancel: () => setProfilesOpen(false) },
      h(Antd.List<ProfileSummary>, { dataSource: state.profiles, size: 'small', renderItem: (item: ProfileSummary) => h(Antd.List.Item, {
        actions: [h(Antd.Button, { key: 'select', type: 'link', size: 'small', onClick: () => actions.run(() => actions.selectProfile(item.profileId)) }, item.profileId === state.profileId ? '当前配置' : '选择')],
      }, h(Antd.List.Item.Meta, { title: item.displayName, description: item.description || undefined })) }),
      h(Antd.Space, { wrap: true }, h(Antd.Button, { icon: icon('PlusOutlined'), disabled: !state.installationId,
        onClick: () => ask('新建配置', '', actions.createProfile) }, '新建配置'),
      h(Antd.Button, { icon: icon('EditOutlined'), disabled: !profile,
        onClick: () => ask('重命名配置', profile?.displayName ?? '', value => actions.editProfile(value, profile?.description ?? '')) }, '重命名'),
      h(Antd.Button, { disabled: !profile, onClick: () => ask('配置说明', profile?.description ?? '', value => actions.editProfile(profile?.displayName ?? '', value)) }, '编辑说明'),
      h(Antd.Button, { icon: icon('CopyOutlined'), disabled: !profile || !writable,
        onClick: () => ask('复制配置', `${profile?.displayName ?? ''} 副本`, actions.cloneProfile) }, '复制'),
      h(Antd.Button, { icon: icon('DeleteOutlined'), danger: true, disabled: !profile,
        onClick: () => confirm('删除配置', `删除 ${profile?.displayName}？`, actions.deleteProfile) }, '删除'))),
    h(Antd.Modal, { title: dialog?.title, open: !!dialog, okText: '确定', cancelText: '取消', onCancel: () => setDialog(undefined),
      okButtonProps: { disabled: !dialogValue.trim() }, onOk: () => {
        const current = dialog; if (!current) return;
        actions.run(async () => { await current.submit(dialogValue.trim()); setDialog(undefined); });
      } }, h(Antd.Input, { value: dialogValue, autoFocus: true, 'aria-label': dialog?.title, onChange: (event: React.ChangeEvent<HTMLInputElement>) => setDialogValue(event.target.value) })));
}
