import type { JobRef, ModRef, ProfileSummary, SourceSearchItem } from '@mintcat/sdk-ts';
import { getSharedRuntime, mountReactView, publishUiSelection } from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, GameUiDescriptor, UiContext } from '@mintcat/sdk-ui';

export async function mountSearch(root: HTMLElement, context: UiContext): Promise<Disposable> {
  const { React, Antd, Icons, createPortal } = getSharedRuntime();
  const h = React.createElement;
  const saved = context.state('search', 1, { gameId: '', profileId: '', sourceId: '', query: '' });
  let alive = true;
  const active = (): boolean => alive && !context.signal.aborted;
  let gameId = saved.gameId, profileId = saved.profileId, sourceId = saved.sourceId, query = saved.query;
  let descriptors: GameUiDescriptor[] = [];
  let profiles: ProfileSummary[] = [], items: SourceSearchItem[] = [];
  let cursor: string | undefined, requestGeneration = 0, bindingGeneration = 0;
  let loading = false, adding = false, error = '', status = '', filter = '', selected: SourceSearchItem | undefined;
  let redraw = (): void => {};
  const update = (): void => { if (active()) redraw(); };
  const persist = (): void => context.saveState('search', 1, { gameId, profileId, sourceId, query });
  const selection = (): void => {
    const descriptor = descriptors.find(item => item.gameId === gameId);
    if (descriptor) publishUiSelection({ gameId, gameName: descriptor.displayName, profileId: profileId || undefined });
  };
  const clearResults = (): void => { ++requestGeneration; loading = false; cursor = undefined; items = []; selected = undefined; error = ''; update(); };
  const run = (action: () => Promise<void>): void => context.guard(async () => {
    try { await action(); } catch (cause) { if (active()) { error = typeof cause === 'object' && cause !== null && 'appError' in cause ? JSON.stringify(cause.appError, null, 2) ?? String(cause) : cause instanceof Error ? cause.message : JSON.stringify(cause, null, 2) ?? String(cause); update(); } throw cause; }
  })();
  const loadBindings = async (): Promise<void> => {
    const generation = ++bindingGeneration; clearResults(); profiles = [];
    const descriptor = descriptors.find(item => item.gameId === gameId);
    const currentGame = gameId, desiredProfile = profileId || saved.profileId, desiredSource = sourceId || saved.sourceId;
    profileId = ''; sourceId = descriptor?.sourceBindings.find(item => item.sourceId === desiredSource)?.sourceId ?? descriptor?.sourceBindings[0]?.sourceId ?? '';
    status = descriptor ? '正在读取配置…' : '没有启用的已签名游戏模块。'; update();
    if (!descriptor) return;
    let profileCursor: string | undefined;
    const loaded: ProfileSummary[] = [];
    do {
      const result = await context.call('profiles.list', { gameId: currentGame, pageSize: 100, ...(profileCursor ? { cursor: profileCursor } : {}) });
      if (!active() || generation !== bindingGeneration || currentGame !== gameId) return;
      loaded.push(...result.items); profileCursor = result.nextCursor;
    } while (profileCursor);
    profiles = loaded; profileId = (profiles.find(item => item.profileId === desiredProfile) ?? profiles.find(item => item.active) ?? profiles[0])?.profileId ?? '';
    status = profileId ? '搜索已签名来源，选择结果添加到配置。' : '请先创建配置，才能添加模组。';
    persist(); selection(); update();
  };
  const search = async (next = false): Promise<void> => {
    if (!gameId || !sourceId) throw new Error('所选游戏没有可用的已签名来源绑定。');
    if (loading || (next && !cursor)) return;
    const generation = ++requestGeneration;
    const binding = descriptors.find(item => item.gameId === gameId)?.sourceBindings.find(item => item.sourceId === sourceId);
    const params = { sourceId, gameId, query, pageSize: 50, ...(binding?.remoteGameKey ? { remoteGameKey: binding.remoteGameKey } : {}), ...(next && cursor ? { cursor } : {}) };
    loading = true; error = ''; if (!next) items = []; update(); persist();
    try {
      const response = await context.query(JSON.stringify(params), () => context.call('sources.search', params));
      if (!active() || generation !== requestGeneration) return;
      items = next ? [...items, ...response.items] : response.items; cursor = response.nextCursor;
      status = cursor ? '还有更多来源结果。' : '已显示全部来源结果。';
    } finally { if (active() && generation === requestGeneration) { loading = false; update(); } }
  };
  const add = async (reference: ModRef): Promise<void> => {
    if (adding) return;
    const target = profiles.find(item => item.profileId === profileId);
    if (!target) throw new Error('请选择目标配置。');
    adding = true; update();
    try {
      const content = await context.call('profiles.contentState', { profileId: target.profileId });
      if (!active()) return;
      if (!content.writable) throw new Error('配置内容正在处理或需要恢复，请稍后重试。');
      const job = await context.request<JobRef>('mods.add', { profileId: target.profileId, reference, expectedContentRevision: content.contentRevision }, crypto.randomUUID());
      if (!active()) return;
      selected = undefined; status = `添加任务 ${job.jobId} 已交给 daemon，关闭页面不会取消任务。`; context.notify(status);
    } finally { if (active()) { adding = false; update(); } }
  };
  function View() {
    const [, setTick] = React.useState(0); redraw = () => setTick(value => value + 1);
    const descriptor = descriptors.find(item => item.gameId === gameId);
    const visible = items.filter(item => !filter || `${item.displayName} ${item.summary ?? ''}`.toLocaleLowerCase().includes(filter.toLocaleLowerCase()));
    const toolbar = h('div', { className: `mintcat-search-header${context.toolbar ? ' mintcat-search-header--portal' : ''}` },
        h(Antd.Button, { type: 'text', icon: h(Icons.HomeOutlined), title: '浏览首页', disabled: loading, onClick: () => { query = ''; clearResults(); run(() => search()); } }),
        h(Antd.Select<string>, { value: gameId || undefined, placeholder: '选择游戏', 'aria-label': '游戏', options: descriptors.map(item => ({ value: item.gameId, label: item.displayName })), style: { width: 160 }, onChange: (value: string) => { gameId = value; run(loadBindings); } }),
        h(Antd.Input.Search, { value: query, placeholder: '搜索模组名称或关键词', allowClear: true, loading, disabled: !sourceId, onChange: (event: { target: { value: string } }) => { query = event.target.value; clearResults(); persist(); }, onSearch: () => run(() => search()), style: { flex: 1, minWidth: 140 } }),
        h(Antd.Select<string>, { value: profileId || undefined, placeholder: '选择添加到的配置', 'aria-label': '目标配置', options: profiles.map(item => ({ value: item.profileId, label: item.displayName })), style: { width: 160 }, onChange: (value: string) => { profileId = value; persist(); selection(); update(); } }),
        h(Antd.Button, { type: 'text', icon: h(Icons.ReloadOutlined, { spin: loading }), title: '刷新搜索', disabled: loading || !sourceId, onClick: () => { context.invalidate(''); run(() => search()); } }));
    return h('div', { className: `mintcat-search-page${context.toolbar ? ' mintcat-search-page--portal' : ''}` },
      context.toolbar ? createPortal(toolbar, context.toolbar) : toolbar,
      h(Antd.Tabs, { activeKey: sourceId, size: 'small', items: (descriptor?.sourceBindings ?? []).map(binding => ({ key: binding.sourceId, label: binding.sourceId })), onChange: (value: string) => { sourceId = value; clearResults(); persist(); run(() => search()); } }),
      h('div', { className: 'mintcat-search-filters' },
        h(Antd.Input, { value: filter, allowClear: true, placeholder: '筛选已加载结果', 'aria-label': '筛选已加载结果', onChange: (event: { target: { value: string } }) => { filter = event.target.value; update(); }, style: { width: 240 } }),
        h(Antd.Typography.Text, { type: 'secondary' }, `${visible.length} / ${items.length} 个结果`)),
      error ? h(Antd.Alert, { type: 'error', showIcon: true, title: error }) : null,
      !sourceId ? h(Antd.Empty, { description: '没有可用来源，请启用有来源绑定的已签名游戏模块。' }) : null,
      loading && !items.length ? h(Antd.Skeleton, { active: true, paragraph: { rows: 3 } }) : null,
      !loading && sourceId && !visible.length ? h(Antd.Empty, { description: '没有找到模组' }) : null,
      h('div', { className: 'mintcat-search-results' }, ...visible.map((item, index) => h('article', { key: `${JSON.stringify(item.reference)}:${index}`, className: 'mintcat-search-card' },
        h(Antd.Avatar, { size: 48, shape: 'square', icon: h(Icons.AppstoreOutlined) }),
        h('div', { className: 'mintcat-search-info' },
          h('div', { className: 'mintcat-search-card-title' }, item.profileUrl && /^https?:\/\//i.test(item.profileUrl) ? h(Antd.Typography.Link, { strong: true, href: item.profileUrl, target: '_blank', rel: 'noopener noreferrer' }, item.displayName) : h(Antd.Typography.Text, { strong: true }, item.displayName), h(Antd.Button, { type: 'text', size: 'small', icon: h(Icons.PlusCircleOutlined), disabled: !profileId || adding, title: '添加到配置', onClick: () => { selected = item; update(); } })),
          h('div', { className: 'mintcat-search-summary' }, item.summary || '此来源未提供模组简介。'),
          h(Antd.Typography.Text, { type: 'secondary', className: 'mintcat-search-reference' }, JSON.stringify(item.reference))),
        item.logoUrl && /^https?:\/\//i.test(item.logoUrl) ? h('img', { className: 'mintcat-search-thumbnail', src: item.logoUrl, alt: item.displayName }) : null))),
      cursor ? h(Antd.Button, { block: true, loading, onClick: () => run(() => search(true)) }, '加载更多结果') : null,
      h(Antd.Typography.Text, { type: 'secondary', role: 'status' }, status),
      h(Antd.Modal, { open: Boolean(selected), title: '添加模组到配置', okText: '添加', cancelText: '取消', confirmLoading: adding, okButtonProps: { disabled: !profileId || adding }, onCancel: () => { if (!adding) { selected = undefined; update(); } }, onOk: () => { if (selected) run(() => add(selected!.reference)); } },
        h(Antd.Typography.Paragraph, null, selected?.displayName),
        error ? h(Antd.Alert, { type: 'error', showIcon: true, title: error, style: { marginBottom: 12 } }) : null,
        h(Antd.Select<string>, { style: { width: '100%' }, value: profileId || undefined, placeholder: '选择目标配置', options: profiles.map(item => ({ value: item.profileId, label: item.displayName })), onChange: (value: string) => { profileId = value; persist(); selection(); update(); } }),
        h(Antd.Alert, { style: { marginTop: 12 }, type: 'info', title: '下载及添加由 daemon 执行；配置写锁与当前内容版本将由 Application API 校验。' })));
  }
  const view = mountReactView(root, h(View));
  const refreshModules = async (): Promise<void> => {
    const generation = ++bindingGeneration;
    const current = await context.request<GameModuleList>('modules.list', {});
    if (!active() || generation !== bindingGeneration) return;
    const previous = JSON.stringify(descriptors.find(item => item.gameId === gameId));
    descriptors = current.modules.flatMap(module => module.enabled && module.game?.descriptor ? [module.game.descriptor] : []);
    gameId = descriptors.some(item => item.gameId === gameId) ? gameId : descriptors[0]?.gameId ?? '';
    if (previous !== JSON.stringify(descriptors.find(item => item.gameId === gameId)) || !profiles.length) { context.invalidate(''); await loadBindings(); } else update();
  };
  const subscription = context.onGenerationChanged(refreshModules);
  run(refreshModules);
  return { dispose() { persist(); alive = false; ++requestGeneration; ++bindingGeneration; subscription.dispose(); view.dispose(); } };
}
