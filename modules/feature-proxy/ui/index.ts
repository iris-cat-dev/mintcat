import { getSharedRuntime, mountReactView } from '@mintcat/sdk-ui';
import type { Disposable, UiContext } from '@mintcat/sdk-ui';
import type { ActionRequired, JobEvent, JobRef } from '@mintcat/sdk-ts';
import { ProxyView } from './view';
import type { ProxyModel, ProxyOperation } from './view';

type PrivilegeOperation = { operation: 'create-credentials' | 'install-certificate' | 'remove-certificate' | 'apply-hosts' | 'cleanup-hosts' } | { operation: 'authorize-listen-port'; port: number };

export async function mountProxyPage(root: HTMLElement, context: UiContext): Promise<Disposable> {
  let disposed = false;
  let polling = false;
  let busy = false;
  const active = (): boolean => !disposed && !context.signal.aborted;
  const jobs = context.state('jobs', 1, [] as string[]);
  let model: ProxyModel = {
    status: null, loading: true, busy: null, error: '', result: '', tasks: [],
    digest: context.state('digest', 1, ''), port: context.state('port', 1, 443),
    offline: context.state('offline', 1, false), existing: '',
  };
  const listeners = new Set<(value: ProxyModel) => void>();
  const update = (value: Partial<ProxyModel>): void => {
    if (!active()) return;
    model = { ...model, ...value };
    for (const listener of listeners) listener(model);
  };
  const errorText = (error: unknown): string => {
    const message = error instanceof Error ? error.message : typeof error === 'string' ? error : JSON.stringify(error);
    return `操作失败：${message}。若提示 permission / 权限不足，请检查守护进程授予的模块权限；系统修改仍须独立人工批准及原生系统授权。`;
  };
  const save = (): void => {
    context.saveState('digest', 1, model.digest.trim());
    context.saveState('port', 1, model.port);
    context.saveState('offline', 1, model.offline);
  };
  const track = (job: JobRef): void => {
    const latest = context.state('jobs', 1, [] as string[]);
    if (!latest.includes(job.jobId)) { latest.push(job.jobId); context.saveState('jobs', 1, latest); }
    if (active() && !jobs.includes(job.jobId)) jobs.push(job.jobId);
  };
  const submit = async (method: string, params: Record<string, unknown>): Promise<JobRef> => {
    const job = await context.request<JobRef>(method, params, crypto.randomUUID());
    track(job);
    return job;
  };
  const privileged = (operation: PrivilegeOperation): Promise<JobRef> => submit('proxy.privilege', { digest: model.digest.trim(), operation });
  const refresh = async (): Promise<void> => {
    if (polling || !active()) return;
    polling = true;
    try {
      const status = await context.request<NonNullable<ProxyModel['status']>>('proxy.status', {});
      if (!active()) return;
      update({ status });
      for (const jobId of jobs.slice()) {
        if (!active()) return;
        try {
          let afterSeq = 0;
          let actionId = '';
          let page = await context.call('jobs.watch', { jobId, afterSeq, limit: 200 });
          const events: JobEvent[] = [];
          for (;;) {
            if (!active()) return;
            events.push(...page.events);
            if (events.length > 200) events.splice(0, events.length - 200);
            for (const event of page.events) if (typeof event.details?.actionId === 'string') actionId = event.details.actionId;
            if (page.nextAfterSeq >= page.snapshot.lastSeq || page.nextAfterSeq <= afterSeq) break;
            afterSeq = page.nextAfterSeq;
            page = await context.call('jobs.watch', { jobId, afterSeq, limit: 200 });
          }
          const reviewed = actionId ? await context.call('actions.get', { actionId }) : undefined;
          if (!active()) return;
          const task = { snapshot: page.snapshot, reviewed, events };
          const tasks = model.tasks.filter(item => item.snapshot.jobId !== jobId);
          tasks.push(task);
          update({ tasks });
        } catch (error) {
          if (active()) { const message = `代理任务 ${jobId}：${errorText(error)}`; update({ error: message }); context.notify(message); }
        }
      }
    } catch (error) {
      if (active()) update({ error: errorText(error) });
    } finally {
      polling = false;
      update({ loading: false });
    }
  };
  const run = async (key: string, operation: () => Promise<unknown>): Promise<void> => {
    if (busy || !active()) return;
    busy = true;
    update({ busy: key, error: '' });
    try {
      save();
      const value = await operation();
      if (active()) { update({ result: JSON.stringify(value, null, 2) ?? '' }); await refresh(); }
    } catch (error) {
      if (active()) update({ error: errorText(error) });
    } finally {
      busy = false;
      update({ busy: null });
    }
  };
  const approve = async (reviewed: ActionRequired, jobId: string): Promise<unknown> => {
    const current = await context.call('actions.get', { actionId: reviewed.actionId });
    if (!active()) return;
    if (current.state !== 'pending' || current.kind !== 'proxy-system-operation' || current.operationId !== `proxy:${jobId}` || JSON.stringify(current) !== JSON.stringify(reviewed)) throw new Error('代理批准内容已变化；请刷新并重新审核当前不可变目标');
    const response = { approved: true };
    if (!window.confirm(`作为人工审核者，批准此精确的代理系统操作？\n\n${JSON.stringify({ action: current, response }, null, 2)}`)) return;
    const job = await context.call('actions.approve', { actionId: current.actionId, expectedRevision: current.revision, response }, crypto.randomUUID());
    track(job);
    return job;
  };
  const operations: Record<ProxyOperation, () => Promise<unknown>> = {
    start: () => submit('proxy.start', { digest: model.digest.trim(), options: { port: model.port, offline: model.offline, bind: '127.0.0.1' } }),
    stop: () => submit('proxy.stop', {}),
    credentials: () => privileged({ operation: 'create-credentials' }),
    installCertificate: () => privileged({ operation: 'install-certificate' }),
    removeCertificate: () => privileged({ operation: 'remove-certificate' }),
    cleanupHosts: () => privileged({ operation: 'cleanup-hosts' }),
    applyHosts: () => privileged({ operation: 'apply-hosts' }),
    authorizePort: () => privileged({ operation: 'authorize-listen-port', port: model.port }),
    attach: async () => {
      const snapshot = await context.call('jobs.get', { jobId: model.existing.trim() });
      if (!active()) return;
      if (!snapshot.command.startsWith('proxy.')) throw new Error('此任务不是代理操作');
      track(snapshot);
      return snapshot;
    },
    refresh,
  };
  if (!active()) return { dispose() {} };
  const { React } = getSharedRuntime();
  const view = mountReactView(root, React.createElement(ProxyView, {
    initial: model,
    subscribe(listener: (value: ProxyModel) => void) {
      listeners.add(listener);
      if (active()) listener(model);
      return () => { listeners.delete(listener); };
    },
    change(value: Partial<Pick<ProxyModel, 'digest' | 'port' | 'offline' | 'existing'>>) { update(value); if (active()) save(); },
    operate: context.guard((operation: ProxyOperation) => run(operation, operations[operation])),
    approve: context.guard((reviewed: ActionRequired, jobId: string) => run(`approve:${jobId}`, () => approve(reviewed, jobId))),
  }));
  const timer = context.interval(refresh, 2000);
  const disposable = { dispose() {
    if (disposed) return;
    disposed = true;
    context.signal.removeEventListener('abort', disposable.dispose);
    timer.dispose();
    listeners.clear();
    view.dispose();
  } };
  context.signal.addEventListener('abort', disposable.dispose, { once: true });
  if (context.signal.aborted) disposable.dispose(); else void refresh();
  return disposable;
}
