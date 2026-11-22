import type {
  ActionRequired, AgentConnectionTestResult, GameSummary, JobSnapshot, ProfileSummary,
} from '@mintcat/sdk-ts';
import {
  getLastUiSelection,
  getSharedRuntime,
  getUiLanguage,
  mountReactView,
  presentUiError,
  subscribeUiSelection,
  tr,
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

export interface AgentIndicatorStatus { open: boolean; waiting: boolean; unread: boolean }
export interface AgentPanel extends Disposable { setOpen(open: boolean): void }

const sessionStateLabels: Record<Session['state'], string> = {
  idle: tr('空闲', 'Idle'), sending: tr('发送中', 'Sending'), failed: tr('失败', 'Failed'),
};
const jobStateLabels: Record<JobSnapshot['state'], string> = {
  queued: tr('排队中', 'Queued'), preparing: tr('准备中', 'Preparing'), 'awaiting-approval': tr('等待人工批准', 'Awaiting manual approval'),
  running: tr('执行中', 'Running'), committing: tr('提交中', 'Committing'), succeeded: tr('已完成', 'Completed'),
  cancelling: tr('正在取消', 'Cancelling'), cancelled: tr('已取消', 'Cancelled'), failed: tr('失败', 'Failed'),
  'recovery-required': tr('需要恢复', 'Recovery required'),
};
const approvalStateLabels: Record<ActionRequired['state'], string> = {
  pending: tr('待批准', 'Pending'), approved: tr('已批准', 'Approved'), rejected: tr('已拒绝', 'Rejected'),
  expired: tr('已过期', 'Expired'), consumed: tr('已使用', 'Used'),
};

function connectionTestMessage(result: AgentConnectionTestResult): string {
  const { message, ok, statusCode, latencyMs } = result;
  if (getUiLanguage() === 'zh-CN') return message;
  // Localize the daemon's built-in probe diagnostics; preserve unrecognized messages verbatim.
  if (ok && message === `连接成功（HTTP ${statusCode}，${latencyMs} ms）`) {
    return `Connected (HTTP ${statusCode}, ${latencyMs.toLocaleString(getUiLanguage())} ms)`;
  }
  if (message === `鉴权失败（HTTP ${statusCode}），请检查 API Key 是否有效`) return `Authentication failed (HTTP ${statusCode}). Check that the API key is valid.`;
  if (message === '模型或接口地址不存在（HTTP 404），请检查 Base URL 与模型名称') return 'Model or endpoint not found (HTTP 404). Check the base URL and model name.';
  if (message === '请求触发限流（HTTP 429），凭据已通过但配额受限') return 'Rate limited (HTTP 429). The credentials were accepted, but the request hit a quota limit.';
  if (message === `服务返回异常状态（HTTP ${statusCode}）`) return `The service returned an unexpected status (HTTP ${statusCode}).`;
  if (message === '连接超时，请检查网络或代理设置') return 'Connection timed out. Check your network or proxy settings.';
  if (message === '无法从本机钥匙串读取凭据，请重新保存 API Key') return 'Could not read credentials from the system credential store. Save the API key again.';
  if (message === '网络不可达，请检查 Base URL 或网络代理设置') return 'Network unreachable. Check the base URL or proxy settings.';
  return message;
}

export function mountAgentPage(root: HTMLElement, context: UiContext): AgentPanel {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  let alive = true, busy = false, drawerOpen = true, loadGeneration = 0, bindingsGeneration = 0;
  let waiting = false, unread = false, lastIndicator: AgentIndicatorStatus | undefined;
  const publishIndicator = (): void => {
    if (lastIndicator?.open === drawerOpen && lastIndicator.waiting === waiting && lastIndicator.unread === unread) return;
    lastIndicator = { open: drawerOpen, waiting, unread };
    window.dispatchEvent(new CustomEvent('mintcat-agent-indicator', { detail: lastIndicator }));
  };
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
  const update = (): void => { if (active()) { publishIndicator(); redraw(); } };
  const setOpen = (open: boolean): void => {
    if (open === drawerOpen) return;
    drawerOpen = open;
    if (open) unread = false;
    update();
  };
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
      if (jobId === turnJobId) {
        turnPending = !['succeeded', 'failed', 'cancelled', 'recovery-required'].includes(job.state);
        if (['failed', 'cancelled', 'recovery-required'].includes(job.state)) { waiting = false; publishIndicator(); }
      }
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
    if (changed) {
      // The first snapshot (including a restored conversation) is the read baseline.
      unread = false;
      waiting = next.state === 'sending';
    } else if (session) {
      const newReply = next.timeline.length > session.timeline.length
        && next.timeline.slice(session.timeline.length).some(item => item.role === 'assistant');
      if (newReply) { waiting = false; if (!drawerOpen) unread = true; }
      else if (next.state === 'failed') waiting = false;
      else if (next.state === 'sending' && session.state !== 'sending') waiting = true;
    }
    session = next; fields.sessionId = next.sessionId;
    if (changed) for (const [key, value] of Object.entries({ ...next.configuration, ...next.target })) fields[key] = String(value ?? '');
    persistConfig();
    const state = sessionStateLabels[next.state] ?? next.state;
    status = `${next.sessionId} · ${tr('版本', 'Revision')} ${next.revision} · ${state}${next.errorCode ? ` · ${next.errorCode}` : ''}`;
    update(); await renderBindings();
  };
  const describeError = (cause: unknown): string => {
    if (typeof cause === 'object' && cause !== null && 'appError' in cause) {
      const failure = presentUiError(cause);
      return tr(`${failure.title}：${failure.description}`, `${failure.title}: ${failure.description}`);
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
    if (!raw || !Number.isSafeInteger(value) || value < 0) throw new Error(tr('请显式填写非负整数版本，不能猜测目标版本。', 'Enter the exact target revision as a nonnegative integer.'));
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
      status = tr('Agent API Key 已安全保存到本机系统钥匙串。', 'The Agent API key was saved securely in the system credential store.');
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
      status = tr('已清除本机系统钥匙串中的 Agent API Key。', 'The Agent API key was removed from the system credential store.');
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
      error = tr('请先填写 OpenAI 兼容服务地址和模型名称。', 'Enter an OpenAI-compatible service URL and model name first.'); update(); return;
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
    if (!fields.sessionId.trim()) throw new Error(tr('请输入会话 ID。', 'Enter a session ID.'));
    const next = await context.request<Session>('agent.sessions.get', { sessionId: fields.sessionId.trim() });
    if (!active()) return;
    if (next.sessionId !== (session?.sessionId ?? saved.sessionId)) { turnJobId = ''; turnPending = false; }
    await render(next);
  };
  const send = async (): Promise<void> => {
    if (!session) throw new Error(tr('请先创建或读取会话。', 'Create or load a session first.'));
    const submitted = prompt, content = submitted.trim(), current = session;
    if (!content) throw new Error(tr('请输入消息。', 'Enter a message.'));
    if (current.state === 'sending' || turnPending) throw new Error(tr('上一轮会话仍在运行。', 'The previous turn is still running.'));
    waiting = true; update();
    let accepted = false;
    try {
      const job = await context.request<{ jobId: string }>('agent.sessions.send', { sessionId: current.sessionId, expectedRevision: current.revision, content }, crypto.randomUUID());
      accepted = true;
      const latest = context.state('conversation', 1, { sessionId: current.sessionId, turnJobId: '', baseUrl: fields.baseUrl, model: fields.model });
      if (latest.sessionId === current.sessionId) context.saveState('conversation', 1, { ...latest, sessionId: current.sessionId, turnJobId: job.jobId });
      if (context.state('prompt', 1, '') === submitted) context.saveState('prompt', 1, '');
      if (!active()) return;
      turnJobId = job.jobId; turnPending = true;
      if (prompt === submitted) { prompt = ''; persistPrompt(); }
      await render(await context.request<Session>('agent.sessions.get', { sessionId: current.sessionId }));
    } catch (cause) {
      if (!active()) return;
      if (!accepted) { waiting = false; update(); }
      await render(await context.request<Session>('agent.sessions.get', { sessionId: current.sessionId }));
      throw cause;
    }
  };
  const approve = async (reviewed: ActionRequired): Promise<void> => {
    const response = JSON.parse(responses[reviewed.actionId]) as Record<string, unknown>;
    const current = await context.call('actions.get', { actionId: reviewed.actionId });
    if (!active()) return;
    if (current.state !== 'pending' || JSON.stringify(current) !== JSON.stringify(reviewed)) throw new Error(tr('批准目标已变化，请刷新并重新审核。', 'The approval target changed. Refresh and review it again.'));
    if (!window.confirm(`${tr('以人工身份批准此不可变目标？', 'Approve this exact target as a human reviewer?')}\n\n${JSON.stringify({ action: current, response }, null, 2)}`)) return;
    const job = await context.call('actions.approve', { actionId: current.actionId, expectedRevision: current.revision, response }, crypto.randomUUID());
    if (!active()) return;
    status = tr(`人工批准已受理 · ${job.jobId}`, `Manual approval accepted · ${job.jobId}`); await renderBindings();
  };
  const advancedLabels: Array<[string, string, boolean?]> = [
    ['credentialRef', tr('daemon 凭据引用（自动填充）', 'Daemon credential reference (filled automatically)')],
    ['gameId', tr('游戏 ID', 'Game ID')],
    ['profileId', tr('配置 ID', 'Profile ID')],
    ['profileRevision', tr('配置版本', 'Profile revision'), true],
    ['contentRevision', tr('内容版本', 'Content revision'), true],
    ['installationId', tr('安装位置 ID', 'Installation ID')],
    ['installationRevision', tr('安装位置版本', 'Installation revision'), true],
  ];
  function View() {
    const [, setTick] = React.useState(0); redraw = () => setTick(value => value + 1);
    const end = React.useRef<HTMLDivElement>(null);
    React.useEffect(() => { if (drawerOpen) end.current?.scrollIntoView({ block: 'nearest' }); }, [drawerOpen, session?.timeline.length]);
    const pendingApproval = actions.some(action => action.state === 'pending');
    const sendDisabled = busy || !session || session.state === 'sending' || turnPending || pendingApproval || !prompt.trim();
    const activeGame = games.find(item => item.gameId === fields.gameId);
    const installations = activeGame?.installations ?? [];
    return h('div', { className: 'mintcat-agent-surface' },
      h(Antd.Drawer, { id: 'mintcat-agent-drawer', open: drawerOpen, placement: 'right', size: 540, getContainer: false, mask: false, onClose: () => setOpen(false), title: h('div', { className: 'mintcat-agent-drawer-title' }, h('span', { className: 'mintcat-agent-title-badge' }, h(Icons.RobotOutlined)), h('div', null, h('div', { className: 'mintcat-agent-title-text' }, tr('Agent 助手', 'Agent Assistant')), h(Antd.Typography.Text, { type: 'secondary', style: { fontSize: 11.5, fontWeight: 400 } }, tr('对话、工具与独立人工批准', 'Chat, tools, and separate manual approval')))), styles: { body: { padding: 0, overflow: 'hidden' } }, extra: h(Antd.Space, { size: 6 }, h(Antd.Button, { size: 'small', icon: h(Icons.SyncOutlined, { spin: targetLoading }), disabled: busy || targetLoading, onClick: () => context.guard(() => syncTargetsFromWorkbench())() }, tr('同步当前目标', 'Sync Current Target')), h(Antd.Button, { size: 'small', icon: h(Icons.ReloadOutlined), disabled: busy || !session, onClick: () => run(async () => { if (session) await render(await context.request<Session>('agent.sessions.get', { sessionId: session.sessionId })); }) }, tr('刷新', 'Refresh'))) },
        h('div', { className: 'mintcat-agent-page' },
          h(Antd.Collapse, { size: 'small', defaultActiveKey: session ? [] : ['settings'], items: [{ key: 'settings', label: h(Antd.Space, { size: 6 }, h('span', null, tr('模型、API Key 与目标配置', 'Model, API Key, and Target Settings')), apiKeyConfigured ? h(Antd.Tag, { color: 'success' }, tr('已存钥匙串 Key', 'API key saved')) : null), children: h(Antd.Form, { layout: 'vertical' },
            h(Antd.Alert, { type: 'info', showIcon: true, style: { marginBottom: 10 }, title: tr('模型工具不能批准或安装；API Key 仅保存在本机系统钥匙串中。', 'Model tools cannot approve actions or install mods. The API key is stored only in the local system credential store.') }),
            h('div', { className: 'mintcat-agent-fields' },
              h(Antd.Form.Item, { label: tr('OpenAI 兼容服务地址', 'OpenAI-compatible service URL'), className: 'mintcat-agent-field-wide', style: { marginBottom: 8 } },
                h(Antd.Input, { size: 'small', value: fields.baseUrl, placeholder: 'https://api.openai.com/v1', disabled: busy, onChange: (event: { target: { value: string } }) => { fields.baseUrl = event.target.value; persistConfig(); update(); } })),
              h(Antd.Form.Item, { label: tr('模型名称', 'Model name'), style: { marginBottom: 8 } },
                h(Antd.Input, { size: 'small', value: fields.model, placeholder: 'gpt-4o-mini / deepseek-chat', disabled: busy, onChange: (event: { target: { value: string } }) => { fields.model = event.target.value; persistConfig(); update(); } })),
              h(Antd.Form.Item, { label: tr('API Key（本机钥匙串加密保存）', 'API key (encrypted in the system credential store)'), style: { marginBottom: 8 } },
                h(Antd.Space.Compact, { style: { width: '100%' } },
                  h(Antd.Input.Password, {
                    size: 'small',
                    value: apiKeyInput,
                    placeholder: apiKeyConfigured ? tr('已保存，输入新密钥可替换', 'Saved. Enter a new key to replace it') : tr('粘贴 sk-... 密钥', 'Paste your sk-... key'),
                    autoComplete: 'off',
                    disabled: busy || keySaving || keyClearing,
                    onChange: (event: { target: { value: string } }) => { apiKeyInput = event.target.value; update(); },
                  }),
                  h(Antd.Button, { size: 'small', type: 'primary', loading: keySaving, disabled: busy || keyClearing || !apiKeyInput.trim(), onClick: () => context.guard(saveApiKey)() }, tr('保存', 'Save')),
                  apiKeyConfigured ? h(Antd.Button, { size: 'small', danger: true, loading: keyClearing, disabled: busy || keySaving, onClick: () => context.guard(clearApiKey)() }, tr('清除', 'Clear')) : null)),
              h(Antd.Form.Item, { label: tr('目标游戏', 'Target game'), style: { marginBottom: 8 } },
                h(Antd.Select<string>, {
                  size: 'small',
                  value: fields.gameId || undefined,
                  placeholder: tr('自动读取已注册游戏', 'Loads registered games automatically'),
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
              h(Antd.Form.Item, { label: tr('目标配置（自动带入修订号）', 'Target profile (revisions filled automatically)'), style: { marginBottom: 8 } },
                h(Antd.Select<string>, {
                  size: 'small',
                  value: fields.profileId || undefined,
                  placeholder: tr('选择配置', 'Select a profile'),
                  disabled: busy || targetLoading || !profiles.length,
                  options: profiles.map(item => ({ value: item.profileId, label: `${item.displayName} (r${item.revision}/c${item.contentRevision})` })),
                  onChange: (value: string) => { syncProfileRevisions(value); update(); },
                })),
              h(Antd.Form.Item, { label: tr('目标安装目录', 'Target installation folder'), className: 'mintcat-agent-field-wide', style: { marginBottom: 8 } },
                h(Antd.Select<string>, {
                  size: 'small',
                  value: fields.installationId || undefined,
                  placeholder: tr('选择已注册安装目录', 'Select a registered installation folder'),
                  disabled: busy || targetLoading || !installations.length,
                  options: installations.map(inst => ({ value: inst.installationId, label: inst.rootPath || inst.installationId })),
                  onChange: (value: string) => { fields.installationId = value; fields.installationRevision = '0'; update(); },
                }))),
            testResult ? h(Antd.Alert, {
              type: testResult.ok ? 'success' : 'warning',
              showIcon: true,
              style: { marginBottom: 8 },
              title: connectionTestMessage(testResult),
            }) : null,
            h(Antd.Space, { wrap: true, style: { marginTop: 4 } },
              h(Antd.Button, { size: 'small', type: 'primary', className: 'glass-primary-action', disabled: busy, onClick: () => run(create) }, tr('创建会话', 'Create Session')),
              h(Antd.Button, { size: 'small', icon: h(Icons.ApiOutlined), loading: testing, disabled: busy || testing || !fields.baseUrl.trim() || !fields.model.trim(), onClick: () => context.guard(testConnection)() }, tr('测试连接', 'Test Connection')),
              h(Antd.Button, { size: 'small', type: 'link', onClick: () => { showAdvancedTargets = !showAdvancedTargets; update(); } }, showAdvancedTargets ? tr('收起高级参数', 'Hide Advanced Settings') : tr('高级目标参数', 'Advanced Target Settings')),
              h(Antd.Input, { size: 'small', style: { width: 136 }, value: fields.sessionId, placeholder: tr('已有会话 ID', 'Existing session ID'), disabled: busy, onChange: (event: { target: { value: string } }) => { fields.sessionId = event.target.value; update(); } }),
              h(Antd.Button, { size: 'small', disabled: busy, onClick: () => run(load) }, tr('读取会话', 'Load Session')),
              h(Antd.Button, { size: 'small', danger: true, disabled: busy || !session, icon: h(Icons.ClearOutlined), onClick: () => run(async () => {
                if (!session) throw new Error(tr('没有可删除的会话。', 'There is no session to delete.'));
                await context.request('agent.sessions.delete', { sessionId: session.sessionId, expectedRevision: session.revision }, crypto.randomUUID());
                if (!active()) return;
                session = undefined; fields.sessionId = ''; turnJobId = ''; turnPending = false; jobs = []; actions = [];
                waiting = false; unread = false;
                persistConfig(); status = tr('仅会话已删除，daemon 任务和批准动作保持不变。', 'Session deleted. Background jobs and approval actions were not changed.');
              }) }, tr('删除会话', 'Delete Session'))),
            showAdvancedTargets ? h('div', { className: 'mintcat-agent-fields', style: { marginTop: 10 } },
              ...advancedLabels.map(([key, label, numeric]) => h(Antd.Form.Item, { key, label, style: { marginBottom: 6 } },
                h(Antd.Input, { size: 'small', value: fields[key], type: numeric ? 'number' : 'text', min: numeric ? 0 : undefined, disabled: busy, onChange: (event: { target: { value: string } }) => { fields[key] = event.target.value; update(); } })))) : null) }] }),
          h('div', { className: 'mintcat-agent-message-list' },
            !session?.timeline.length ? h('div', { className: 'mintcat-agent-empty' },
              h('div', { className: 'mintcat-agent-empty-orb' }, h(Icons.RobotOutlined)),
              h('div', { className: 'mintcat-agent-empty-title' }, tr('MintCat 智能模组助手', 'MintCat Mod Assistant')),
              h('div', { className: 'mintcat-agent-empty-desc' }, tr('已自动同步当前游戏与配置目标；填入模型与 API Key 后即可创建会话开始对话。', 'The current game and profile are synced automatically. Enter your model and API key, then create a session to start chatting.'))) : null,
            ...(session?.timeline ?? []).map(item => h('div', { key: item.id, className: `mintcat-agent-message mintcat-agent-message-${item.role}` }, h('div', { className: 'mintcat-agent-message-icon' }, h(item.role === 'user' ? Icons.UserOutlined : item.role === 'assistant' ? Icons.RobotOutlined : Icons.ToolOutlined)), h('div', { className: 'mintcat-agent-message-body' }, item.toolName ? h(Antd.Tag, { style: { marginBottom: 4 } }, item.toolName) : null, h('div', { style: { whiteSpace: 'pre-wrap' } }, item.content)))),
            ...actions.map(action => h(Antd.Card, { key: action.actionId, size: 'small', className: 'mintcat-agent-confirmation', title: tr('独立人工批准', 'Separate Manual Approval'), extra: h(Antd.Tag, { color: action.state === 'pending' ? 'warning' : undefined }, approvalStateLabels[action.state] ?? action.state) },
              h(Antd.Typography.Text, { strong: true }, action.kind), h('pre', null, JSON.stringify({ actionId: action.actionId, revision: action.revision, target: action.target, targetRevision: action.targetRevision, planHash: action.planHash, expiresAt: action.expiresAt, responseSchema: action.responseSchema }, null, 2)),
              action.state === 'pending' ? h('div', null, h(Antd.Input.TextArea, { value: responses[action.actionId], 'aria-label': tr(`批准响应 ${action.actionId}`, `Approval response ${action.actionId}`), disabled: busy, onChange: (event: { target: { value: string } }) => { responses[action.actionId] = event.target.value; update(); }, autoSize: { minRows: 2, maxRows: 8 } }), h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.CheckOutlined), disabled: busy, style: { marginTop: 8 }, onClick: () => run(() => approve(action)) }, tr('审核并以人工身份批准', 'Review and Approve Manually'))) : null)),
            jobs.length ? h(Antd.Collapse, { size: 'small', items: jobs.map(job => ({ key: job.jobId, label: `${job.command} · ${jobStateLabels[job.state] ?? job.state}`, children: h('pre', null, JSON.stringify(job, null, 2)) })) }) : null,
            error ? h(Antd.Alert, { type: 'error', showIcon: true, title: error }) : null,
            busy || turnPending || session?.state === 'sending' ? h('div', { className: 'mintcat-agent-loading' }, h(Antd.Spin, { size: 'small' }), tr('正在处理会话及工具任务…', 'Processing the conversation and tool jobs…')) : null,
            h('div', { ref: end })),
          status ? h('div', { role: 'status', className: 'mintcat-agent-status' }, status) : h('span', { role: 'status', className: 'mintcat-agent-status-empty' }),
          h('div', { className: 'mintcat-agent-composer' }, h(Antd.Input.TextArea, { value: prompt, maxLength: 32768, disabled: busy || !session || turnPending || pendingApproval, autoSize: { minRows: 2, maxRows: 5 }, placeholder: pendingApproval ? tr('请先独立审核待批准动作', 'Review the pending approval request first') : tr('输入消息，Shift+Enter 换行…', 'Type a message. Shift+Enter for a new line…'), onChange: (event: { target: { value: string } }) => { prompt = event.target.value; persistPrompt(); update(); }, onPressEnter: (event: { shiftKey: boolean; preventDefault(): void }) => { if (!event.shiftKey) { event.preventDefault(); if (!sendDisabled) run(send); } } }), h(Antd.Button, { type: 'primary', className: 'glass-primary-action mintcat-agent-send-btn', icon: h(Icons.SendOutlined), disabled: sendDisabled, onClick: () => run(send) }, tr('发送', 'Send'))))));
  }
  const view = mountReactView(root, h(View));
  publishIndicator();
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
  return { setOpen, dispose() {
    persistPrompt(); persistConfig(); alive = false; ++loadGeneration; ++bindingsGeneration;
    drawerOpen = false; waiting = false; unread = false; publishIndicator();
    selectionSub(); poll.dispose(); context.dirty(false); view.dispose();
  } };
}
