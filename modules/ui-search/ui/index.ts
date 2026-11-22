import type { ReactNode } from 'react';
import type { GroupSummary, ModRef, ProfileSummary, SourceSearchItem } from '@mintcat/sdk-ts';
import {
  copyTextToClipboard,
  getSharedRuntime,
  getUiLanguage,
  getSourceAuthGuide,
  getSourceDisplayName,
  isRemoteSourceId,
  mountReactView,
  navigateUi,
  openExternalUrl,
  presentUiError,
  publishUiSelection,
  tr,
  validateSourceCredentialInput,
} from '@mintcat/sdk-ui';
import type {
  Disposable,
  GameModuleList,
  GameUiDescriptor,
  RemoteSourceId,
  UiContext,
  UiErrorContext,
  UiErrorPresentation,
} from '@mintcat/sdk-ui';

export async function mountSearch(root: HTMLElement, context: UiContext): Promise<Disposable> {
  const SEARCH_CACHE_TTL_MS = 60_000;
  const { React, Antd, Icons, createPortal } = getSharedRuntime();
  const h = React.createElement;
  const saved = context.state('search', 1, { gameId: '', profileId: '', sourceId: '', query: '', sort: 'default' });
  let alive = true;
  const active = (): boolean => alive && !context.signal.aborted;
  let gameId = saved.gameId, profileId = saved.profileId, sourceId = saved.sourceId, query = saved.query, sort = saved.sort || 'default';
  let includeDependencies = true;
  let descriptors: GameUiDescriptor[] = [];
  let hasLoadedModules = false;
  let availableSources = new Set<string>();
  let sourceCredentials = new Map<string, boolean>();
  let profiles: ProfileSummary[] = [], items: SourceSearchItem[] = [];
  let targetGroups: GroupSummary[] = [], targetGroupsLoading = false, targetGroupId: string | undefined;
  let groupsGeneration = 0;
  let cursor: string | undefined, requestGeneration = 0, bindingGeneration = 0;
  let loading = false, bindingsLoading = false, adding = false, searched = false, error: UiErrorPresentation | undefined, errorContext: UiErrorContext = 'search';
  let profileHint = '', status = '', filter = '', selected: SourceSearchItem | undefined;
  let hasSubmittedAdd = false;
  let copiedUrl = '', copyResetTimer: ReturnType<typeof setTimeout> | undefined;
  let credentialInput = '', credentialError = '', credentialSaving = false, credentialClearing = false;
  let credentialModalSource: RemoteSourceId | undefined;
  let redraw = (): void => {};
  const update = (): void => { if (active()) redraw(); };
  const clearCopyTimer = (): void => {
    if (copyResetTimer !== undefined) { clearTimeout(copyResetTimer); copyResetTimer = undefined; }
    copiedUrl = '';
  };
  const resetCredentialDraft = (): void => {
    credentialInput = '';
    credentialError = '';
  };
  const openPortalUrl = (url: string): void => {
    void openExternalUrl(url).catch(() => {
      if (active()) context.notify(tr('无法自动打开浏览器，请点击「复制地址」后手动访问。', 'Could not open the browser. Click Copy URL to open it manually.'));
    });
  };
  const copyPortalUrl = (url: string): void => {
    void copyTextToClipboard(url).then(copied => {
      if (!active()) return;
      if (!copied) { context.notify(tr('复制失败，请手动选中地址复制。', 'Could not copy the URL. Select and copy it manually.')); return; }
      clearCopyTimer();
      copiedUrl = url;
      context.notify(tr('已复制凭据获取地址。', 'Credential portal URL copied.'));
      update();
      copyResetTimer = setTimeout(() => { copyResetTimer = undefined; copiedUrl = ''; update(); }, 2_000);
    });
  };
  const persist = (): void => context.saveState('search', 1, { gameId, profileId, sourceId, query, sort });
  const selection = (): void => {
    const descriptor = descriptors.find(item => item.gameId === gameId);
    if (descriptor) publishUiSelection({ gameId, gameName: descriptor.displayName, profileId: profileId || undefined });
  };
  const clearResults = (): void => { ++requestGeneration; clearCopyTimer(); resetCredentialDraft(); loading = false; searched = false; cursor = undefined; items = []; selected = undefined; error = undefined; status = ''; update(); };
  const fail = (cause: unknown, operation: UiErrorContext): void => {
    error = presentUiError(cause, operation); errorContext = operation; status = ''; update();
  };
  const run = (action: () => Promise<void>, operation: UiErrorContext = 'search'): void => context.guard(async () => {
    const generation = bindingGeneration, searchGeneration = requestGeneration, currentGame = gameId;
    try { await action(); } catch (cause) {
      if (active() && generation === bindingGeneration && searchGeneration === requestGeneration && currentGame === gameId) fail(cause, operation);
    }
  })();
  const saveCredential = async (targetSourceId: RemoteSourceId, autoReloadSearch: boolean): Promise<void> => {
    if (credentialSaving || credentialClearing) return;
    const validated = validateSourceCredentialInput(credentialInput);
    if (!validated.ok) {
      credentialError = validated.message;
      update();
      return;
    }
    credentialSaving = true;
    credentialError = '';
    update();
    try {
      const result = await context.call('sources.credentials.set', { sourceId: targetSourceId, secret: validated.value }, crypto.randomUUID());
      if (!active()) { resetCredentialDraft(); return; }
      sourceCredentials.set(result.sourceId, result.credentialConfigured);
      resetCredentialDraft();
      credentialModalSource = undefined;
      context.invalidate('');
      context.notify(tr(`已保存 ${getSourceDisplayName(targetSourceId)} 凭据到本机系统钥匙串。`, `Saved the ${getSourceDisplayName(targetSourceId)} credential to your system keychain.`));
      if (errorContext === 'add' && error?.kind === 'source-unauthorized') error = undefined;
      credentialSaving = false;
      update();
      if (autoReloadSearch && sourceId === targetSourceId && gameId) {
        await search();
      }
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause, 'search');
      credentialError = tr(`${presented.title}：${presented.description}`, `${presented.title}: ${presented.description}`);
      credentialSaving = false;
      update();
    }
  };
  const clearCredential = async (targetSourceId: RemoteSourceId): Promise<void> => {
    if (credentialSaving || credentialClearing) return;
    credentialClearing = true;
    credentialError = '';
    update();
    try {
      const result = await context.call('sources.credentials.clear', { sourceId: targetSourceId }, crypto.randomUUID());
      if (!active()) return;
      sourceCredentials.set(result.sourceId, result.credentialConfigured);
      resetCredentialDraft();
      context.invalidate('');
      context.notify(tr(`已清除 ${getSourceDisplayName(targetSourceId)} 已保存的凭据。`, `Cleared the saved ${getSourceDisplayName(targetSourceId)} credential.`));
      credentialClearing = false;
      update();
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause, 'search');
      credentialError = tr(`${presented.title}：${presented.description}`, `${presented.title}: ${presented.description}`);
      credentialClearing = false;
      update();
    }
  };
  const openCredentialModal = (targetSourceId: RemoteSourceId): void => {
    resetCredentialDraft();
    credentialModalSource = targetSourceId;
    update();
  };
  const closeCredentialModal = (): void => {
    if (credentialSaving || credentialClearing) return;
    resetCredentialDraft();
    credentialModalSource = undefined;
    update();
  };
  const loadBindings = async (): Promise<void> => {
    const generation = ++bindingGeneration; clearResults(); profiles = []; profileHint = '';
    const descriptor = descriptors.find(item => item.gameId === gameId);
    const currentGame = gameId, desiredProfile = profileId || saved.profileId, desiredSource = sourceId || saved.sourceId;
    profileId = ''; sourceId = descriptor?.sourceBindings.find(item => item.sourceId === desiredSource)?.sourceId ?? descriptor?.sourceBindings[0]?.sourceId ?? '';
    bindingsLoading = Boolean(descriptor); status = descriptor ? '' : tr('尚未安装游戏支持插件；备份和 Agent 仍可使用。', 'No game support plugins are installed. Backups and Agent are still available.');
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
        ? tr('可浏览模组；请先在模组管理中创建配置，才能添加模组。', 'You can browse mods now. To add them, first create a profile in the Mod Library.')
        : tr('可浏览模组；请先在游戏设置中注册真实安装目录，再创建配置以添加模组。', 'You can browse mods now. To add them, register your game installation folder in Game Settings, then create a profile.');
      persist(); selection(); update();
      if (gameId && sourceId) {
        bindingsLoading = false;
        update();
        await search();
      }
    } catch (cause) {
      if (active() && generation === bindingGeneration && currentGame === gameId) fail(cause, 'bindings');
    } finally {
      if (active() && generation === bindingGeneration && currentGame === gameId) { bindingsLoading = false; update(); }
    }
  };
  const parseSort = (value: string): { sortField?: 'updated' | 'downloads' | 'subscribers' | 'rating' | 'name'; sortDirection?: 'ascending' | 'descending' } => {
    if (!value || value === 'default') return {};
    const [field, direction] = value.split(':');
    return {
      sortField: field as 'updated' | 'downloads' | 'subscribers' | 'rating' | 'name',
      sortDirection: direction as 'ascending' | 'descending',
    };
  };
  const formatCompactNumber = (value: number): string => {
    const language = getUiLanguage();
    if (language === 'en-US') return value.toLocaleString(language, { notation: 'compact' });
    if (value >= 100_000) return `${(value / 10_000).toFixed(1)}万`;
    if (value >= 10_000) return `${(value / 10_000).toFixed(2)}万`;
    if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
    return String(value);
  };
  const formatUnixDate = (seconds: number): string => {
    const ms = seconds > 10_000_000_000 ? seconds : seconds * 1000;
    const date = new Date(ms);
    if (Number.isNaN(date.getTime())) return '';
    const language = getUiLanguage();
    return language === 'zh-CN'
      ? `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
      : date.toLocaleDateString(language);
  };
  const search = async (next = false): Promise<void> => {
    if (!gameId || !sourceId) throw new Error(tr('所选游戏没有已配置的在线来源绑定。', 'No online source is configured for the selected game.'));
    if (loading || (next && !cursor)) return;
    const generation = ++requestGeneration;
    const binding = descriptors.find(item => item.gameId === gameId)?.sourceBindings.find(item => item.sourceId === sourceId);
    const sortParams = parseSort(sort);
    const params = { sourceId, gameId, query, pageSize: 50, ...sortParams, ...(binding?.remoteGameKey ? { remoteGameKey: binding.remoteGameKey } : {}), ...(next && cursor ? { cursor } : {}) };
    loading = true; searched = true; error = undefined; status = ''; if (!next) items = []; update(); persist();
    try {
      const response = await context.query(JSON.stringify(params), () => context.call('sources.search', params), SEARCH_CACHE_TTL_MS);
      if (!active() || generation !== requestGeneration) return;
      items = next ? [...items, ...response.items] : response.items; cursor = response.nextCursor;
      status = cursor ? tr('还有更多来源结果。', 'More results are available.') : tr('已显示全部来源结果。', 'All results are shown.');
    } catch (cause) {
      if (active() && generation === requestGeneration) { cursor = undefined; fail(cause, 'search'); }
    } finally { if (active() && generation === requestGeneration) { loading = false; update(); } }
  };
  const ROOT_GROUP_VALUE = '__root__';
  const loadTargetGroups = async (forProfileId: string): Promise<void> => {
    const gen = ++groupsGeneration;
    targetGroupId = undefined;
    if (!forProfileId) {
      targetGroups = [];
      targetGroupsLoading = false;
      update();
      return;
    }
    targetGroupsLoading = true;
    update();
    try {
      const result = await context.call('groups.list', { profileId: forProfileId });
      if (!active() || gen !== groupsGeneration) return;
      targetGroups = result.items;
    } catch {
      if (!active() || gen !== groupsGeneration) return;
      targetGroups = [];
    } finally {
      if (active() && gen === groupsGeneration) {
        targetGroupsLoading = false;
        update();
      }
    }
  };
  interface GroupTreeNode {
    value: string;
    title: ReactNode;
    children?: GroupTreeNode[];
  }
  const buildGroupTree = (): GroupTreeNode[] => {
    const visited = new Set<string>();
    const walk = (parentId?: string): GroupTreeNode[] => targetGroups
      .filter(group => group.parentGroupId === parentId)
      .sort((a, b) => a.sortOrder - b.sortOrder)
      .flatMap(group => {
        if (visited.has(group.groupId)) return [];
        visited.add(group.groupId);
        const children = walk(group.groupId);
        return [{
          value: group.groupId,
          title: h('span', { className: 'mintcat-search-tree-opt' }, h(Icons.FolderOutlined), ' ', group.name),
          ...(children.length ? { children } : {}),
        }];
      });
    return [{
      value: ROOT_GROUP_VALUE,
      title: h('span', { className: 'mintcat-search-tree-opt' }, h(Icons.HomeOutlined), tr(' 根目录（未分组）', ' Root (ungrouped)')),
      children: walk(undefined),
    }];
  };
  const add = async (reference: ModRef): Promise<void> => {
    if (adding) return;
    const target = profiles.find(item => item.profileId === profileId);
    if (!target) throw new Error(tr('请选择目标配置。', 'Select a target profile.'));
    adding = true; update();
    const generation = bindingGeneration, searchGeneration = requestGeneration;
    error = undefined;
    try {
      const content = await context.call('profiles.contentState', { profileId: target.profileId });
      if (!active()) return;
      if (!content.writable) throw new Error(tr('配置正在处理中，请稍后再试。', 'The profile is busy. Try again later.'));
      const job = await context.call('mods.add', {
        profileId: target.profileId,
        reference,
        expectedContentRevision: content.contentRevision,
        includeDependencies,
        ...(targetGroupId ? { groupId: targetGroupId } : {}),
      }, crypto.randomUUID());
      // Accepted jobs are still returned if the user navigates away while submitting.
      context.onlineModJobs.track(job.jobId, target.profileId);
      if (!active()) return;
      selected = undefined;
      hasSubmittedAdd = true;
      status = tr(`已提交模组添加任务（${job.jobId.slice(0, 8)}），可在下载管理查看进度。`, `Mod add job (${job.jobId.slice(0, 8)}) submitted. Check its progress in Downloads.`);
      context.notify(status, tr('已提交模组添加任务', 'Mod add job submitted'));
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
    const authGuide = getSourceAuthGuide(sourceId);
    const activeRemoteSource = isRemoteSourceId(sourceId) ? sourceId : undefined;
    const hasSavedCredential = Boolean(activeRemoteSource && sourceCredentials.get(activeRemoteSource));
    const isAuthFailure = error?.kind === 'source-unauthorized';
    const showAuthGuideCard = Boolean(error && isAuthFailure && errorContext === 'search' && !items.length);
    const activeSourceLabel = getSourceDisplayName(sourceId);
    const credentialBusy = credentialSaving || credentialClearing;
    const renderCredentialEntry = (targetSource: RemoteSourceId, autoReload: boolean, submitLabel: string) => {
      const targetGuide = getSourceAuthGuide(targetSource);
      return h('div', { className: 'mintcat-auth-form' },
        h('div', { className: 'mintcat-auth-form-head' },
          h('label', { className: 'mintcat-auth-form-label', htmlFor: `mintcat-cred-input-${targetSource}` }, tr(`直接填入 ${targetGuide?.credentialLabel ?? '来源凭据'}`, `Enter your ${targetGuide?.credentialLabel ?? 'source credential'}`)),
          h('span', { className: 'mintcat-auth-form-hint' }, tr('保存后立即生效，无需编辑任何配置文件', 'Takes effect immediately after saving. No configuration changes needed.'))),
        h('div', { className: 'mintcat-auth-form-row' },
          h(Antd.Input.Password, {
            id: `mintcat-cred-input-${targetSource}`,
            className: 'mintcat-auth-input',
            value: credentialInput,
            placeholder: targetGuide?.placeholder ?? tr('粘贴官方生成的访问令牌或 API Key', 'Paste an access token or API key from the official source'),
            'aria-label': tr('来源凭据输入框', 'Source credential'),
            autoComplete: 'off',
            spellCheck: false,
            allowClear: true,
            disabled: credentialBusy,
            onChange: (event: { target: { value: string } }) => {
              credentialInput = event.target.value;
              if (credentialError) credentialError = '';
              update();
            },
            onPressEnter: () => {
              if (!credentialBusy && credentialInput.trim()) {
                context.guard(() => saveCredential(targetSource, autoReload))();
              }
            },
          }),
          h(Antd.Button, {
            type: 'primary',
            className: 'glass-primary-action mintcat-auth-save-btn',
            icon: h(Icons.SafetyCertificateOutlined),
            loading: credentialSaving,
            disabled: credentialBusy || !credentialInput.trim(),
            onClick: () => context.guard(() => saveCredential(targetSource, autoReload))(),
          }, submitLabel)),
        credentialError ? h('div', { className: 'mintcat-auth-form-error', role: 'alert' }, credentialError) : null);
    };
    const errorAlert = error ? h(Antd.Alert, {
      type: 'error', showIcon: true, className: 'mintcat-search-alert', title: errorContext === 'bindings' ? error.title : tr(`${activeSourceLabel}：${error.title}`, `${activeSourceLabel}: ${error.title}`),
      description: h('div', null, h('p', null, error.description),
        isAuthFailure && authGuide ? h('div', { className: 'mintcat-search-alert-guide' },
          h('span', null, tr(`获取 ${authGuide.credentialLabel}：`, `Get a ${authGuide.credentialLabel}:`)),
          h('a', {
            href: authGuide.portalUrl,
            target: '_blank',
            rel: 'noopener noreferrer',
            className: 'mintcat-search-alert-link',
            onClick: (event: { preventDefault(): void }) => { event.preventDefault(); openPortalUrl(authGuide.portalUrl); },
          }, authGuide.portalUrl, ' ', h(Icons.ExportOutlined, { className: 'mintcat-search-ext-icon' })),
          h(Antd.Button, {
            size: 'small',
            type: 'link',
            icon: h(copiedUrl === authGuide.portalUrl ? Icons.CheckOutlined : Icons.CopyOutlined),
            onClick: () => copyPortalUrl(authGuide.portalUrl),
          }, copiedUrl === authGuide.portalUrl ? tr('已复制', 'Copied') : tr('复制地址', 'Copy URL'))) : null,
        isAuthFailure && activeRemoteSource && errorContext === 'add'
          ? renderCredentialEntry(activeRemoteSource, false, tr('保存凭据', 'Save credential'))
          : null,
        error.diagnostics.length ? h('details', null, h('summary', null, tr('技术详情', 'Technical details')), h('pre', null, error.diagnostics.join('\n'))) : null),
      action: h('div', { className: 'mintcat-search-alert-actions' },
        error.retryable && errorContext !== 'add' ? h(Antd.Button, { size: 'small', disabled: loading || bindingsLoading, onClick: () => {
          context.invalidate(''); if (errorContext === 'bindings') run(() => refreshModules(true), 'bindings'); else run(() => search());
        } }, errorContext === 'bindings' ? tr('重新读取配置', 'Reload configuration') : tr('重试', 'Retry')) : null,
        errorContext === 'search' ? alternatives.map(binding => h(Antd.Button, { key: binding.sourceId, size: 'small', onClick: () => switchSource(binding.sourceId) }, tr(`切换到 ${getSourceDisplayName(binding.sourceId)}`, `Switch to ${getSourceDisplayName(binding.sourceId)}`))) : null),
    }) : null;
    const authGuideCard = showAuthGuideCard && error ? h('div', { className: 'mintcat-auth-guide' },
      h('section', { className: 'mintcat-auth-guide-card', role: 'alert', 'aria-labelledby': 'mintcat-auth-guide-title' },
        h('div', { className: 'mintcat-auth-guide-head' },
          h('div', { className: 'mintcat-auth-guide-orb', 'aria-hidden': true }, h(Icons.KeyOutlined)),
          h('div', { className: 'mintcat-auth-guide-heading' },
            h('div', { className: 'mintcat-auth-guide-badges' },
              h('span', { className: `mintcat-search-source-tag mintcat-search-source-tag--${sourceId}` }, activeSourceLabel),
              authGuide ? h('span', { className: 'mintcat-auth-cred-chip' }, authGuide.credentialLabel) : null,
              hasSavedCredential ? h('span', { className: 'mintcat-auth-state-chip' }, tr('已保存凭据 · 可能已失效', 'Saved credential may be invalid')) : null),
            h('h3', { id: 'mintcat-auth-guide-title', className: 'mintcat-auth-guide-title' }, tr(`${activeSourceLabel}：${error.title}`, `${activeSourceLabel}: ${error.title}`)),
            h('p', { className: 'mintcat-auth-guide-desc' }, hasSavedCredential
              ? tr('当前保存的凭据无效、已过期或权限不足。请在下方重新粘贴有效凭据替换，或先清除旧凭据。', 'The saved credential is invalid, expired, or missing permissions. Paste a valid replacement below or clear the saved credential.')
              : error.description))),
        authGuide ? h('div', { className: 'mintcat-auth-portal' },
          h('div', { className: 'mintcat-auth-portal-info' },
            h('span', { className: 'mintcat-auth-portal-label' }, tr(`官方获取入口 · ${authGuide.portalName}`, `Official credential portal · ${authGuide.portalName}`)),
            h('a', {
              href: authGuide.portalUrl,
              target: '_blank',
              rel: 'noopener noreferrer',
              className: 'mintcat-auth-portal-url',
              onClick: (event: { preventDefault(): void }) => { event.preventDefault(); openPortalUrl(authGuide.portalUrl); },
            }, authGuide.portalUrl, ' ', h(Icons.ExportOutlined, { className: 'mintcat-search-ext-icon' }))),
          h('div', { className: 'mintcat-auth-portal-actions' },
            h(Antd.Button, {
              className: 'mintcat-auth-portal-btn',
              icon: h(Icons.ExportOutlined),
              onClick: () => openPortalUrl(authGuide.portalUrl),
            }, authGuide.actionLabel),
            h(Antd.Button, {
              icon: h(copiedUrl === authGuide.portalUrl ? Icons.CheckOutlined : Icons.CopyOutlined),
              onClick: () => copyPortalUrl(authGuide.portalUrl),
            }, copiedUrl === authGuide.portalUrl ? tr('已复制', 'Copied') : tr('复制地址', 'Copy URL')))) : null,
        activeRemoteSource ? renderCredentialEntry(activeRemoteSource, true, tr('保存并重新加载', 'Save and reload')) : null,
        authGuide ? h('div', { className: 'mintcat-auth-steps' },
          h('div', { className: 'mintcat-auth-steps-title' }, tr('获取步骤', 'How to get a credential')),
          h('ol', { className: 'mintcat-auth-step-list' }, ...authGuide.steps.map((step, index) => h('li', { key: index, className: 'mintcat-auth-step-item' },
            h('span', { className: 'mintcat-auth-step-num', 'aria-hidden': true }, String(index + 1)),
            h('span', { className: 'mintcat-auth-step-text' }, step))))) : null,
        h('div', { className: 'mintcat-auth-note' },
          h(Icons.LockOutlined, { className: 'mintcat-auth-note-icon', 'aria-hidden': true }),
          h('span', null, tr('凭据将直接加密保存在本机系统钥匙串中，不会以明文写入配置文件、数据库或日志。', 'Credentials are encrypted in your system keychain and never written in plain text to configuration files, databases, or logs.'))),
        h('div', { className: 'mintcat-auth-actions' },
          h(Antd.Button, {
            icon: h(Icons.ReloadOutlined, { spin: loading }),
            disabled: loading || bindingsLoading || credentialBusy,
            onClick: () => { context.invalidate(''); run(() => search()); },
          }, tr('重新检测', 'Check again')),
          hasSavedCredential && activeRemoteSource ? h(Antd.Button, {
            danger: true,
            icon: h(Icons.DeleteOutlined),
            loading: credentialClearing,
            disabled: credentialBusy,
            onClick: () => context.guard(() => clearCredential(activeRemoteSource))(),
          }, tr('清除已保存凭据', 'Clear saved credential')) : null,
          ...alternatives.map(binding => h(Antd.Button, {
            key: binding.sourceId,
            disabled: credentialBusy,
            onClick: () => switchSource(binding.sourceId),
          }, tr(`切换到 ${getSourceDisplayName(binding.sourceId)}`, `Switch to ${getSourceDisplayName(binding.sourceId)}`))),
          h(Antd.Button, { icon: h(Icons.ImportOutlined), onClick: () => navigateUi('local') }, tr('前往本地导入', 'Go to Local Import'))),
        error.diagnostics.length ? h('details', { className: 'mintcat-auth-details' },
          h('summary', null, tr('技术详情', 'Technical details')),
          h('pre', null, error.diagnostics.join('\n'))) : null)) : null;
    const modalGuide = credentialModalSource ? getSourceAuthGuide(credentialModalSource) : undefined;
    const modalHasSavedCredential = Boolean(credentialModalSource && sourceCredentials.get(credentialModalSource));
    const toolbar = h('div', { className: `mintcat-search-header${context.toolbar ? ' mintcat-search-header--portal' : ''}` },
        h('div', { className: 'mintcat-search-game-control' },
          h('span', { className: 'mintcat-search-game-badge', 'aria-hidden': true }, h(Icons.CompassOutlined)),
          h(Antd.Select<string>, { variant: 'borderless', value: gameId || undefined, placeholder: tr('选择游戏', 'Select a game'), 'aria-label': tr('游戏', 'Game'), popupMatchSelectWidth: false, disabled: !descriptors.length, options: descriptors.map(item => ({ value: item.gameId, label: item.displayName })), className: 'mintcat-search-game-select', onChange: (value: string) => { gameId = value; run(loadBindings); } })),
        h('label', { className: `mintcat-search-box${!sourceId ? ' mintcat-search-box--disabled' : ''}` },
          h(Icons.SearchOutlined),
          h('input', {
            type: 'search',
            value: query,
            placeholder: tr('搜索在线模组名称或关键词…', 'Search online mods by name or keyword…'),
            'aria-label': tr('搜索在线模组名称或关键词', 'Search online mods by name or keyword'),
            disabled: !sourceId,
            onChange: (event: { target: { value: string } }) => { query = event.target.value; clearResults(); persist(); },
            onKeyDown: (event: { key: string; preventDefault(): void }) => {
              if (event.key === 'Enter' && sourceId && !loading) { event.preventDefault(); run(() => search()); }
            },
          }),
          query ? h('button', { type: 'button', className: 'mintcat-search-box-clear', 'aria-label': tr('清除关键词', 'Clear search'), onClick: () => { query = ''; clearResults(); persist(); run(() => search()); } }, '×') : h('kbd', { 'aria-hidden': true }, '↵')),
        h('div', { className: `mintcat-search-profile-control${bindingsLoading || !profiles.length ? ' mintcat-search-profile-control--disabled' : ''}` },
          h('span', { className: 'mintcat-search-profile-label' }, h(Icons.ProfileOutlined), tr('配置', 'Profile')),
          h(Antd.Select<string>, { variant: 'borderless', value: profileId || undefined, placeholder: tr('选择目标配置', 'Select a target profile'), 'aria-label': tr('目标配置', 'Target profile'), popupMatchSelectWidth: false, loading: bindingsLoading, disabled: bindingsLoading || !profiles.length, options: profiles.map(item => ({ value: item.profileId, label: item.displayName })), className: 'mintcat-search-profile-select', onChange: (value: string) => { profileId = value; persist(); selection(); update(); } })),
        h('div', { className: 'mintcat-search-toolbar-group' },
          h(Antd.Button, { type: 'text', icon: h(Icons.HomeOutlined), title: tr('浏览首页', 'Browse home'), 'aria-label': tr('浏览首页', 'Browse home'), disabled: loading || !sourceId, onClick: () => { query = ''; clearResults(); run(() => search()); } }),
          h(Antd.Button, { type: 'text', icon: h(Icons.ReloadOutlined, { spin: loading }), title: tr('刷新搜索', 'Refresh search'), 'aria-label': tr('刷新搜索', 'Refresh search'), disabled: loading || !sourceId, onClick: () => { context.invalidate(''); run(() => search()); } }),
          h(Antd.Button, { type: 'text', icon: h(Icons.SyncOutlined), title: tr('重新读取游戏与来源', 'Reload games and sources'), 'aria-label': tr('重新读取游戏与来源', 'Reload games and sources'), disabled: loading || bindingsLoading, onClick: () => run(() => refreshModules(true), 'bindings') })));
    const sourceBindings = descriptor?.sourceBindings ?? [];
    const showFooterStatus = bindingsLoading || Boolean(sourceId && status);
    return h('div', { className: `mintcat-search-page${context.toolbar ? ' mintcat-search-page--portal' : ''}` },
      context.toolbar ? createPortal(toolbar, context.toolbar) : toolbar,
      sourceBindings.length ? h('div', { className: 'mintcat-search-subbar' },
        h(Antd.Tabs, { activeKey: sourceId, size: 'small', className: 'mintcat-search-tabs', items: sourceBindings.map(binding => ({ key: binding.sourceId, label: binding.sourceId })), onChange: switchSource }),
        h('div', { className: 'mintcat-search-filters' },
          h(Antd.Select<string>, {
            size: 'small',
            value: sort,
            'aria-label': tr('搜索排序方式', 'Search sort order'),
            popupMatchSelectWidth: false,
            disabled: loading || !sourceId,
            style: { minWidth: 112 },
            options: [
              { value: 'default', label: tr('默认排序', 'Default order') },
              { value: 'updated:descending', label: tr('最新更新', 'Recently updated') },
              { value: 'downloads:descending', label: tr('下载最多', 'Most downloaded') },
              { value: 'subscribers:descending', label: tr('订阅最多', 'Most subscribed') },
              { value: 'rating:descending', label: tr('评分最高', 'Highest rated') },
              { value: 'name:ascending', label: tr('名称 A → Z', 'Name A → Z') },
              { value: 'name:descending', label: tr('名称 Z → A', 'Name Z → A') },
            ],
            onChange: (value: string) => { sort = value; clearResults(); persist(); run(() => search()); },
          }),
          items.length || filter ? h(React.Fragment, null,
            h(Antd.Input, { value: filter, allowClear: true, size: 'small', prefix: h(Icons.FilterOutlined), placeholder: tr('筛选已加载结果…', 'Filter loaded results…'), 'aria-label': tr('筛选已加载结果', 'Filter loaded results'), onChange: (event: { target: { value: string } }) => { filter = event.target.value; update(); }, className: 'mintcat-search-filter-input' }),
            h('span', { className: 'mintcat-search-count-pill' }, tr(`${visible.length} / ${items.length} 个结果`, `${visible.length} of ${items.length} results`))) : null,
          activeRemoteSource ? h(Antd.Button, {
            size: 'small',
            className: `mintcat-search-cred-btn${hasSavedCredential ? ' mintcat-search-cred-btn--set' : ''}`,
            icon: h(Icons.KeyOutlined),
            onClick: () => openCredentialModal(activeRemoteSource),
          }, hasSavedCredential ? tr('已配置凭据', 'Credential configured') : tr('配置凭据', 'Configure credential')) : null)) : null,
      errorContext !== 'add' && !showAuthGuideCard ? errorAlert : null,
      profileHint ? h(Antd.Alert, { type: 'info', showIcon: true, className: 'mintcat-search-alert', title: profileHint }) : null,
      authGuideCard,
      !sourceId && !bindingsLoading ? h('div', { className: 'mc-empty-hero' },
        h('div', { className: 'mc-empty-hero-card' },
          h('div', { className: 'mc-empty-hero-orb', 'aria-hidden': true }, h(Icons.CompassOutlined)),
          h('h3', { className: 'mc-empty-hero-title' }, descriptors.length ? tr('当前游戏暂无可用在线来源', 'No online sources are available for this game') : tr('尚未安装游戏支持插件', 'No game support plugins installed')),
          h('p', { className: 'mc-empty-hero-desc' }, descriptors.length
            ? tr('所选游戏没有已配置的在线来源。请配置支持此游戏的来源；本地文件可从本地导入添加。', 'No online source is configured for the selected game. Configure a compatible source, or use Local Import to add files from your computer.')
            : tr('尚未安装游戏支持插件。安装游戏支持后即可浏览在线模组；备份和 Agent 仍可使用。', 'Install a game support plugin to browse online mods. Backups and Agent are still available.')),
          h('div', { className: 'mc-empty-hero-actions' },
            descriptors.length
              ? h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.ImportOutlined), onClick: () => navigateUi('local') }, tr('前往本地导入', 'Go to Local Import'))
              : h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.CloudOutlined), onClick: () => navigateUi('cloud') }, tr('备份与恢复', 'Backup and Restore'))),
          !descriptors.length ? h('div', { className: 'mc-empty-bento' },
            h('div', { className: 'mc-empty-bento-item' },
              h('div', { className: 'mc-empty-bento-head' }, h(Icons.SafetyCertificateOutlined), tr('签名游戏扩展', 'Signed game extensions')),
              h('span', { className: 'mc-empty-bento-text' }, tr('接入已签名游戏包后自动解锁 mod.io、ModCat 与 Nexus 在线检索。', 'Add a signed game package to enable online search on mod.io, ModCat, and Nexus.'))),
            h('button', { type: 'button', className: 'mc-empty-bento-item', onClick: () => navigateUi('cloud') },
              h('div', { className: 'mc-empty-bento-head' }, h(Icons.CloudUploadOutlined), tr('备份与迁移', 'Backup and Migration')),
              h('span', { className: 'mc-empty-bento-text' }, tr('随时导出便携归档、管理云快照或导入旧版配置数据。', 'Export portable archives, manage cloud snapshots, or import legacy profile data.')))) : null)) : null,
      loading && !items.length ? h(Antd.Skeleton, { active: true, paragraph: { rows: 3 } }) : null,
      !loading && !error && sourceId && !visible.length ? h('div', { className: 'mc-empty-hero' },
        h('div', { className: 'mc-empty-hero-card' },
          h('div', { className: 'mc-empty-hero-orb', 'aria-hidden': true }, h(searched ? Icons.InboxOutlined : Icons.SearchOutlined)),
          h('h3', { className: 'mc-empty-hero-title' }, searched ? tr('没有找到模组', 'No mods found') : tr('探索在线模组库', 'Explore online mods')),
          h('p', { className: 'mc-empty-hero-desc' }, searched
            ? tr('没有找到匹配的模组，请尝试更换关键词或切换其他在线来源。', 'No mods matched your search. Try a different keyword or switch to another source.')
            : tr('在顶部输入关键词回车搜索，或直接浏览当前来源的热门模组。', 'Type a keyword and press Enter to search, or browse popular mods from this source.')),
          !searched ? h('div', { className: 'mc-empty-hero-actions' },
            h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.HomeOutlined), onClick: () => { query = ''; clearResults(); run(() => search()); } }, tr('浏览热门模组', 'Browse popular mods'))) : null)) : null,
      visible.length ? h('div', { className: 'mintcat-search-results' }, ...visible.map((item, index) => h('article', { key: `${JSON.stringify(item.reference)}:${index}`, className: 'mintcat-search-card' },
        h('div', { className: 'mintcat-search-media' },
          item.logoUrl && /^https?:\/\//i.test(item.logoUrl)
            ? h('img', { className: 'mintcat-search-thumbnail', src: item.logoUrl, alt: item.displayName })
            : h('div', { className: 'mintcat-search-cover-placeholder', 'aria-hidden': true }, h(Icons.AppstoreOutlined))),
        h('div', { className: 'mintcat-search-info' },
          h('div', { className: 'mintcat-search-card-title' },
            item.profileUrl && /^https?:\/\//i.test(item.profileUrl)
              ? h(Antd.Typography.Link, {
                strong: true,
                href: item.profileUrl,
                target: '_blank',
                rel: 'noopener noreferrer',
                onClick: item.profileUrl.startsWith('https://')
                  ? (event: { preventDefault(): void }) => { event.preventDefault(); openPortalUrl(item.profileUrl!); }
                  : undefined,
              }, item.displayName, ' ', h(Icons.ExportOutlined, { className: 'mintcat-search-ext-icon' }))
              : h(Antd.Typography.Text, { strong: true }, item.displayName)),
          h('div', { className: 'mintcat-search-summary' }, item.summary || tr('此来源未提供模组简介。', 'This source did not provide a description.')),
          h('div', { className: 'mintcat-search-meta-tags' },
            h('span', { className: `mintcat-search-source-tag mintcat-search-source-tag--${item.reference.sourceId}` }, getSourceDisplayName(item.reference.sourceId)),
            item.author ? h('span', { className: 'mintcat-search-meta-chip' }, tr(`作者 ${item.author}`, `By ${item.author}`)) : null,
            typeof item.downloads === 'number' ? h('span', { className: 'mintcat-search-meta-chip', title: tr(`总下载量 ${item.downloads}`, `Total downloads: ${item.downloads.toLocaleString(getUiLanguage())}`) }, h(Icons.DownloadOutlined), ' ', formatCompactNumber(item.downloads)) : null,
            typeof item.subscribers === 'number' ? h('span', { className: 'mintcat-search-meta-chip', title: tr(`订阅数 ${item.subscribers}`, `Subscribers: ${item.subscribers.toLocaleString(getUiLanguage())}`) }, h(Icons.StarOutlined), ' ', formatCompactNumber(item.subscribers)) : null,
            typeof item.rating === 'number' && item.rating > 0 ? h('span', { className: 'mintcat-search-meta-chip', title: tr(`评分 ${item.rating}`, `Rating: ${item.rating}`) }, `${tr('评分', 'Rating')} ${item.rating <= 1 ? `${Math.round(item.rating * 100)}%` : item.rating.toFixed(1)}`) : null,
            typeof item.updatedAt === 'number' && formatUnixDate(item.updatedAt) ? h('span', { className: 'mintcat-search-meta-chip' }, formatUnixDate(item.updatedAt)) : null,
            h('span', { className: 'mintcat-search-meta-chip' }, `#${item.reference.remoteModId}`))),
        h('div', { className: 'mintcat-search-card-actions' },
          h(Antd.Button, { size: 'small', className: 'mintcat-search-add-btn', icon: h(Icons.PlusOutlined), disabled: !profileId || adding, title: tr('添加到配置', 'Add to profile'), onClick: () => { error = undefined; selected = item; void loadTargetGroups(profileId); update(); } }, tr('添加', 'Add')))))) : null,
      cursor ? h(Antd.Button, { block: true, className: 'mintcat-search-more-btn', loading, onClick: () => run(() => search(true)) }, tr('加载更多结果', 'Load more results')) : null,
      showFooterStatus ? h('div', { className: 'mintcat-search-footer-status' },
        h(Antd.Typography.Text, { type: 'secondary', role: 'status' }, bindingsLoading ? tr('正在读取配置…', 'Loading configuration…') : status),
        hasSubmittedAdd && !bindingsLoading ? h(Antd.Button, { size: 'small', type: 'link', onClick: () => navigateUi('downloads') }, tr('查看下载管理', 'View Downloads')) : null) : null,
      h(Antd.Modal, { open: Boolean(selected), title: tr('添加模组到配置', 'Add mod to profile'), okText: tr('添加并下载', 'Add and download'), cancelText: tr('取消', 'Cancel'), confirmLoading: adding, okButtonProps: { disabled: !profileId || adding, className: 'glass-primary-action' }, onCancel: () => { if (!adding) { selected = undefined; if (errorContext === 'add') error = undefined; resetCredentialDraft(); update(); } }, onOk: () => { if (selected) run(() => add(selected!.reference), 'add'); } },
        h('div', { className: 'mintcat-add-modal-body' },
          h('div', { className: 'mintcat-add-modal-name' }, selected?.displayName),
          h(Antd.Typography.Text, { type: 'secondary' }, tr('添加后会尝试将模组文件下载到本地缓存，不会自动部署到游戏。', 'MintCat will try to download the mod files to the local cache without deploying them to the game.')),
          errorAlert,
          h('div', { className: 'mintcat-add-field' },
            h('span', { className: 'mintcat-add-field-label' }, tr('目标配置', 'Target profile')),
            h(Antd.Select<string>, {
              style: { width: '100%' },
              value: profileId || undefined,
              placeholder: tr('选择目标配置', 'Select a target profile'),
              'aria-label': tr('目标配置', 'Target profile'),
              options: profiles.map(item => ({ value: item.profileId, label: item.displayName })),
              onChange: (value: string) => { profileId = value; persist(); selection(); void loadTargetGroups(value); update(); },
            })),
          h('div', { className: 'mintcat-add-field' },
            h('span', { className: 'mintcat-add-field-label' }, tr('导入目录', 'Import folder')),
            h(Antd.TreeSelect<string>, {
              style: { width: '100%' },
              'aria-label': tr('导入目标目录', 'Import destination folder'),
              loading: targetGroupsLoading,
              value: targetGroupId ?? ROOT_GROUP_VALUE,
              treeData: buildGroupTree(),
              treeDefaultExpandAll: true,
              showSearch: true,
              treeNodeFilterProp: 'title',
              placeholder: tr('选择目标分组目录', 'Select a destination group'),
              onChange: (value: string) => { targetGroupId = !value || value === ROOT_GROUP_VALUE ? undefined : value; update(); },
            })),
          h(Antd.Checkbox, {
            checked: includeDependencies,
            onChange: (event: { target: { checked: boolean } }) => { includeDependencies = event.target.checked; update(); },
          }, tr('自动解析并添加所需前置依赖模组', 'Automatically find and add required mods')))),
      h(Antd.Modal, {
        open: Boolean(credentialModalSource && modalGuide),
        title: modalGuide
          ? modalHasSavedCredential ? tr(`更换 ${modalGuide.sourceName} 凭据`, `Replace ${modalGuide.sourceName} credential`) : tr(`配置 ${modalGuide.sourceName} 凭据`, `Configure ${modalGuide.sourceName} credential`)
          : tr('配置来源凭据', 'Configure source credential'),
        footer: null,
        destroyOnHidden: true,
        onCancel: closeCredentialModal,
      }, modalGuide && credentialModalSource ? h('div', { className: 'mintcat-cred-modal-body' },
        h('div', { className: 'mintcat-auth-portal mintcat-auth-portal--compact' },
          h('div', { className: 'mintcat-auth-portal-info' },
            h('span', { className: 'mintcat-auth-portal-label' }, tr(`官方获取入口 · ${modalGuide.portalName}`, `Official credential portal · ${modalGuide.portalName}`)),
            h('a', {
              href: modalGuide.portalUrl,
              target: '_blank',
              rel: 'noopener noreferrer',
              className: 'mintcat-auth-portal-url',
              onClick: (event: { preventDefault(): void }) => { event.preventDefault(); openPortalUrl(modalGuide.portalUrl); },
            }, modalGuide.portalUrl, ' ', h(Icons.ExportOutlined, { className: 'mintcat-search-ext-icon' }))),
          h('div', { className: 'mintcat-auth-portal-actions' },
            h(Antd.Button, {
              size: 'small',
              icon: h(Icons.ExportOutlined),
              onClick: () => openPortalUrl(modalGuide.portalUrl),
            }, modalGuide.actionLabel),
            h(Antd.Button, {
              size: 'small',
              icon: h(copiedUrl === modalGuide.portalUrl ? Icons.CheckOutlined : Icons.CopyOutlined),
              onClick: () => copyPortalUrl(modalGuide.portalUrl),
            }, copiedUrl === modalGuide.portalUrl ? tr('已复制', 'Copied') : tr('复制地址', 'Copy URL')))),
        renderCredentialEntry(credentialModalSource, Boolean(searched || error), tr('保存凭据', 'Save credential')),
        h('div', { className: 'mintcat-auth-note' },
          h(Icons.LockOutlined, { className: 'mintcat-auth-note-icon', 'aria-hidden': true }),
          h('span', null, tr('凭据将直接加密保存在本机系统钥匙串中，不会以明文写入配置文件、数据库或日志。', 'Credentials are encrypted in your system keychain and never written in plain text to configuration files, databases, or logs.'))),
        modalHasSavedCredential ? h('div', { className: 'mintcat-cred-modal-footer' },
          h(Antd.Button, {
            danger: true,
            size: 'small',
            icon: h(Icons.DeleteOutlined),
            loading: credentialClearing,
            disabled: credentialBusy,
            onClick: () => context.guard(() => clearCredential(credentialModalSource!))(),
          }, tr('清除已保存凭据', 'Clear saved credential'))) : null) : null));
  }
  const view = mountReactView(root, h(View));
  const refreshModules = async (invalidateCache = false): Promise<void> => {
    const generation = ++bindingGeneration;
    bindingsLoading = true; update();
    try {
      const [current, sources] = await Promise.all([
        context.request<GameModuleList>('modules.list', {}), context.call('sources.list', {}),
      ]);
      if (!active() || generation !== bindingGeneration) return;
      const previous = hasLoadedModules ? JSON.stringify(descriptors.find(item => item.gameId === gameId)) : undefined;
      availableSources = new Set(sources.sources.map(source => source.sourceId));
      sourceCredentials = new Map(sources.sources.map(source => [source.sourceId, Boolean(source.credentialConfigured)]));
      descriptors = current.modules.flatMap(module => module.enabled && module.game?.descriptor ? [{ ...module.game.descriptor, sourceBindings: module.game.descriptor.sourceBindings.filter(binding => availableSources.has(binding.sourceId)) }] : []);
      gameId = descriptors.some(item => item.gameId === gameId) ? gameId : descriptors[0]?.gameId ?? '';
      const nextDescriptor = JSON.stringify(descriptors.find(item => item.gameId === gameId));
      const descriptorChanged = hasLoadedModules && previous !== nextDescriptor;
      const firstLoad = !hasLoadedModules;
      hasLoadedModules = true;
      if (invalidateCache || descriptorChanged) context.invalidate('');
      if (firstLoad || descriptorChanged || !profiles.length) {
        await loadBindings();
      } else {
        update();
      }
    } catch (cause) {
      if (active() && generation === bindingGeneration) fail(cause, 'bindings');
    } finally {
      if (active() && generation === bindingGeneration) { bindingsLoading = false; update(); }
    }
  };
  const subscription = context.onInventoryChanged(() => { run(() => refreshModules(false)); });
  run(() => refreshModules(false));
  return { dispose() { persist(); clearCopyTimer(); resetCredentialDraft(); alive = false; ++requestGeneration; ++bindingGeneration; subscription.dispose(); view.dispose(); } };
}
