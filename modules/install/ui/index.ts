import type { DetectedInstallInput, InstallReceiptSummary, JobRef, JobSnapshot, ProfileSummary, ModSummary } from '@mintcat/sdk-ts';
import { getSharedRuntime, mountReactView, presentUiError, publishUiSelection } from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, GameUiDescriptor, Json, UiContext } from '@mintcat/sdk-ui';
type TrackedTask = { jobId: string; actionId: string };
interface InstallPlanResult { planId: string; planHash: string }
type InputMetadata = { profileModId: string; format: string; audioOnlyHint: boolean };

function formatCause(cause: unknown): string {
  if (typeof cause === 'object' && cause !== null && 'appError' in cause) {
    const failure = presentUiError(cause);
    return `${failure.title}：${failure.description}`;
  }
  return cause instanceof Error ? cause.message : JSON.stringify(cause, null, 2) ?? String(cause);
}

export async function mountInstall(root: HTMLElement, context: UiContext): Promise<Disposable> {
  const { React, Antd, Icons } = getSharedRuntime(); const h = React.createElement;
  const saved = context.state('install', 1, { gameId: '', profileId: '', installationId: '', variantId: '', jobs: [] as TrackedTask[], inputMetadata: {} as Record<string, InputMetadata> });
  let alive = true, busy = false, loading = false, selectionGeneration = 0, pollPending = false;
  const active = (): boolean => alive && !context.signal.aborted;
  let gameId = saved.gameId, profileId = saved.profileId, installationId = saved.installationId, variantId = saved.variantId;
  let descriptors: GameUiDescriptor[] = [], profiles: ProfileSummary[] = [], mods: ModSummary[] = [];
  let installations: Array<{ installationId: string; rootPath?: string }> = [];
  let receipts: InstallReceiptSummary[] = [];
  let detectedInputs: Record<string, DetectedInstallInput> = {};
  let metadataProfileId = '', error = '', status = '', existingJob = '', receiptId = '', receiptRevision = '';
  const metadata: Record<string, InputMetadata> = {}, tracked = saved.jobs;
  const snapshots: Record<string, JobSnapshot> = {}, taskErrors: Record<string, string> = {};
  let redraw = (): void => {};
  const update = (): void => { if (active()) redraw(); };
  const persist = (): void => context.saveState('install', 1, { gameId, profileId, installationId, variantId, jobs: tracked.map(task => ({ ...task })), inputMetadata: metadataProfileId === profileId ? Object.fromEntries(Object.entries(metadata).map(([key, value]) => [key, { ...value }])) : {} });
  const selection = (): void => { const descriptor = descriptors.find(item => item.gameId === gameId); if (descriptor) publishUiSelection({ gameId, gameName: descriptor.displayName, profileId: profileId || undefined }); };
  const run = (action: () => Promise<void>): void => {
    if (busy || !active()) return;
    busy = true; error = ''; update();
    context.guard(async () => {
      try { await action(); } catch (cause) { if (active()) { error = formatCause(cause); update(); } throw cause; }
      finally { if (active()) { busy = false; update(); } }
    })();
  };
  const loadReceipts = async (): Promise<void> => {
    const currentInstallation = installationId;
    if (!currentInstallation) { receipts = []; return; }
    try {
      const result = await context.call('install.receipts', { installationId: currentInstallation });
      if (!active() || installationId !== currentInstallation) return;
      receipts = result.items;
      if (receipts.length && !receiptId) {
        receiptId = receipts[0].receiptId;
        receiptRevision = String(receipts[0].profileRevision);
      }
    } catch {
      if (active() && installationId === currentInstallation) receipts = [];
    }
  };
  const loadProfile = async (): Promise<void> => {
    const current = profileId, generation = ++selectionGeneration; loading = true; update();
    try {
      if (!current) { mods = []; detectedInputs = {}; metadataProfileId = ''; for (const key of Object.keys(metadata)) delete metadata[key]; return; }
      let cursor: string | undefined;
      const loaded: ModSummary[] = [];
      do {
        const result = await context.call('mods.list', { profileId: current, pageSize: 100, ...(cursor ? { cursor } : {}) });
        if (!active() || generation !== selectionGeneration || profileId !== current) return;
        loaded.push(...result.items); cursor = result.nextCursor;
      } while (cursor);
      let detectedMap: Record<string, DetectedInstallInput> = {};
      try {
        const detected = await context.call('install.detectInputs', { profileId: current });
        if (!active() || generation !== selectionGeneration || profileId !== current) return;
        detectedMap = Object.fromEntries(detected.items.map(item => [item.profileModId, item]));
      } catch {
        detectedMap = {};
      }
      const previous = metadataProfileId === current ? { ...metadata } : saved.profileId === current ? saved.inputMetadata ?? {} : {};
      mods = loaded; detectedInputs = detectedMap; metadataProfileId = current;
      for (const key of Object.keys(metadata)) delete metadata[key];
      for (const mod of mods.filter(item => item.enabled)) {
        const auto = detectedMap[mod.profileModId];
        metadata[mod.profileModId] = {
          profileModId: mod.profileModId,
          format: auto?.format ?? previous[mod.profileModId]?.format ?? '',
          audioOnlyHint: auto?.verified ? auto.audioOnlyHint : previous[mod.profileModId]?.audioOnlyHint === true,
        };
      }
      persist(); selection();
    } finally { if (active() && generation === selectionGeneration) { loading = false; update(); } }
  };
  const loadGame = async (): Promise<void> => {
    const current = gameId, generation = ++selectionGeneration;
    const desiredProfile = profileId || saved.profileId, desiredVariant = variantId || saved.variantId, desiredInstallation = installationId || saved.installationId;
    const previousProfile = metadataProfileId, previousMetadata = structuredClone(metadata);
    loading = true; profiles = []; installations = []; profileId = ''; installationId = ''; variantId = ''; mods = []; metadataProfileId = '';
    for (const key of Object.keys(metadata)) delete metadata[key]; update();
    try {
      const descriptor = descriptors.find(item => item.gameId === current);
      if (!descriptor) { status = '没有启用的已签名游戏适配器。已有 daemon 任务仍可查看。'; return; }
      variantId = descriptor.variants.find(item => item.variantId === desiredVariant)?.variantId ?? descriptor.variants[0]?.variantId ?? '';
      const games = await context.call('games.list', { gameId: current, pageSize: 100 });
      if (!active() || generation !== selectionGeneration || current !== gameId) return;
      const enrolled = games.items.find(item => item.gameId === current);
      if (!enrolled) { status = '请先在模组管理中注册真实游戏安装目录，再创建配置。已有 daemon 任务仍可查看。'; persist(); selection(); return; }
      installations = enrolled.installations;
      installationId = installations.find(item => item.installationId === desiredInstallation)?.installationId ?? installations[0]?.installationId ?? '';
      let cursor: string | undefined; const loaded: ProfileSummary[] = [];
      do {
        const result = await context.call('profiles.list', { gameId: current, pageSize: 100, ...(cursor ? { cursor } : {}) });
        if (!active() || generation !== selectionGeneration || current !== gameId) return;
        loaded.push(...result.items); cursor = result.nextCursor;
      } while (cursor);
      profiles = loaded; profileId = (profiles.find(item => item.profileId === desiredProfile) ?? profiles.find(item => item.active) ?? profiles[0])?.profileId ?? '';
      if (profileId === previousProfile) { metadataProfileId = previousProfile; Object.assign(metadata, previousMetadata); }
      persist(); selection();
      await loadReceipts();
      if (profileId) await loadProfile(); else status = '请先创建配置。';
    } finally { if (active() && generation === selectionGeneration) { loading = false; update(); } }
  };
  const track = (job: JobRef): void => {
    if (!active()) {
      const latest = context.state('install', 1, saved);
      if (!latest.jobs.some(task => task.jobId === job.jobId)) latest.jobs.push({ jobId: job.jobId, actionId: '' });
      context.saveState('install', 1, latest); return;
    }
    if (!tracked.some(task => task.jobId === job.jobId)) tracked.push({ jobId: job.jobId, actionId: '' });
    persist(); status = `任务 ${job.jobId} 已交给 daemon；关闭页面不会取消。`; context.notify(status); update();
  };
  const poll = async (): Promise<void> => {
    if (pollPending || !active()) return; pollPending = true;
    try {
      for (const task of tracked.slice()) {
        try {
          let afterSeq = 0;
          let page = await context.call('jobs.watch', { jobId: task.jobId, afterSeq, limit: 200 });
          for (;;) {
            if (!active()) return;
            const event = page.events.slice().reverse().find(item => typeof item.details?.actionId === 'string');
            if (event) task.actionId = event.details!.actionId as string;
            if (page.nextAfterSeq >= page.snapshot.lastSeq || page.nextAfterSeq <= afterSeq) break;
            afterSeq = page.nextAfterSeq; page = await context.call('jobs.watch', { jobId: task.jobId, afterSeq, limit: 200 });
          }
          snapshots[task.jobId] = page.snapshot; taskErrors[task.jobId] = '';
        } catch (cause) { if (active()) taskErrors[task.jobId] = formatCause(cause); }
      }
      if (active()) { persist(); update(); }
    } finally { pollPending = false; }
  };
  const start = async (method: 'install.plan' | 'mods.pack'): Promise<void> => {
    if (metadataProfileId !== profileId || loading) throw new Error('请等待当前配置输入读取完成。');
    const selected = profiles.find(item => item.profileId === profileId);
    if (!selected || !installationId || !variantId) throw new Error('请选择配置、安装位置及已签名游戏变体。');
    const currentGame = gameId, currentInstallation = installationId, currentVariant = variantId, inputMetadata = structuredClone(Object.values(metadata));
    const content = await context.call('profiles.contentState', { profileId: selected.profileId });
    if (!active()) return;
    if (!content.writable) throw new Error('配置正在处理或需要恢复。');
    if (content.contentRevision !== selected.contentRevision) { await loadGame(); throw new Error('配置内容已变化，请重新审核当前输入。'); }
    if (inputMetadata.some(item => !item.format)) throw new Error('请为每个启用的输入选择准确格式。');
    const settings = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', { scope: { kind: 'game', gameId: currentGame }, section: 'game' });
    if (!active()) return;
    track(await context.request<JobRef>(method, { profileId: selected.profileId, revision: selected.revision, contentRevision: selected.contentRevision, gameId: currentGame, installationId: currentInstallation, options: { variantId: currentVariant, settingsRevision: settings.revision, settings: settings.values, inputMetadata } }, crypto.randomUUID())); await poll();
  };
  const approve = async (task: TrackedTask, snapshot: JobSnapshot): Promise<void> => {
    const action = await context.call('actions.get', { actionId: task.actionId });
    if (!active()) return;
    if (action.state !== 'pending' || !['install-approval', 'uninstall-approval'].includes(action.kind) || (action.kind === 'install-approval' ? snapshot.command !== 'install.plan' : snapshot.command !== 'install.uninstall')) throw new Error('此动作与待审核安装操作不匹配。');
    const plan = snapshot.result as InstallPlanResult | undefined;
    if (action.kind === 'install-approval' && (!plan?.planId || plan.planHash !== action.planHash)) throw new Error('已验证计划与批准目标不匹配，拒绝部署。');
    if (!window.confirm(`以人工身份批准 ${action.kind}？\n\n${JSON.stringify({ action, result: snapshot.result }, null, 2)}`)) return;
    await context.call('actions.approve', { actionId: action.actionId, expectedRevision: action.revision, response: { approved: true } }, crypto.randomUUID());
    if (!active()) return;
    if (action.kind === 'install-approval') track(await context.call('install.apply', { planId: plan!.planId, planHash: plan!.planHash, approvalRef: action.actionId }, crypto.randomUUID()));
    await poll();
  };
  function View() {
    const [, setTick] = React.useState(0); redraw = () => setTick(value => value + 1);
    const descriptor = descriptors.find(item => item.gameId === gameId);
    const canPlan = Boolean(descriptor && profileId && installationId && variantId && metadataProfileId === profileId && !loading);
    return h('div', { className: 'mintcat-install-page' },
      h('div', { className: 'mintcat-install-header' },
        h('div', { className: 'mintcat-install-heading' },
          h(Antd.Typography.Title, { level: 5, style: { margin: 0 } }, h(Icons.DeploymentUnitOutlined), ' 安装与打包'),
          h('span', { className: 'mintcat-install-subtitle' }, '校验启用模组格式，生成不可变安装计划或独立打包产物')),
        h(Antd.Button, { icon: h(Icons.ReloadOutlined), disabled: busy, onClick: () => run(async () => { await loadGame(); await poll(); }) }, '刷新')),
      error ? h(Antd.Alert, { type: 'error', showIcon: true, title: error }) : null,
      status ? h(Antd.Alert, { type: 'info', showIcon: true, role: 'status', title: status }) : h('span', { role: 'status', className: 'mintcat-install-sr-status' }),
      h(Antd.Card, { size: 'small', title: '当前安装目标' },
        h(Antd.Form, { layout: 'vertical' },
          h('div', { className: 'mintcat-install-targets' },
            h(Antd.Form.Item, { label: '游戏' }, h(Antd.Select<string>, { value: gameId || undefined, disabled: busy || loading, options: descriptors.map(item => ({ value: item.gameId, label: item.displayName })), placeholder: '没有游戏适配器', onChange: (value: string) => { gameId = value; run(loadGame); } })),
            h(Antd.Form.Item, { label: '配置' }, h(Antd.Select<string>, { value: profileId || undefined, disabled: busy || loading || !profiles.length, options: profiles.map(item => ({ value: item.profileId, label: item.displayName })), placeholder: '请选择配置', onChange: (value: string) => { profileId = value; run(loadProfile); } })),
            h(Antd.Form.Item, { label: '安装位置' }, h(Antd.Select<string>, { value: installationId || undefined, disabled: busy || loading || !installations.length, options: installations.map(item => ({ value: item.installationId, label: item.rootPath || item.installationId })), placeholder: '没有可用安装位置', onChange: (value: string) => { installationId = value; receiptId = ''; receiptRevision = ''; persist(); run(loadReceipts); } })),
            h(Antd.Form.Item, { label: '游戏变体' }, h(Antd.Select<string>, { value: variantId || undefined, disabled: busy || loading || !descriptor, options: (descriptor?.variants ?? []).map(item => ({ value: item.variantId, label: item.displayName })), placeholder: '请选择已签名变体', onChange: (value: string) => { variantId = value; persist(); update(); } })))),
            !canPlan && !loading && !status ? h(Antd.Alert, { type: 'info', showIcon: true, title: '需要启用的已签名游戏模块、配置、安装位置与变体，才能创建计划。' }) : null,
        h(Antd.Table<ModSummary>, { size: 'small', loading, rowKey: 'profileModId', dataSource: mods.filter(item => item.enabled), pagination: false, locale: { emptyText: '此配置没有启用的输入' }, columns: [
          { title: '启用的模组', dataIndex: 'displayName' },
          { title: '缓存状态', width: 150, render: (_: unknown, mod: ModSummary) => {
            const info = detectedInputs[mod.profileModId];
            if (!info?.verified) return h(Antd.Tag, { color: 'warning' }, '未缓存（需先下载）');
            return h(Antd.Tag, { color: 'success' }, info.format ? `已识别 · ${info.format}` : '已缓存');
          } },
          { title: '准确输入格式', width: 220, render: (_: unknown, mod: ModSummary) => h(Antd.Select<string>, { style: { width: '100%' }, size: 'small', value: metadata[mod.profileModId]?.format || undefined, placeholder: '选择已验证格式', disabled: busy, options: [{ value: 'pak', label: 'PAK 归档 (.pak)' }, { value: 'zip', label: 'ZIP 压缩包 (.zip)' }, { value: 'directory-snapshot', label: '目录快照' }], onChange: (value: string) => { metadata[mod.profileModId].format = value; persist(); update(); } }) },
          { title: '仅音频提示', width: 110, align: 'center' as const, render: (_: unknown, mod: ModSummary) => h(Antd.Checkbox, { checked: metadata[mod.profileModId]?.audioOnlyHint, disabled: busy, onChange: (event: { target: { checked: boolean } }) => { metadata[mod.profileModId].audioOnlyHint = event.target.checked; persist(); update(); } }) },
        ] }),
        h(Antd.Space, { wrap: true, style: { marginTop: 14 } },
          h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.DeploymentUnitOutlined), disabled: busy || !canPlan, onClick: () => run(() => start('install.plan')) }, '生成不可变安装计划'),
          h(Antd.Button, { icon: h(Icons.InboxOutlined), disabled: busy || !canPlan, onClick: () => run(() => start('mods.pack')) }, '仅打包，不部署'))),
      h(Antd.Card, { size: 'small', title: '凭安装收据卸载', extra: h(Antd.Button, { size: 'small', icon: h(Icons.ReloadOutlined), disabled: busy || !installationId, onClick: () => run(loadReceipts) }, '刷新收据') },
        receipts.length ? h(Antd.List<InstallReceiptSummary>, {
          size: 'small',
          style: { marginBottom: 12 },
          dataSource: receipts,
          renderItem: (item: InstallReceiptSummary) => h(Antd.List.Item, {
            actions: [
              h(Antd.Button, {
                key: 'uninstall', size: 'small', danger: true, disabled: busy || !installationId,
                onClick: () => run(async () => {
                  track(await context.call('install.uninstall', { installationId, receiptId: item.receiptId, revision: item.profileRevision }, crypto.randomUUID()));
                  await poll();
                }),
              }, '一键卸载'),
            ],
          }, h(Antd.List.Item.Meta, {
            title: `收据 ${item.receiptId.slice(0, 12)}… · 配置版本 r${item.profileRevision}`,
            description: `${item.fileCount} 个部署文件 · ${new Date(item.committedAtMs).toLocaleString()}`,
          })),
        }) : null,
        h('div', { className: 'mintcat-install-uninstall-row' },
          h(Antd.Input, { value: receiptId, placeholder: '已验证收据 ID', 'aria-label': '已验证收据 ID', disabled: busy, className: 'mintcat-install-receipt-input', onChange: (event: { target: { value: string } }) => { receiptId = event.target.value; update(); } }),
          h(Antd.Input, { value: receiptRevision, type: 'number', min: 0, placeholder: '绑定的配置版本', 'aria-label': '收据绑定的配置版本', disabled: busy, className: 'mintcat-install-revision-input', onChange: (event: { target: { value: string } }) => { receiptRevision = event.target.value; update(); } }),
          h(Antd.Button, { danger: true, disabled: busy || !installationId || !receiptId.trim() || !receiptRevision.trim(), onClick: () => run(async () => {
            if (!receiptId.trim() || !installationId || !receiptRevision.trim() || !Number.isSafeInteger(Number(receiptRevision)) || Number(receiptRevision) < 0) throw new Error('请选择安装位置、准确收据及其绑定的配置版本。');
            track(await context.call('install.uninstall', { installationId, receiptId: receiptId.trim(), revision: Number(receiptRevision) }, crypto.randomUUID())); await poll();
          }) }, '请求卸载'))),
      h(Antd.Card, {
        size: 'small',
        title: 'daemon 持久任务',
        extra: h(Antd.Space, { size: 6 },
          tracked.some(task => ['succeeded', 'failed', 'cancelled'].includes(snapshots[task.jobId]?.state ?? ''))
            ? h(Antd.Button, { size: 'small', onClick: () => {
              const keep = tracked.filter(task => !['succeeded', 'failed', 'cancelled'].includes(snapshots[task.jobId]?.state ?? ''));
              tracked.splice(0, tracked.length, ...keep);
              persist(); update();
            } }, '清除已完成') : null,
          h(Antd.Button, { size: 'small', icon: h(Icons.ReloadOutlined), disabled: busy, onClick: () => run(poll) }, '刷新任务')),
      },
        h(Antd.Space.Compact, { style: { width: '100%', marginBottom: 12 } }, h(Antd.Input, { value: existingJob, placeholder: '已有 daemon 任务 ID', disabled: busy, onChange: (event: { target: { value: string } }) => { existingJob = event.target.value; update(); } }), h(Antd.Button, { disabled: busy || !existingJob.trim(), onClick: () => run(async () => {
          const snapshot = await context.call('jobs.get', { jobId: existingJob.trim() });
          if (!active()) return;
          if (!['install.plan', 'mods.pack', 'install.apply', 'install.uninstall'].includes(snapshot.command)) throw new Error('此任务不是安装或打包操作。');
          track(snapshot); await poll();
        }) }, '关联任务')),
        tracked.length ? h(Antd.Collapse, { size: 'small', items: tracked.map(task => {
          const snapshot = snapshots[task.jobId];
          return { key: task.jobId, label: h(Antd.Space, null, h('strong', null, snapshot?.command ?? task.jobId), h(Antd.Tag, { color: snapshot?.state === 'awaiting-approval' ? 'warning' : snapshot?.state === 'succeeded' ? 'success' : snapshot?.state === 'failed' ? 'error' : 'processing' }, snapshot?.state ?? '正在读取')), children: h('div', null,
            h('pre', null, JSON.stringify(snapshot, null, 2)), taskErrors[task.jobId] ? h(Antd.Alert, { type: 'error', title: taskErrors[task.jobId] }) : null,
            snapshot?.state === 'awaiting-approval' && task.actionId ? h(Antd.Button, { type: 'primary', className: 'glass-primary-action', disabled: busy, onClick: () => run(() => approve(task, snapshot)) }, '审核并批准不可变目标') : null,
            snapshot && !['succeeded', 'cancelled', 'failed', 'recovery-required'].includes(snapshot.state) ? h(Antd.Button, { disabled: busy, style: { marginLeft: 8 }, onClick: () => run(async () => { await context.call('jobs.cancel', { jobId: task.jobId, expectedRevision: snapshot.lastSeq }, crypto.randomUUID()); await poll(); }) }, '请求取消') : null) };
        }) }) : h(Antd.Empty, { image: Antd.Empty.PRESENTED_IMAGE_SIMPLE, description: '暂无已关联的安装或打包任务' })));
  }
  const view = mountReactView(root, h(View));
  const refreshModules = async (): Promise<void> => {
    const current = await context.request<GameModuleList>('modules.list', {});
    if (!active()) return;
    const previous = JSON.stringify(descriptors.find(item => item.gameId === gameId));
    descriptors = current.modules.flatMap(module => module.enabled && module.game?.descriptor ? [module.game.descriptor] : []);
    gameId = descriptors.some(item => item.gameId === gameId) ? gameId : descriptors[0]?.gameId ?? '';
    if (previous !== JSON.stringify(descriptors.find(item => item.gameId === gameId)) || !profiles.length) await loadGame();
    update();
  };
  const subscription = context.onInventoryChanged(refreshModules), timer = context.interval(poll, 1000);
  run(async () => { await refreshModules(); await poll(); });
  return { dispose() { persist(); alive = false; ++selectionGeneration; subscription.dispose(); timer.dispose(); view.dispose(); } };
}
