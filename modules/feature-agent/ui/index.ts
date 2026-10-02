import type { ActionRequired, JobSnapshot } from '@mintcat/sdk-ts';
import { getSharedRuntime, mountReactView } from '@mintcat/sdk-ui';
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
  const active = (): boolean => alive && !context.signal.aborted;
  const saved = context.state('conversation', 1, { sessionId: '', turnJobId: '' });
  let session: Session | undefined, turnJobId = saved.turnJobId ?? '', turnPending = Boolean(turnJobId);
  let prompt = context.state<string>('prompt', 1, ''), error = '', status = '';
  const fields: Record<string, string> = { baseUrl: 'https://api.openai.com/v1', model: '', credentialRef: '', gameId: '', profileId: '', profileRevision: '0', contentRevision: '0', installationId: '', installationRevision: '0', sessionId: saved.sessionId };
  let jobs: JobSnapshot[] = [], actions: ActionRequired[] = [];
  const responses: Record<string, string> = {};
  let redraw = (): void => {};
  const update = (): void => { if (active()) redraw(); };
  const persistPrompt = (): void => { context.saveState('prompt', 1, prompt); context.dirty(Boolean(prompt)); };
  context.dirty(Boolean(prompt));
  const findActions = (value: unknown, ids: Set<string>): void => {
    if (Array.isArray(value)) { for (const item of value) findActions(item, ids); }
    else if (value && typeof value === 'object') for (const [key, item] of Object.entries(value)) {
      if (key === 'actionId' && typeof item === 'string') ids.add(item); else findActions(item, ids);
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
    context.saveState('conversation', 1, { sessionId: next.sessionId, turnJobId });
    status = `${next.sessionId} · 版本 ${next.revision} · ${next.state}${next.errorCode ? ` · ${next.errorCode}` : ''}`;
    update(); await renderBindings();
  };
  const run = (action: () => Promise<void>): void => {
    if (busy || !active()) return;
    ++loadGeneration; ++bindingsGeneration; busy = true; error = ''; update();
    context.guard(async () => {
      try { await action(); }
      catch (cause) { if (active()) { error = typeof cause === 'object' && cause !== null && 'appError' in cause ? JSON.stringify(cause.appError, null, 2) ?? String(cause) : cause instanceof Error ? cause.message : JSON.stringify(cause, null, 2) ?? String(cause); update(); } throw cause; }
      finally { if (active()) { busy = false; update(); } }
    })();
  };
  const revision = (key: string): number => {
    const raw = fields[key].trim(), value = Number(raw);
    if (!raw || !Number.isSafeInteger(value) || value < 0) throw new Error('请显式填写非负整数版本，不能猜测目标版本。');
    return value;
  };
  const create = async (): Promise<void> => {
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
      const latest = context.state('conversation', 1, { sessionId: current.sessionId, turnJobId: '' });
      if (latest.sessionId === current.sessionId) context.saveState('conversation', 1, { sessionId: current.sessionId, turnJobId: job.jobId });
      if (context.state('prompt', 1, '') === submitted) context.saveState('prompt', 1, '');
      if (!active()) return;
      turnJobId = job.jobId; turnPending = true;
      if (prompt === submitted) { prompt = ''; persistPrompt(); }
      await render(await context.request<Session>('agent.sessions.get', { sessionId: current.sessionId }));
    } catch (cause) {
      if (!active()) return;
      // Failed transports can still persist the turn and sanitized provider failure.
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
    // Independent authenticated UI approval, never an Agent session/model tool.
    const job = await context.call('actions.approve', { actionId: current.actionId, expectedRevision: current.revision, response }, crypto.randomUUID());
    if (!active()) return;
    status = `人工批准已受理 · ${job.jobId}`; await renderBindings();
  };
  const labels: Array<[string, string, boolean?]> = [['baseUrl', 'OpenAI 兼容服务地址'], ['model', '模型'], ['credentialRef', 'daemon 凭据引用（不是 Token）'], ['gameId', '游戏 ID'], ['profileId', '配置 ID'], ['profileRevision', '配置版本', true], ['contentRevision', '内容版本', true], ['installationId', '安装位置 ID'], ['installationRevision', '安装位置版本', true]];
  function View() {
    const [, setTick] = React.useState(0); redraw = () => setTick(value => value + 1);
    const end = React.useRef<HTMLDivElement>(null);
    React.useEffect(() => { end.current?.scrollIntoView({ block: 'nearest' }); }, [session?.timeline.length]);
    const pendingApproval = actions.some(action => action.state === 'pending');
    const sendDisabled = busy || !session || session.state === 'sending' || turnPending || pendingApproval || !prompt.trim();
    return h('div', { className: 'mintcat-agent-surface' },
      !drawerOpen ? h(Antd.Button, { icon: h(Icons.RobotOutlined), onClick: () => { drawerOpen = true; update(); } }, '打开 Agent 助手') : null,
      h(Antd.Drawer, { open: drawerOpen, placement: 'right', size: 680, getContainer: false, mask: false, onClose: () => { drawerOpen = false; update(); }, title: h('div', null, 'Agent 助手', h('div', null, h(Antd.Typography.Text, { type: 'secondary', style: { fontSize: 12 } }, '对话、工具与独立人工批准'))), styles: { body: { padding: 0, overflow: 'hidden' } }, extra: h(Antd.Button, { icon: h(Icons.ReloadOutlined), disabled: busy || !session, onClick: () => run(async () => { if (session) await render(await context.request<Session>('agent.sessions.get', { sessionId: session.sessionId })); }) }, '刷新') },
        h('div', { className: 'mintcat-agent-page' },
          h(Antd.Collapse, { size: 'small', items: [{ key: 'settings', label: '模型及明确操作目标', children: h('div', null,
            h(Antd.Alert, { type: 'info', showIcon: true, title: '模型工具不能批准或安装；任务由 daemon 持有，关闭对话不会取消任务。' }),
            h('div', { className: 'mintcat-agent-fields' }, ...labels.map(([key, label, numeric]) => h(Antd.Form.Item, { key, label, style: { marginBottom: 8 } }, h(Antd.Input, { value: fields[key], type: numeric ? 'number' : 'text', min: numeric ? 0 : undefined, disabled: busy, onChange: (event: { target: { value: string } }) => { fields[key] = event.target.value; update(); } })))),
            h(Antd.Space, { wrap: true }, h(Antd.Button, { type: 'primary', disabled: busy, onClick: () => run(create) }, '创建会话'), h(Antd.Input, { value: fields.sessionId, placeholder: '已有会话 ID', disabled: busy, onChange: (event: { target: { value: string } }) => { fields.sessionId = event.target.value; update(); } }), h(Antd.Button, { disabled: busy, onClick: () => run(load) }, '读取会话'), h(Antd.Button, { danger: true, disabled: busy || !session, icon: h(Icons.ClearOutlined), onClick: () => run(async () => {
              if (!session) throw new Error('没有可删除的会话。');
              await context.request('agent.sessions.delete', { sessionId: session.sessionId, expectedRevision: session.revision }, crypto.randomUUID());
              if (!active()) return;
              session = undefined; fields.sessionId = ''; turnJobId = ''; turnPending = false; jobs = []; actions = [];
              context.saveState('conversation', 1, { sessionId: '', turnJobId: '' }); status = '仅会话已删除，daemon 任务和批准动作保持不变。';
            }) }, '删除会话'))) }] }),
          h('div', { className: 'mintcat-agent-message-list' },
            !session?.timeline.length ? h('div', { className: 'mintcat-agent-empty' }, h(Icons.RobotOutlined, { style: { fontSize: 44, color: 'var(--ant-color-primary)' } }), h(Antd.Empty, { description: '配置模型与目标后开始对话。' })) : null,
            ...(session?.timeline ?? []).map(item => h('div', { key: item.id, className: `mintcat-agent-message mintcat-agent-message-${item.role}` }, h('div', { className: 'mintcat-agent-message-icon' }, h(item.role === 'user' ? Icons.UserOutlined : item.role === 'assistant' ? Icons.RobotOutlined : Icons.ToolOutlined)), h('div', { className: 'mintcat-agent-message-body' }, item.toolName ? h(Antd.Tag, null, item.toolName) : null, h('div', { style: { whiteSpace: 'pre-wrap' } }, item.content)))),
            ...actions.map(action => h(Antd.Card, { key: action.actionId, size: 'small', className: 'mintcat-agent-confirmation', title: '独立人工批准', extra: h(Antd.Tag, { color: action.state === 'pending' ? 'warning' : undefined }, action.state) },
              h(Antd.Typography.Text, { strong: true }, action.kind), h('pre', null, JSON.stringify({ actionId: action.actionId, revision: action.revision, target: action.target, targetRevision: action.targetRevision, planHash: action.planHash, expiresAt: action.expiresAt, responseSchema: action.responseSchema }, null, 2)),
              action.state === 'pending' ? h('div', null, h(Antd.Input.TextArea, { value: responses[action.actionId], 'aria-label': `批准响应 ${action.actionId}`, disabled: busy, onChange: (event: { target: { value: string } }) => { responses[action.actionId] = event.target.value; update(); }, autoSize: { minRows: 2, maxRows: 8 } }), h(Antd.Button, { type: 'primary', icon: h(Icons.CheckOutlined), disabled: busy, style: { marginTop: 8 }, onClick: () => run(() => approve(action)) }, '审核并以人工身份批准')) : null)),
            jobs.length ? h(Antd.Collapse, { size: 'small', items: jobs.map(job => ({ key: job.jobId, label: `${job.command} · ${job.state}`, children: h('pre', null, JSON.stringify(job, null, 2)) })) }) : null,
            error ? h(Antd.Alert, { type: 'error', showIcon: true, title: error }) : null,
            busy || turnPending || session?.state === 'sending' ? h('div', { className: 'mintcat-agent-loading' }, h(Antd.Spin, { size: 'small' }), '正在处理会话及工具任务…') : null,
            h('div', { ref: end })),
          h(Antd.Typography.Text, { type: 'secondary', role: 'status', className: 'mintcat-agent-status' }, status),
          h('div', { className: 'mintcat-agent-composer' }, h(Antd.Input.TextArea, { value: prompt, maxLength: 32768, disabled: busy || !session || turnPending || pendingApproval, autoSize: { minRows: 2, maxRows: 5 }, placeholder: pendingApproval ? '请先独立审核待批准动作' : '输入消息，Shift+Enter 换行', onChange: (event: { target: { value: string } }) => { prompt = event.target.value; persistPrompt(); update(); }, onPressEnter: (event: { shiftKey: boolean; preventDefault(): void }) => { if (!event.shiftKey) { event.preventDefault(); if (!sendDisabled) run(send); } } }), h(Antd.Button, { type: 'primary', icon: h(Icons.SendOutlined), disabled: sendDisabled, onClick: () => run(send) }, '发送')))));
  }
  const view = mountReactView(root, h(View));
  if (saved.sessionId) run(load);
  const poll = context.interval(async () => {
    if (busy || !session || !active()) return;
    const current = session.sessionId, generation = ++loadGeneration;
    try {
      const next = await context.request<Session>('agent.sessions.get', { sessionId: current });
      if (active() && generation === loadGeneration && session?.sessionId === current) await render(next);
    } catch (cause) { if (active() && generation === loadGeneration) { error = typeof cause === 'object' && cause !== null && 'appError' in cause ? JSON.stringify(cause.appError, null, 2) ?? String(cause) : cause instanceof Error ? cause.message : JSON.stringify(cause, null, 2) ?? String(cause); update(); } }
  }, 2000);
  return { dispose() { persistPrompt(); alive = false; ++loadGeneration; ++bindingsGeneration; poll.dispose(); context.dirty(false); view.dispose(); } };
}
