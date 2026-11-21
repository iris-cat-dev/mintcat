import type {
  ActionRequired, AgentConnectionTestResult, GameSummary, JobSnapshot, ProfileSummary,
} from '@mintcat/sdk-ts';
import {
  getLastUiSelection,
  getSharedRuntime,
  mountReactView,
  presentUiError,
  subscribeUiSelection,
  validateSourceCredentialInput,
} from '@mintcat/sdk-ui';
import type { Disposable, UiContext } from '@mintcat/sdk-ui';

interface Configuration { baseUrl: string; model: string; credentialRef?: string }
interface Target { gameId: string; profileId: string; profileRevision: number; contentRevision: number; installationId: string; installationRevision: number }
interface Session {
  sessionId: string; revision: number; state: 'idle' | 'sending' | 'failed'; configuration: Configuration; target: Target;
  timeline: Array<{ id: string; role: string; content: string; toolName?: string }>;
  bindings: Array<{ jobId?: string; actionId?: string }>; errorCode?: string;
}

export async function mountAgentPage(root: HTMLElement, context: UiContext): Promise<Disposable> {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  let alive = true, busy = false, drawerOpen = true, loadGeneration = 0, bindingsGeneration = 0;
  let targetLoading = false, keySaving = false, keyClearing = false, testing = false;
  let showAdvancedTargets = false;
  const active = (): boolean => alive && !context.signal.aborted;
  const saved = context.state('conversation', 1, { sessionId: '', turnJobId: '', baseUrl: 'https://api.openai.com/v1', model: '' });
  let session: Session | undefined, turnJobId = saved.turnJobId ?? '', turnPending = Boolean(turnJobId);
  let prompt = context.state<string>('prompt', 1, ''), error = '', status = '';
  let games: GameSummary[] = [], profiles: ProfileSummary[] = [];
  let apiKeyInput = '', apiKeyConfigured = false, testResult: AgentConnectionTestResult | undefined;
  const fields: Record<string, string> = {
    baseUrl: saved.baseUrl || 'https://api.openai.com/v1',
    model: saved.model || '',
    credentialRef: '',
    gameId: '',
    profileId: '',
    profileRevision: '0',
    contentRevision: '0',
    installationId: '',
    installationRevision: '0',
    sessionId: saved.sessionId,
  };
  let jobs: JobSnapshot[] = [], actions: ActionRequired[] = [];
  const responses: Record<string, string> = {};
  let redraw = (): void => {};
  const update = (): void => { if (active()) redraw(); };
  const persistConfig = (): void => {
    if (!active()) return;
    context.saveState('conversation', 1, {
      sessionId: session?.sessionId ?? fields.sessionId,
      turnJobId,
      baseUrl: fields.baseUrl,
      model: fields.model,
    });
  };
  const persistPrompt = (): void => { context.saveState('prompt', 1, prompt); context.dirty(Boolean(prompt)); };
  context.dirty(Boolean(prompt));
  const findActions = (value: unknown, ids: Set<string>): void => {
    if (Array.isArray(value)) { for (const item of value) findActions(item, ids); }
    else if (value && typeof value === 'object') for (const [key, item] of Object.entries(value)) {
      if (key === 'actionId' && typeof item === 'string') ids.add(item); else findActions(item, ids);
    }
  };
  const syncProfileRevisions = (profileId: string): void => {
    const selectedProfile = profiles.find(item => item.profileId === profileId);
    if (!selectedProfile) return;
    fields.profileId = selectedProfile.profileId;
    fields.profileRevision = String(selectedProfile.revision);
    fields.contentRevision = String(selectedProfile.contentRevision);
  };
  const loadProfilesForGame = async (gameId: string, preferredProfileId?: string): Promise<void> => {
    if (!gameId) {
      profiles = [];
      fields.profileId = '';
      fields.profileRevision = '0';
      fields.contentRevision = '0';
      return;
    }
    const loaded: ProfileSummary[] = [];
    let cursor: string | undefined;
    do {
      const page = await context.call('profiles.list', { gameId, pageSize: 100, ...(cursor ? { cursor } : {}) });
      if (!active()) return;
      loaded.push(...page.items);
      cursor = page.nextCursor;
    } while (cursor);
    profiles = loaded;
    const chosen = profiles.find(item => item.profileId === preferredProfileId)
      ?? profiles.find(item => item.profileId === fields.profileId)
      ?? profiles.find(item => item.active)
      ?? profiles[0];
    if (chosen) {
      syncProfileRevisions(chosen.profileId);
    } else {
      fields.profileId = '';
      fields.profileRevision = '0';
      fields.contentRevision = '0';
    }
  };
  const syncTargetsFromWorkbench = async (preferred?: { gameId?: string; profileId?: string }): Promise<void> => {
    targetLoading = true; update();
    try {
      const [gamesPage, credStatus] = await Promise.all([
        context.call('games.list', { pageSize: 100 }),
        context.call('agent.credentials.status', {}).catch(() => ({ credentialConfigured: false, credentialRef: undefined })),
      ]);
      if (!active()) return;
      apiKeyConfigured = Boolean(credStatus.credentialConfigured);
      if (credStatus.credentialRef && !fields.credentialRef) {
        fields.credentialRef = credStatus.credentialRef;
      }
      games = gamesPage.items;
      const hint = preferred ?? getLastUiSelection();
      const chosenGame = games.find(item => item.gameId === hint?.gameId)
        ?? games.find(item => item.gameId === fields.gameId)
        ?? games[0];
      if (chosenGame) {
        fields.gameId = chosenGame.gameId;
        const chosenInst = chosenGame.installations.find(inst => inst.installationId === fields.installationId)
          ?? chosenGame.installations[0];
        fields.installationId = chosenInst?.installationId ?? '';
        fields.installationRevision = '0';
        await loadProfilesForGame(chosenGame.gameId, hint?.profileId);
      }
    } catch {
      /* Empty/uninitialized workspace still allows manual entry. */
    } finally {
      if (active()) { targetLoading = false; update(); }
    }
  };
  const renderBindings = async (): Promise<void> => {
    if (!session || !active()) return;
    const generation = ++bindingsGeneration, current = session;
    const loadedJobs: JobSnapshot[] = [], ids = new Set(current.bindings.flatMap(binding => binding.actionId ? [binding.actionId] : []));
    for (const jobId of new Set([turnJobId, ...current.bindings.map(binding => binding.jobId ?? '')].filter(Boolean))) {
      const job = await context.call('jobs.get', { jobId });
      if (!active() || generation !== bindingsGeneration || session?.sessionId !== current.sessionId) return;
      if (jobId === turnJobId) turnPending = !['succeeded', 'failed', 'cancelled', 'recovery-required'].includes(job.state);
      loadedJobs.push(job); findActions(job.result, ids);
      if (job.state === 'awaiting-approval' && !job.result) {
        let afterSeq = 0;
        for (;;) {
          const page = await context.call('jobs.watch', { jobId, afterSeq, limit: 100 });
          if (!active() || generation !== bindingsGeneration) return;
          for (const event of page.events) findActions(event.details, ids);
          if (page.nextAfterSeq >= page.snapshot.lastSeq || page.nextAfterSeq <= afterSeq) break;
          afterSeq = page.nextAfterSeq;
        }
      }
    }
    const loadedActions: ActionRequired[] = [];
    for (const actionId of ids) {
      const action = await context.call('actions.get', { actionId });
      if (!active() || generation !== bindingsGeneration) return;
      loadedActions.push(action); responses[actionId] ??= '{}';
    }
    if (active() && generation === bindingsGeneration && session?.sessionId === current.sessionId) { jobs = loadedJobs; actions = loadedActions; update(); }
  };
  const render = async (next: Session): Promise<void> => {
    if (!active()) return;
    const changed = session?.sessionId !== next.sessionId;
    session = next; fields.sessionId = next.sessionId;
    if (changed) for (const [key, value] of Object.entries({ ...next.configuration, ...next.target })) fields[key] = String(value ?? '');
    persistConfig();
    status = `${next.sessionId} · 版本 ${next.revision} · ${next.state}${next.errorCode ? ` · ${next.errorCode}` : ''}`;
    update(); await renderBindings();
  };
  const describeError = (cause: unknown): string => {
    if (typeof cause === 'object' && cause !== null && 'appError' in cause) {
      const failure = presentUiError(cause);
      return `${failure.title}：${failure.description}`;
    }
    return cause instanceof Error ? cause.message : JSON.stringify(cause, null, 2) ?? String(cause);
  };
  const run = (action: () => Promise<void>): void => {
    if (busy || !active()) return;
    ++loadGeneration; ++bindingsGeneration; busy = true; error = ''; update();
    context.guard(async () => {
      try { await action(); }
      catch (cause) { if (active()) { error = describeError(cause); update(); } throw cause; }
      finally { if (active()) { busy = false; update(); } }
    })();
  };
  const revision = (key: string): number => {
    const raw = fields[key].trim(), value = Number(raw);
    if (!raw || !Number.isSafeInteger(value) || value < 0) throw new Error('请显式填写非负整数版本，不能猜测目标版本。');
    return value;
  };
  const saveApiKey = async (): Promise<void> => {
    if (keySaving || keyClearing) return;
    const validated = validateSourceCredentialInput(apiKeyInput);
    if (!validated.ok) { error = validated.message; update(); return; }
    keySaving = true; error = ''; update();
    try {
      const result = await context.call('agent.credentials.set', { secret: validated.value }, crypto.randomUUID());
      if (!active()) return;
      apiKeyInput = '';
      apiKeyConfigured = result.credentialConfigured;
      fields.credentialRef = result.credentialRef ?? '';
      status = 'Agent API Key 已安全保存到本机系统钥匙串。';
      context.notify(status);
    } catch (cause) {
      if (active()) error = describeError(cause);
    } finally {
      if (active()) { keySaving = false; update(); }
    }
  };
  const clearApiKey = async (): Promise<void> => {
    if (keySaving || keyClearing) return;
    keyClearing = true; error = ''; update();
    try {
      await context.call('agent.credentials.clear', {}, crypto.randomUUID());
      if (!active()) return;
      apiKeyConfigured = false;
      fields.credentialRef = '';
      status = '已清除本机系统钥匙串中的 Agent API Key。';
      context.notify(status);
    } catch (cause) {
      if (active()) error = describeError(cause);
    } finally {
      if (active()) { keyClearing = false; update(); }
    }
  };
  const testConnection = async (): Promise<void> => {
    if (testing) return;
    if (!fields.baseUrl.trim() || !fields.model.trim()) {
      error = '请先填写 OpenAI 兼容服务地址和模型名称。'; update(); return;
    }
    testing = true; error = ''; testResult = undefined; update();
    try {
      const credentialRef = fields.credentialRef.trim();
      const result = await context.call('agent.connection.test', {
        baseUrl: fields.baseUrl.trim(),
        model: fields.model.trim(),
        ...(credentialRef ? { credentialRef } : {}),
      });
      if (!active()) return;
      testResult = result;
    } catch (cause) {
      if (active()) error = describeError(cause);
    } finally {
      if (active()) { testing = false; update(); }
    }
  };
  const create = async (): Promise<void> => {
    persistConfig();
    const credentialRef = fields.credentialRef.trim();
    const next = await context.request<Session>('agent.sessions.create', {
      configuration: { baseUrl: fields.baseUrl.trim(), model: fields.model.trim(), ...(credentialRef ? { credentialRef } : {}) },
      target: { gameId: fields.gameId.trim(), profileId: fields.profileId.trim(), profileRevision: revision('profileRevision'), contentRevision: revision('contentRevision'), installationId: fields.installationId.trim(), installationRevision: revision('installationRevision') },
    }, crypto.randomUUID());
    if (!active()) return;
    turnJobId = ''; turnPending = false; await render(next);
  };
  const load = async (): Promise<void> => {
    if (!fields.sessionId.trim()) throw new Error('请输入会话 ID。');
    const next = await context.request<Session>('agent.sessions.get', { sessionId: fields.sessionId.trim() });
    if (!active()) return;
    if (next.sessionId !== session?.sessionId) { turnJobId = ''; turnPending = false; }
    await render(next);
  };
  const send = async (): Promise<void> => {
    if (!session) throw new Error('请先创建或读取会话。');
    const submitted = prompt, content = submitted.trim(), current = session;
    if (!content) throw new Error('请输入消息。');
    if (current.state === 'sending' || turnPending) throw new Error('上一轮会话仍在运行。');
    try {
      const job = await context.request<{ jobId: string }>('agent.sessions.send', { sessionId: current.sessionId, expectedRevision: current.revision, content }, crypto.randomUUID());
      const latest = context.state('conversation', 1, { sessionId: current.sessionId, turnJobId: '', baseUrl: fields.baseUrl, model: fields.model });
      if (latest.sessionId === current.sessionId) context.saveState('conversation', 1, { ...latest, sessionId: current.sessionId, turnJobId: job.jobId });
      if (context.state('prompt', 1, '') === submitted) context.saveState('prompt', 1, '');
      if (!active()) return;
      turnJobId = job.jobId; turnPending = true;
      if (prompt === submitted) { prompt = ''; persistPrompt(); }
      await render(await context.request<Session>('agent.sessions.get', { sessionId: current.sessionId }));
    } catch (cause) {
      if (!active()) return;
      await render(await context.request<Session>('agent.sessions.get', { sessionId: current.sessionId }));
      throw cause;
    }
  };
  const approve = async (reviewed: ActionRequired): Promise<void> => {
    const response = JSON.parse(responses[reviewed.actionId]) as Record<string, unknown>;
    const current = await context.call('actions.get', { actionId: reviewed.actionId });
    if (!active()) return;
    if (current.state !== 'pending' || JSON.stringify(current) !== JSON.stringify(reviewed)) throw new Error('批准目标已变化，请刷新并重新审核。');
    if (!window.confirm(`以人工身份批准此不可变目标？\n\n${JSON.stringify({ action: current, response }, null, 2)}`)) return;
    const job = await context.call('actions.approve', { actionId: current.actionId, expectedRevision: current.revision, response }, crypto.randomUUID());
    if (!active()) return;
    status = `人工批准已受理 · ${job.jobId}`; await renderBindings();
  };
  const advancedLabels: Array<[string, string, boolean?]> = [
    ['credentialRef', 'daemon 凭据引用（自动填充）'],
    ['gameId', '游戏 ID'],
    ['profileId', '配置 ID'],
    ['profileRevision', '配置版本', true],
    ['contentRevision', '内容版本', true],
    ['installationId', '安装位置 ID'],
    ['installationRevision', '安装位置版本', true],
  ];
  function View() {
    const [, setTick] = React.useState(0); redraw = () => setTick(value => value + 1);
    const end = React.useRef<HTMLDivElement>(null);
    React.useEffect(() => { end.current?.scrollIntoView({ block: 'nearest' }); }, [session?.timeline.length]);
    const pendingApproval = actions.some(action => action.state === 'pending');
    const sendDisabled = busy || !session || session.state === 'sending' || turnPending || pendingApproval || !prompt.trim();
    const activeGame = games.find(item => item.gameId === fields.gameId);
    const installations = activeGame?.installations ?? [];
    return h('div', { className: 'mintcat-agent-surface' },
      !drawerOpen ? h(Antd.Button, { type: 'primary', className: 'mintcat-agent-reopen glass-primary-action', icon: h(Icons.RobotOutlined), onClick: () => { drawerOpen = true; update(); } }, '打开 Agent 助手') : null,
      h(Antd.Drawer, { open: drawerOpen, placement: 'right', size: 540, getContainer: false, mask: false, onClose: () => { drawerOpen = false; update(); }, title: h('div', { className: 'mintcat-agent-drawer-title' }, h('span', { className: 'mintcat-agent-title-badge' }, h(Icons.RobotOutlined)), h('div', null, h('div', { className: 'mintcat-agent-title-text' }, 'Agent 助手'), h(Antd.Typography.Text, { type: 'secondary', style: { fontSize: 11.5, fontWeight: 400 } }, '对话、工具与独立人工批准'))), styles: { body: { padding: 0, overflow: 'hidden' } }, extra: h(Antd.Space, { size: 6 }, h(Antd.Button, { size: 'small', icon: h(Icons.SyncOutlined, { spin: targetLoading }), disabled: busy || targetLoading, onClick: () => context.guard(() => syncTargetsFromWorkbench())() }, '同步当前目标'), h(Antd.Button, { size: 'small', icon: h(Icons.ReloadOutlined), disabled: busy || !session, onClick: () => run(async () => { if (session) await render(await context.request<Session>('agent.sessions.get', { sessionId: session.sessionId })); }) }, '刷新')) },
        h('div', { className: 'mintcat-agent-page' },
          h(Antd.Collapse, { size: 'small', defaultActiveKey: session ? [] : ['settings'], items: [{ key: 'settings', label: h(Antd.Space, { size: 6 }, h('span', null, '模型、API Key 与目标配置'), apiKeyConfigured ? h(Antd.Tag, { color: 'success' }, '已存钥匙串 Key') : null), children: h(Antd.Form, { layout: 'vertical' },
            h(Antd.Alert, { type: 'info', showIcon: true, style: { marginBottom: 10 }, title: '模型工具不能批准或安装；API Key 仅保存在本机系统钥匙串中。' }),
            h('div', { className: 'mintcat-agent-fields' },
              h(Antd.Form.Item, { label: 'OpenAI 兼容服务地址', className: 'mintcat-agent-field-wide', style: { marginBottom: 8 } },
                h(Antd.Input, { size: 'small', value: fields.baseUrl, placeholder: 'https://api.openai.com/v1', disabled: busy, onChange: (event: { target: { value: string } }) => { fields.baseUrl = event.target.value; persistConfig(); update(); } })),
              h(Antd.Form.Item, { label: '模型名称', style: { marginBottom: 8 } },
                h(Antd.Input, { size: 'small', value: fields.model, placeholder: 'gpt-4o-mini / deepseek-chat', disabled: busy, onChange: (event: { target: { value: string } }) => { fields.model = event.target.value; persistConfig(); update(); } })),
              h(Antd.Form.Item, { label: 'API Key（本机钥匙串加密保存）', style: { marginBottom: 8 } },
                h(Antd.Space.Compact, { style: { width: '100%' } },
                  h(Antd.Input.Password, {
                    size: 'small',
                    value: apiKeyInput,
                    placeholder: apiKeyConfigured ? '已保存，输入新密钥可替换' : '粘贴 sk-... 密钥',
                    autoComplete: 'off',
                    disabled: busy || keySaving || keyClearing,
                    onChange: (event: { target: { value: string } }) => { apiKeyInput = event.target.value; update(); },
                  }),
                  h(Antd.Button, { size: 'small', type: 'primary', loading: keySaving, disabled: busy || keyClearing || !apiKeyInput.trim(), onClick: () => context.guard(saveApiKey)() }, '保存'),
                  apiKeyConfigured ? h(Antd.Button, { size: 'small', danger: true, loading: keyClearing, disabled: busy || keySaving, onClick: () => context.guard(clearApiKey)() }, '清除') : null)),
              h(Antd.Form.Item, { label: '目标游戏', style: { marginBottom: 8 } },
                h(Antd.Select<string>, {
                  size: 'small',
                  value: fields.gameId || undefined,
                  placeholder: '自动读取已注册游戏',
                  disabled: busy || targetLoading || !games.length,
                  options: games.map(item => ({ value: item.gameId, label: item.displayName })),
                  onChange: (value: string) => {
                    fields.gameId = value;
                    const chosen = games.find(g => g.gameId === value);
                    fields.installationId = chosen?.installations[0]?.installationId ?? '';
                    fields.installationRevision = '0';
                    context.guard(() => loadProfilesForGame(value).then(update))();
                  },
                })),
              h(Antd.Form.Item, { label: '目标配置（自动带入修订号）', style: { marginBottom: 8 } },
                h(Antd.Select<string>, {
                  size: 'small',
                  value: fields.profileId || undefined,
                  placeholder: '选择配置',
                  disabled: busy || targetLoading || !profiles.length,
                  options: profiles.map(item => ({ value: item.profileId, label: `${item.displayName} (r${item.revision}/c${item.contentRevision})` })),
                  onChange: (value: string) => { syncProfileRevisions(value); update(); },
                })),
              h(Antd.Form.Item, { label: '目标安装目录', className: 'mintcat-agent-field-wide', style: { marginBottom: 8 } },
                h(Antd.Select<string>, {
                  size: 'small',
                  value: fields.installationId || undefined,
                  placeholder: '选择已注册安装目录',
                  disabled: busy || targetLoading || !installations.length,
                  options: installations.map(inst => ({ value: inst.installationId, label: inst.rootPath || inst.installationId })),
                  onChange: (value: string) => { fields.installationId = value; fields.installationRevision = '0'; update(); },
                }))),
            testResult ? h(Antd.Alert, {
              type: testResult.ok ? 'success' : 'warning',
              showIcon: true,
              style: { marginBottom: 8 },
              title: testResult.message,
            }) : null,
            h(Antd.Space, { wrap: true, style: { marginTop: 4 } },
              h(Antd.Button, { size: 'small', type: 'primary', className: 'glass-primary-action', disabled: busy, onClick: () => run(create) }, '创建会话'),
              h(Antd.Button, { size: 'small', icon: h(Icons.ApiOutlined), loading: testing, disabled: busy || testing || !fields.baseUrl.trim() || !fields.model.trim(), onClick: () => context.guard(testConnection)() }, '测试连接'),
              h(Antd.Button, { size: 'small', type: 'link', onClick: () => { showAdvancedTargets = !showAdvancedTargets; update(); } }, showAdvancedTargets ? '收起高级参数' : '高级目标参数'),
              h(Antd.Input, { size: 'small', style: { width: 136 }, value: fields.sessionId, placeholder: '已有会话 ID', disabled: busy, onChange: (event: { target: { value: string } }) => { fields.sessionId = event.target.value; update(); } }),
              h(Antd.Button, { size: 'small', disabled: busy, onClick: () => run(load) }, '读取会话'),
              h(Antd.Button, { size: 'small', danger: true, disabled: busy || !session, icon: h(Icons.ClearOutlined), onClick: () => run(async () => {
                if (!session) throw new Error('没有可删除的会话。');
                await context.request('agent.sessions.delete', { sessionId: session.sessionId, expectedRevision: session.revision }, crypto.randomUUID());
                if (!active()) return;
                session = undefined; fields.sessionId = ''; turnJobId = ''; turnPending = false; jobs = []; actions = [];
                persistConfig(); status = '仅会话已删除，daemon 任务和批准动作保持不变。';
              }) }, '删除会话')),
            showAdvancedTargets ? h('div', { className: 'mintcat-agent-fields', style: { marginTop: 10 } },
              ...advancedLabels.map(([key, label, numeric]) => h(Antd.Form.Item, { key, label, style: { marginBottom: 6 } },
                h(Antd.Input, { size: 'small', value: fields[key], type: numeric ? 'number' : 'text', min: numeric ? 0 : undefined, disabled: busy, onChange: (event: { target: { value: string } }) => { fields[key] = event.target.value; update(); } })))) : null) }] }),
          h('div', { className: 'mintcat-agent-message-list' },
            !session?.timeline.length ? h('div', { className: 'mintcat-agent-empty' },
              h('div', { className: 'mintcat-agent-empty-orb' }, h(Icons.RobotOutlined)),
              h('div', { className: 'mintcat-agent-empty-title' }, 'MintCat 智能模组助手'),
              h('div', { className: 'mintcat-agent-empty-desc' }, '已自动同步当前游戏与配置目标；填入模型与 API Key 后即可创建会话开始对话。')) : null,
            ...(session?.timeline ?? []).map(item => h('div', { key: item.id, className: `mintcat-agent-message mintcat-agent-message-${item.role}` }, h('div', { className: 'mintcat-agent-message-icon' }, h(item.role === 'user' ? Icons.UserOutlined : item.role === 'assistant' ? Icons.RobotOutlined : Icons.ToolOutlined)), h('div', { className: 'mintcat-agent-message-body' }, item.toolName ? h(Antd.Tag, { style: { marginBottom: 4 } }, item.toolName) : null, h('div', { style: { whiteSpace: 'pre-wrap' } }, item.content)))),
            ...actions.map(action => h(Antd.Card, { key: action.actionId, size: 'small', className: 'mintcat-agent-confirmation', title: '独立人工批准', extra: h(Antd.Tag, { color: action.state === 'pending' ? 'warning' : undefined }, action.state) },
              h(Antd.Typography.Text, { strong: true }, action.kind), h('pre', null, JSON.stringify({ actionId: action.actionId, revision: action.revision, target: action.target, targetRevision: action.targetRevision, planHash: action.planHash, expiresAt: action.expiresAt, responseSchema: action.responseSchema }, null, 2)),
              action.state === 'pending' ? h('div', null, h(Antd.Input.TextArea, { value: responses[action.actionId], 'aria-label': `批准响应 ${action.actionId}`, disabled: busy, onChange: (event: { target: { value: string } }) => { responses[action.actionId] = event.target.value; update(); }, autoSize: { minRows: 2, maxRows: 8 } }), h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.CheckOutlined), disabled: busy, style: { marginTop: 8 }, onClick: () => run(() => approve(action)) }, '审核并以人工身份批准')) : null)),
            jobs.length ? h(Antd.Collapse, { size: 'small', items: jobs.map(job => ({ key: job.jobId, label: `${job.command} · ${job.state}`, children: h('pre', null, JSON.stringify(job, null, 2)) })) }) : null,
            error ? h(Antd.Alert, { type: 'error', showIcon: true, title: error }) : null,
            busy || turnPending || session?.state === 'sending' ? h('div', { className: 'mintcat-agent-loading' }, h(Antd.Spin, { size: 'small' }), '正在处理会话及工具任务…') : null,
            h('div', { ref: end })),
          status ? h('div', { role: 'status', className: 'mintcat-agent-status' }, status) : h('span', { role: 'status', className: 'mintcat-agent-status-empty' }),
          h('div', { className: 'mintcat-agent-composer' }, h(Antd.Input.TextArea, { value: prompt, maxLength: 32768, disabled: busy || !session || turnPending || pendingApproval, autoSize: { minRows: 2, maxRows: 5 }, placeholder: pendingApproval ? '请先独立审核待批准动作' : '输入消息，Shift+Enter 换行…', onChange: (event: { target: { value: string } }) => { prompt = event.target.value; persistPrompt(); update(); }, onPressEnter: (event: { shiftKey: boolean; preventDefault(): void }) => { if (!event.shiftKey) { event.preventDefault(); if (!sendDisabled) run(send); } } }), h(Antd.Button, { type: 'primary', className: 'glass-primary-action mintcat-agent-send-btn', icon: h(Icons.SendOutlined), disabled: sendDisabled, onClick: () => run(send) }, '发送')))));
  }
  const view = mountReactView(root, h(View));
  const selectionSub = subscribeUiSelection(selection => {
    if (!session) context.guard(() => syncTargetsFromWorkbench({ gameId: selection.gameId, profileId: selection.profileId }))();
  });
  context.guard(() => syncTargetsFromWorkbench())();
  if (saved.sessionId) run(load);
  const poll = context.interval(async () => {
    if (busy || !session || !active()) return;
    const current = session.sessionId, generation = ++loadGeneration;
    try {
      const next = await context.request<Session>('agent.sessions.get', { sessionId: current });
      if (active() && generation === loadGeneration && session?.sessionId === current) await render(next);
    } catch (cause) { if (active() && generation === loadGeneration) { error = describeError(cause); update(); } }
  }, 2000);
  return { dispose() { persistPrompt(); persistConfig(); alive = false; ++loadGeneration; ++bindingsGeneration; selectionSub(); poll.dispose(); context.dirty(false); view.dispose(); } };
}
