import type { JobRef, ModRef, ProfileSummary, SourceSearchItem } from '@mintcat/sdk-ts';
import { getSharedRuntime, mountReactView, navigateUi, presentUiError, publishUiSelection } from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, GameUiDescriptor, UiContext, UiErrorContext, UiErrorPresentation } from '@mintcat/sdk-ui';

export async function mountSearch(root: HTMLElement, context: UiContext): Promise<Disposable> {
  const { React, Antd, Icons, createPortal } = getSharedRuntime();
  const h = React.createElement;
  const saved = context.state('search', 1, { gameId: '', profileId: '', sourceId: '', query: '' });
  let alive = true;
  const active = (): boolean => alive && !context.signal.aborted;
  let gameId = saved.gameId, profileId = saved.profileId, sourceId = saved.sourceId, query = saved.query;
  let descriptors: GameUiDescriptor[] = [];
  let availableSources = new Set<string>();
  let profiles: ProfileSummary[] = [], items: SourceSearchItem[] = [];
  let cursor: string | undefined, requestGeneration = 0, bindingGeneration = 0;
  let loading = false, bindingsLoading = false, adding = false, searched = false, error: UiErrorPresentation | undefined, errorContext: UiErrorContext = 'search';
  let profileHint = '', status = '', filter = '', selected: SourceSearchItem | undefined;
  let redraw = (): void => {};
  const update = (): void => { if (active()) redraw(); };
  const persist = (): void => context.saveState('search', 1, { gameId, profileId, sourceId, query });
  const selection = (): void => {
    const descriptor = descriptors.find(item => item.gameId === gameId);
    if (descriptor) publishUiSelection({ gameId, gameName: descriptor.displayName, profileId: profileId || undefined });
  };
  const clearResults = (): void => { ++requestGeneration; loading = false; searched = false; cursor = undefined; items = []; selected = undefined; error = undefined; status = ''; update(); };
  const fail = (cause: unknown, operation: UiErrorContext): void => {
    error = presentUiError(cause, operation); errorContext = operation; status = ''; update();
  };
  const run = (action: () => Promise<void>, operation: UiErrorContext = 'search'): void => context.guard(async () => {
    const generation = bindingGeneration, searchGeneration = requestGeneration, currentGame = gameId;
    try { await action(); } catch (cause) {
      if (active() && generation === bindingGeneration && searchGeneration === requestGeneration && currentGame === gameId) fail(cause, operation);
    }
  })();
  const loadBindings = async (): Promise<void> => {
    const generation = ++bindingGeneration; clearResults(); profiles = []; profileHint = '';
    const descriptor = descriptors.find(item => item.gameId === gameId);
    const currentGame = gameId, desiredProfile = profileId || saved.profileId, desiredSource = sourceId || saved.sourceId;
    profileId = ''; sourceId = descriptor?.sourceBindings.find(item => item.sourceId === desiredSource)?.sourceId ?? descriptor?.sourceBindings[0]?.sourceId ?? '';
    bindingsLoading = Boolean(descriptor); status = descriptor ? '' : '尚未安装游戏支持插件；备份和 Agent 仍可使用。';
    persist(); selection(); update();
    if (!descriptor) return;
    try {
      const games = await context.call('games.list', { gameId: currentGame, pageSize: 100 });
      if (!active() || generation !== bindingGeneration || currentGame !== gameId) return;
      const enrolled = games.items.some(item => item.gameId === currentGame);
      let profileCursor: string | undefined;
      const loaded: ProfileSummary[] = [];
      // Signed game support allows browsing before an installation is enrolled.
      // Only enrolled games can have Library profiles.
      if (enrolled) {
        do {
          const result = await context.call('profiles.list', { gameId: currentGame, pageSize: 100, ...(profileCursor ? { cursor: profileCursor } : {}) });
          if (!active() || generation !== bindingGeneration || currentGame !== gameId) return;
          loaded.push(...result.items); profileCursor = result.nextCursor;
        } while (profileCursor);
      }
      profiles = loaded; profileId = (profiles.find(item => item.profileId === desiredProfile) ?? profiles.find(item => item.active) ?? profiles[0])?.profileId ?? '';
      profileHint = profileId ? '' : enrolled
        ? '可浏览模组；请先在模组管理中创建配置，才能添加模组。'
        : '可浏览模组；请先在游戏设置中注册真实安装目录，再创建配置以添加模组。';
      persist(); selection(); update();
    } catch (cause) {
      if (active() && generation === bindingGeneration && currentGame === gameId) fail(cause, 'bindings');
    } finally {
      if (active() && generation === bindingGeneration && currentGame === gameId) { bindingsLoading = false; update(); }
    }
  };
  const search = async (next = false): Promise<void> => {
    if (!gameId || !sourceId) throw new Error('所选游戏没有已配置的在线来源绑定。');
    if (loading || (next && !cursor)) return;
    const generation = ++requestGeneration;
    const binding = descriptors.find(item => item.gameId === gameId)?.sourceBindings.find(item => item.sourceId === sourceId);
    const params = { sourceId, gameId, query, pageSize: 50, ...(binding?.remoteGameKey ? { remoteGameKey: binding.remoteGameKey } : {}), ...(next && cursor ? { cursor } : {}) };
    loading = true; searched = true; error = undefined; status = ''; if (!next) items = []; update(); persist();
    try {
      const response = await context.query(JSON.stringify(params), () => context.call('sources.search', params));
      if (!active() || generation !== requestGeneration) return;
      items = next ? [...items, ...response.items] : response.items; cursor = response.nextCursor;
      status = cursor ? '还有更多来源结果。' : '已显示全部来源结果。';
    } catch (cause) {
      if (active() && generation === requestGeneration) { cursor = undefined; fail(cause, 'search'); }
    } finally { if (active() && generation === requestGeneration) { loading = false; update(); } }
  };
  const add = async (reference: ModRef): Promise<void> => {
    if (adding) return;
    const target = profiles.find(item => item.profileId === profileId);
    if (!target) throw new Error('请选择目标配置。');
    adding = true; update();
    const generation = bindingGeneration, searchGeneration = requestGeneration;
    error = undefined;
    try {
      const content = await context.call('profiles.contentState', { profileId: target.profileId });
      if (!active()) return;
      if (!content.writable) throw new Error('配置内容正在处理或需要恢复，请稍后重试。');
      const job = await context.request<JobRef>('mods.add', { profileId: target.profileId, reference, expectedContentRevision: content.contentRevision }, crypto.randomUUID());
      if (!active()) return;
      selected = undefined; status = `添加任务 ${job.jobId} 已交给 daemon，关闭页面不会取消任务。`; context.notify(status);
    } catch (cause) {
      if (active() && generation === bindingGeneration && searchGeneration === requestGeneration) fail(cause, 'add');
    } finally { if (active()) { adding = false; update(); } }
  };
  function View() {
    const [, setTick] = React.useState(0); redraw = () => setTick(value => value + 1);
    const descriptor = descriptors.find(item => item.gameId === gameId);
    const visible = items.filter(item => !filter || `${item.displayName} ${item.summary ?? ''}`.toLocaleLowerCase().includes(filter.toLocaleLowerCase()));
    const alternatives = (descriptor?.sourceBindings ?? []).filter(binding => binding.sourceId !== sourceId && availableSources.has(binding.sourceId));
    const switchSource = (value: string): void => { sourceId = value; clearResults(); persist(); run(() => search()); };
    const sourceNames: Record<string, string> = { modio: 'mod.io', modcat: 'ModCat', nexusmods: 'Nexus Mods' };
    const errorAlert = error ? h(Antd.Alert, {
      type: 'error', showIcon: true, className: 'mintcat-search-alert', title: errorContext === 'bindings' ? error.title : `${sourceNames[sourceId] ?? sourceId}：${error.title}`,
      description: h('div', null, h('p', null, error.description),
        error.diagnostics.length ? h('details', null, h('summary', null, '技术详情'), h('pre', null, error.diagnostics.join('\n'))) : null),
      action: h('div', { className: 'mintcat-search-alert-actions' },
        error.retryable && errorContext !== 'add' ? h(Antd.Button, { size: 'small', disabled: loading || bindingsLoading, onClick: () => {
          context.invalidate(''); if (errorContext === 'bindings') run(refreshModules, 'bindings'); else run(() => search());
        } }, errorContext === 'bindings' ? '重新读取配置' : '重试') : null,
        errorContext === 'search' ? alternatives.map(binding => h(Antd.Button, { key: binding.sourceId, size: 'small', onClick: () => switchSource(binding.sourceId) }, `切换到 ${binding.sourceId === 'modcat' ? 'ModCat' : binding.sourceId}`)) : null),
    }) : null;
    const toolbar = h('div', { className: `mintcat-search-header${context.toolbar ? ' mintcat-search-header--portal' : ''}` },
        h('div', { className: 'mintcat-search-game-control' },
          h('span', { className: 'mintcat-search-game-badge', 'aria-hidden': true }, h(Icons.CompassOutlined)),
          h(Antd.Select<string>, { variant: 'borderless', value: gameId || undefined, placeholder: '选择游戏', 'aria-label': '游戏', popupMatchSelectWidth: false, disabled: !descriptors.length, options: descriptors.map(item => ({ value: item.gameId, label: item.displayName })), className: 'mintcat-search-game-select', onChange: (value: string) => { gameId = value; run(loadBindings); } })),
        h('label', { className: `mintcat-search-box${!sourceId ? ' mintcat-search-box--disabled' : ''}` },
          h(Icons.SearchOutlined),
          h('input', {
            type: 'search',
            value: query,
            placeholder: '搜索在线模组名称或关键词…',
            'aria-label': '搜索在线模组名称或关键词',
            disabled: !sourceId,
            onChange: (event: { target: { value: string } }) => { query = event.target.value; clearResults(); persist(); },
            onKeyDown: (event: { key: string; preventDefault(): void }) => {
              if (event.key === 'Enter' && sourceId && !loading) { event.preventDefault(); run(() => search()); }
            },
          }),
          query ? h('button', { type: 'button', className: 'mintcat-search-box-clear', 'aria-label': '清除关键词', onClick: () => { query = ''; clearResults(); persist(); } }, '×') : h('kbd', { 'aria-hidden': true }, '↵')),
        h('div', { className: `mintcat-search-profile-control${bindingsLoading || !profiles.length ? ' mintcat-search-profile-control--disabled' : ''}` },
          h('span', { className: 'mintcat-search-profile-label' }, h(Icons.ProfileOutlined), '配置'),
          h(Antd.Select<string>, { variant: 'borderless', value: profileId || undefined, placeholder: '选择目标配置', 'aria-label': '目标配置', popupMatchSelectWidth: false, loading: bindingsLoading, disabled: bindingsLoading || !profiles.length, options: profiles.map(item => ({ value: item.profileId, label: item.displayName })), className: 'mintcat-search-profile-select', onChange: (value: string) => { profileId = value; persist(); selection(); update(); } })),
        h('div', { className: 'mintcat-search-toolbar-group' },
          h(Antd.Button, { type: 'text', icon: h(Icons.HomeOutlined), title: '浏览首页', disabled: loading || !sourceId, onClick: () => { query = ''; clearResults(); run(() => search()); } }),
          h(Antd.Button, { type: 'text', icon: h(Icons.ReloadOutlined, { spin: loading }), title: '刷新搜索', disabled: loading || !sourceId, onClick: () => { context.invalidate(''); run(() => search()); } }),
          h(Antd.Button, { type: 'text', icon: h(Icons.SyncOutlined), title: '重新读取游戏与来源', disabled: loading || bindingsLoading, onClick: () => run(refreshModules, 'bindings') })));
    const sourceBindings = descriptor?.sourceBindings ?? [];
    const showFooterStatus = bindingsLoading || Boolean(sourceId && status);
    return h('div', { className: `mintcat-search-page${context.toolbar ? ' mintcat-search-page--portal' : ''}` },
      context.toolbar ? createPortal(toolbar, context.toolbar) : toolbar,
      sourceBindings.length ? h('div', { className: 'mintcat-search-subbar' },
        h(Antd.Tabs, { activeKey: sourceId, size: 'small', className: 'mintcat-search-tabs', items: sourceBindings.map(binding => ({ key: binding.sourceId, label: binding.sourceId })), onChange: switchSource }),
        items.length || filter ? h('div', { className: 'mintcat-search-filters' },
          h(Antd.Input, { value: filter, allowClear: true, size: 'small', prefix: h(Icons.FilterOutlined), placeholder: '筛选已加载结果…', 'aria-label': '筛选已加载结果', onChange: (event: { target: { value: string } }) => { filter = event.target.value; update(); }, className: 'mintcat-search-filter-input' }),
          h('span', { className: 'mintcat-search-count-pill' }, `${visible.length} / ${items.length} 个结果`)) : null) : null,
      errorContext !== 'add' ? errorAlert : null,
      profileHint ? h(Antd.Alert, { type: 'info', showIcon: true, className: 'mintcat-search-alert', title: profileHint }) : null,
      !sourceId && !bindingsLoading ? h('div', { className: 'mc-empty-hero' },
        h('div', { className: 'mc-empty-hero-card' },
          h('div', { className: 'mc-empty-hero-orb', 'aria-hidden': true }, h(Icons.CompassOutlined)),
          h('h3', { className: 'mc-empty-hero-title' }, descriptors.length ? '当前游戏暂无可用在线来源' : '尚未安装游戏支持插件'),
          h('p', { className: 'mc-empty-hero-desc' }, descriptors.length
            ? '所选游戏没有已配置的在线来源。请配置支持此游戏的来源；本地文件可从本地导入添加。'
            : '尚未安装游戏支持插件。安装游戏支持后即可浏览在线模组；备份和 Agent 仍可使用。'),
          h('div', { className: 'mc-empty-hero-actions' },
            descriptors.length
              ? h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.ImportOutlined), onClick: () => navigateUi('local') }, '前往本地导入')
              : h(React.Fragment, null,
                h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.RobotOutlined), onClick: () => navigateUi('robot') }, '打开 Agent 助手'),
                h(Antd.Button, { icon: h(Icons.CloudOutlined), onClick: () => navigateUi('cloud') }, '备份与恢复'))),
          !descriptors.length ? h('div', { className: 'mc-empty-bento' },
            h('div', { className: 'mc-empty-bento-item' },
              h('div', { className: 'mc-empty-bento-head' }, h(Icons.SafetyCertificateOutlined), '签名游戏扩展'),
              h('span', { className: 'mc-empty-bento-text' }, '接入已签名游戏包后自动解锁 mod.io、ModCat 与 Nexus 在线检索。')),
            h('button', { type: 'button', className: 'mc-empty-bento-item', onClick: () => navigateUi('cloud') },
              h('div', { className: 'mc-empty-bento-head' }, h(Icons.CloudUploadOutlined), '备份与迁移'),
              h('span', { className: 'mc-empty-bento-text' }, '随时导出便携归档、管理云快照或导入旧版配置数据。')),
            h('button', { type: 'button', className: 'mc-empty-bento-item', onClick: () => navigateUi('robot') },
              h('div', { className: 'mc-empty-bento-head' }, h(Icons.RobotOutlined), '智能模组助手'),
              h('span', { className: 'mc-empty-bento-text' }, '支持 OpenAI 兼容模型对话，所有变更均经人工审核确认。'))) : null)) : null,
      loading && !items.length ? h(Antd.Skeleton, { active: true, paragraph: { rows: 3 } }) : null,
      !loading && !error && sourceId && !visible.length ? h('div', { className: 'mc-empty-hero' },
        h('div', { className: 'mc-empty-hero-card' },
          h('div', { className: 'mc-empty-hero-orb', 'aria-hidden': true }, h(searched ? Icons.InboxOutlined : Icons.SearchOutlined)),
          h('h3', { className: 'mc-empty-hero-title' }, searched ? '没有找到模组' : '探索在线模组库'),
          h('p', { className: 'mc-empty-hero-desc' }, searched
            ? '没有找到匹配的模组，请尝试更换关键词或切换其他在线来源。'
            : '在顶部输入关键词回车搜索，或直接浏览当前来源的热门模组。'),
          !searched ? h('div', { className: 'mc-empty-hero-actions' },
            h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.HomeOutlined), onClick: () => { query = ''; clearResults(); run(() => search()); } }, '浏览热门模组')) : null)) : null,
      visible.length ? h('div', { className: 'mintcat-search-results' }, ...visible.map((item, index) => h('article', { key: `${JSON.stringify(item.reference)}:${index}`, className: 'mintcat-search-card' },
        h('div', { className: 'mintcat-search-media' },
          item.logoUrl && /^https?:\/\//i.test(item.logoUrl)
            ? h('img', { className: 'mintcat-search-thumbnail', src: item.logoUrl, alt: item.displayName })
            : h('div', { className: 'mintcat-search-cover-placeholder', 'aria-hidden': true }, h(Icons.AppstoreOutlined))),
        h('div', { className: 'mintcat-search-info' },
          h('div', { className: 'mintcat-search-card-title' },
            item.profileUrl && /^https?:\/\//i.test(item.profileUrl)
              ? h(Antd.Typography.Link, { strong: true, href: item.profileUrl, target: '_blank', rel: 'noopener noreferrer' }, item.displayName, ' ', h(Icons.ExportOutlined, { className: 'mintcat-search-ext-icon' }))
              : h(Antd.Typography.Text, { strong: true }, item.displayName)),
          h('div', { className: 'mintcat-search-summary' }, item.summary || '此来源未提供模组简介。'),
          h('div', { className: 'mintcat-search-meta-tags' },
            h('span', { className: `mintcat-search-source-tag mintcat-search-source-tag--${item.reference.sourceId}` }, sourceNames[item.reference.sourceId] ?? item.reference.sourceId),
            h('span', { className: 'mintcat-search-meta-chip' }, `游戏 ${item.reference.remoteGameKey}`),
            h('span', { className: 'mintcat-search-meta-chip' }, `#${item.reference.remoteModId}`))),
        h('div', { className: 'mintcat-search-card-actions' },
          h(Antd.Button, { size: 'small', className: 'mintcat-search-add-btn', icon: h(Icons.PlusOutlined), disabled: !profileId || adding, title: '添加到配置', onClick: () => { error = undefined; selected = item; update(); } }, '添加'))))) : null,
      cursor ? h(Antd.Button, { block: true, className: 'mintcat-search-more-btn', loading, onClick: () => run(() => search(true)) }, '加载更多结果') : null,
      showFooterStatus ? h('div', { className: 'mintcat-search-footer-status' },
        h(Antd.Typography.Text, { type: 'secondary', role: 'status' }, bindingsLoading ? '正在读取配置…' : status)) : null,
      h(Antd.Modal, { open: Boolean(selected), title: '添加模组到配置', okText: '确认添加', cancelText: '取消', confirmLoading: adding, okButtonProps: { disabled: !profileId || adding, className: 'glass-primary-action' }, onCancel: () => { if (!adding) { selected = undefined; if (errorContext === 'add') error = undefined; update(); } }, onOk: () => { if (selected) run(() => add(selected!.reference), 'add'); } },
        h(Antd.Typography.Paragraph, { strong: true, style: { marginBottom: 10 } }, selected?.displayName),
        errorAlert,
        h(Antd.Select<string>, { style: { width: '100%' }, value: profileId || undefined, placeholder: '选择目标配置', options: profiles.map(item => ({ value: item.profileId, label: item.displayName })), onChange: (value: string) => { profileId = value; persist(); selection(); update(); } }),
        h(Antd.Alert, { style: { marginTop: 12 }, type: 'info', showIcon: true, title: '下载及添加由 daemon 执行；配置写锁与当前内容版本将由 Application API 校验。' })));
  }
  const view = mountReactView(root, h(View));
  const refreshModules = async (): Promise<void> => {
    const generation = ++bindingGeneration;
    bindingsLoading = true; update();
    try {
      const [current, sources] = await Promise.all([
        context.request<GameModuleList>('modules.list', {}), context.call('sources.list', {}),
      ]);
      if (!active() || generation !== bindingGeneration) return;
      const previous = JSON.stringify(descriptors.find(item => item.gameId === gameId));
      availableSources = new Set(sources.sources.map(source => source.sourceId));
      descriptors = current.modules.flatMap(module => module.enabled && module.game?.descriptor ? [{ ...module.game.descriptor, sourceBindings: module.game.descriptor.sourceBindings.filter(binding => availableSources.has(binding.sourceId)) }] : []);
      gameId = descriptors.some(item => item.gameId === gameId) ? gameId : descriptors[0]?.gameId ?? '';
      if (previous !== JSON.stringify(descriptors.find(item => item.gameId === gameId)) || !profiles.length) { context.invalidate(''); await loadBindings(); } else update();
    } catch (cause) {
      if (active() && generation === bindingGeneration) fail(cause, 'bindings');
    } finally {
      if (active() && generation === bindingGeneration) { bindingsLoading = false; update(); }
    }
  };
  const subscription = context.onInventoryChanged(() => { run(refreshModules); });
  run(refreshModules);
  return { dispose() { persist(); alive = false; ++requestGeneration; ++bindingGeneration; subscription.dispose(); view.dispose(); } };
}
