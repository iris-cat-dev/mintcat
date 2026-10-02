import type { ActionRequired, JobRef, JobSnapshot } from '@mintcat/sdk-ts';
import { getSharedRuntime, mountReactView } from '@mintcat/sdk-ui';
import type { Disposable, Json, UiContext } from '@mintcat/sdk-ui';
import { BackupView } from './view';
import type { BackupModel, PendingRequest, TaskView, TrackedTask } from './view';

type MutationParams = {
  'backup.export': { targetPath: string };
  'backup.restore': { sourcePath: string };
  'backup.recover': Record<string, never>;
  'backup.cloud.upload': { metadata: { createdAt: string; size: number; note?: string } };
  'backup.cloud.download': { backupId: string; targetPath: string };
  'library.import-legacy': { sourcePath: string };
  'library.config-import': { sourcePath: string; format: 'backup' | 'legacy-sqlite' };
};
const mutationMethods: Record<keyof MutationParams, true> = {
  'backup.export': true, 'backup.restore': true, 'backup.recover': true,
  'backup.cloud.upload': true, 'backup.cloud.download': true,
  'library.import-legacy': true, 'library.config-import': true,
};

/** The React surface observes durable daemon jobs; unmounting never cancels them. */
export async function mountBackupPage(root: HTMLElement, context: UiContext): Promise<Disposable> {
  let alive = true;
  let pollPending = false;
  let refreshGeneration = 0;
  let pageGeneration = 0;
  let revision = 0;
  const listeners = new Set<() => void>();
  const active = (): boolean => alive && !context.signal.aborted;
  const changed = (): void => {
    if (!active()) return;
    ++revision;
    for (const listener of listeners) listener();
  };
  // Keep the state versions and original request keys across view reattachment.
  const saved = context.state('paths', 1, { output: '', source: '', note: '', legacySource: '', configSource: '', configFormat: 'backup' });
  const fields = {
    output: saved.output ?? '', source: saved.source ?? '', note: saved.note ?? '',
    legacySource: saved.legacySource ?? '', configSource: saved.configSource ?? '',
    configFormat: saved.configFormat === 'legacy-sqlite' ? 'legacy-sqlite' as const : 'backup' as const,
  };
  const tasks = context.state('tasks', 1, {
    jobs: [] as TrackedTask[], pending: [] as PendingRequest[], credentials: {} as Record<string, boolean>,
  });
  const persistTasks = (): void => { if (active()) context.saveState('tasks', 1, tasks); };
  const persistFields = (): void => {
    if (!active()) return;
    context.saveState('paths', 1, fields);
    context.dirty(fields.note.trim().length > 0);
  };
  const describeError = (error: unknown): string => {
    const detail = typeof error === 'object' && error !== null && 'appError' in error
      ? JSON.stringify(error.appError, null, 2) ?? String(error)
      : error instanceof Error ? error.message : typeof error === 'string' ? error : JSON.stringify(error, null, 2) ?? String(error);
    return /permission|forbidden|unauthorized|access.denied/i.test(detail)
      ? `权限不足，请检查当前模块授权。\n${detail}` : `操作失败：${detail}`;
  };
  const required = (key: 'output' | 'source' | 'legacySource' | 'configSource'): string => {
    const labels = { output: '输出归档路径', source: '待恢复归档路径', legacySource: '旧版 SQLite 路径', configSource: '配置快照路径' };
    const value = fields[key].trim();
    if (!value) throw new Error(`请填写${labels[key]}`);
    return value;
  };
  const run = context.guard(async (name: string, action: () => Promise<void>) => {
    if (model.busy || !active()) return;
    model.busy = name;
    const generation = pageGeneration;
    model.status = ''; model.statusError = false; changed();
    try { await action(); }
    catch (error) {
      if (active() && generation === pageGeneration) { model.status = describeError(error); model.statusError = true; changed(); }
    } finally {
      if (active()) { model.busy = ''; changed(); }
    }
  });
  const track = (job: JobRef, targetPath = ''): void => {
    if (!active()) return;
    if (!tasks.jobs.some(task => task.jobId === job.jobId)) tasks.jobs.push({ jobId: job.jobId, actionId: '', targetPath });
    persistTasks(); changed();
  };
  const resolve = async (request: PendingRequest): Promise<void> => {
    const generation = pageGeneration;
    try {
      const job = await context.request<JobRef>(request.method, request.params, request.key);
      if (!active() || generation !== pageGeneration) return;
      track(job, request.targetPath);
      tasks.pending = tasks.pending.filter(item => item.key !== request.key);
      persistTasks();
      model.status = `已登记后台任务 ${job.jobId}，请在下方查看持久状态与输出。`; changed();
    } catch (error) {
      if (!active() || generation !== pageGeneration) return;
      if (typeof error === 'object' && error !== null && 'appError' in error) {
        const appError = error.appError as { jobId?: string };
        if (appError.jobId) track({ jobId: appError.jobId }, request.targetPath);
        tasks.pending = tasks.pending.filter(item => item.key !== request.key);
        persistTasks(); changed();
      }
      // An ambiguous transport failure must retain its original idempotency key.
      throw error;
    }
  };
  const submit = async <M extends keyof MutationParams>(method: M, params: MutationParams[M], targetPath = ''): Promise<void> => {
    if (!active()) return;
    const request: PendingRequest = { key: crypto.randomUUID(), method, params, targetPath };
    tasks.pending.push(request); persistTasks(); changed();
    await resolve(request); await poll();
  };
  const refresh = async (): Promise<void> => {
    if (!active()) return;
    const generation = ++refreshGeneration;
    const page = pageGeneration;
    model.historyLoading = true; model.historyError = ''; changed();
    try {
      const records = await context.query('cloud-history', () => context.request<BackupModel['records']>('backup.cloud.list', {}));
      if (!active() || generation !== refreshGeneration || page !== pageGeneration) return;
      model.records = records;
    } catch (error) {
      if (active() && generation === refreshGeneration && page === pageGeneration) {
        model.records = []; model.historyError = describeError(error);
      }
    } finally {
      if (active() && generation === refreshGeneration && page === pageGeneration) { model.historyLoading = false; changed(); }
    }
  };
  const approve = async (task: TrackedTask, reviewed: ActionRequired, importCredentials: boolean): Promise<void> => {
    const generation = pageGeneration;
    const current = await context.call('actions.get', { actionId: reviewed.actionId });
    if (!active() || generation !== pageGeneration) return;
    if (current.state !== 'pending' || JSON.stringify(current) !== JSON.stringify(reviewed)) {
      await poll(); throw new Error('批准对象已发生变化或不再等待批准，请重新审阅当前后台操作。');
    }
    if (current.kind !== 'backup-owner-import' || task.actionId !== current.actionId) throw new Error('该操作不是刚刚审阅的备份导入。');
    if (!window.confirm(`是否独立批准将此冻结快照导入当前空的 Library？\n导入旧版凭据：${importCredentials ? '是（已明确勾选）' : '否'}\n\n${JSON.stringify(current, null, 2)}`)) return;
    if (!active() || generation !== pageGeneration) return;
    await context.call('actions.approve', {
      actionId: current.actionId, expectedRevision: current.revision, response: { approved: true, importCredentials },
    }, crypto.randomUUID());
    if (!active() || generation !== pageGeneration) return;
    model.status = `已提交任务 ${task.jobId} 的批准；后台任务尚不一定完成。`; changed();
    await poll();
  };
  const viewFor = (task: TrackedTask): TaskView => {
    let view = model.views.get(task.jobId);
    if (!view) { view = { afterSeq: 0, events: [], error: '' }; model.views.set(task.jobId, view); }
    return view;
  };
  const poll = async (): Promise<void> => {
    if (!active() || pollPending) return;
    pollPending = true;
    const generation = pageGeneration;
    try {
      for (const task of tasks.jobs.slice()) {
        const view = viewFor(task);
        try {
          let afterSeq = view.afterSeq;
          let snapshot: JobSnapshot = await context.call('jobs.get', { jobId: task.jobId });
          if (!active() || generation !== pageGeneration) return;
          const events: TaskView['events'] = [];
          for (;;) {
            const result = await context.call('jobs.watch', { jobId: task.jobId, afterSeq, limit: 200 });
            if (!active() || generation !== pageGeneration) return;
            snapshot = result.snapshot; events.push(...result.events);
            for (const event of result.events) if (typeof event.details?.actionId === 'string') task.actionId = event.details.actionId;
            if (result.nextAfterSeq >= snapshot.lastSeq || result.nextAfterSeq <= afterSeq) { afterSeq = result.nextAfterSeq; break; }
            afterSeq = result.nextAfterSeq;
          }
          const previous = view.snapshot;
          view.afterSeq = afterSeq; view.snapshot = snapshot; view.events.push(...events); view.error = '';
          if (snapshot.state === 'awaiting-approval' && task.actionId) {
            const action = await context.call('actions.get', { actionId: task.actionId });
            if (!active() || generation !== pageGeneration) return;
            view.action = action;
            if (previous?.state !== snapshot.state) model.expandedJobs.add(task.jobId);
          } else {
            view.action = undefined;
          }
          if (snapshot.state === 'succeeded' && previous?.state !== 'succeeded' && snapshot.command === 'backup.cloud.upload') {
            context.invalidate('cloud-history'); await refresh();
            if (!active() || generation !== pageGeneration) return;
          }
          persistTasks(); changed();
        } catch (error) {
          if (active() && generation === pageGeneration) { view.error = describeError(error); changed(); }
        }
      }
    } finally { pollPending = false; }
  };
  const model: BackupModel = {
    fields, tasks, records: [], views: new Map(), expandedJobs: new Set(), busy: '', status: '', statusError: false,
    historyLoading: false, historyError: '', existingJob: '',
    subscribe: listener => { listeners.add(listener); return () => { listeners.delete(listener); }; },
    getRevision: () => revision,
    updateField: (key, value) => { if (active()) { Object.assign(fields, { [key]: value }); persistFields(); changed(); } },
    updateExistingJob: value => { if (active()) { model.existingJob = value; changed(); } },
    expandJobs: keys => { if (active()) { model.expandedJobs = new Set(keys); changed(); } },
    setCredentials: (actionId, value) => { if (active()) { tasks.credentials[actionId] = value; persistTasks(); changed(); } },
    perform: run,
    exportArchive: () => { const targetPath = required('output'); return submit('backup.export', { targetPath }, targetPath); },
    restoreArchive: () => submit('backup.restore', { sourcePath: required('source') }),
    importLegacy: () => submit('library.import-legacy', { sourcePath: required('legacySource') }),
    importConfig: () => submit('library.config-import', { sourcePath: required('configSource'), format: fields.configFormat }),
    upload: () => submit('backup.cloud.upload', { metadata: { createdAt: new Date().toISOString(), size: 0, ...(fields.note.trim() ? { note: fields.note.trim() } : {}) } }),
    refresh: async () => { context.invalidate('cloud-history'); await refresh(); },
    download: backupId => { const targetPath = required('output'); return submit('backup.cloud.download', { backupId, targetPath }, targetPath); },
    recover: () => submit('backup.recover', {}),
    resolvePending: async request => { await resolve(request); await poll(); },
    approve,
    restoreDownload: task => submit('backup.restore', { sourcePath: task.targetPath }),
    poll,
    attach: async () => {
      const generation = pageGeneration;
      const jobId = model.existingJob.trim(); if (!jobId) throw new Error('请填写后台任务 ID');
      const snapshot = await context.call('jobs.get', { jobId });
      if (!active() || generation !== pageGeneration) return;
      if (!Object.prototype.hasOwnProperty.call(mutationMethods, snapshot.command)) throw new Error('该任务不是备份或 Library 导入操作');
      track({ jobId: snapshot.jobId }); await poll();
    },
  };
  const { React } = getSharedRuntime();
  const mounted = mountReactView(root, React.createElement(BackupView, { model }));
  persistFields();
  const timer = context.interval(poll, 1_000);
  const generationSubscription = context.onGenerationChanged(async () => {
    if (!active()) return;
    ++pageGeneration;
    context.invalidate('cloud-history'); await Promise.all([refresh(), poll()]);
  });
  context.guard(async () => { await Promise.all([refresh(), poll()]); })();
  return {
    dispose() {
      if (!alive) return;
      persistFields(); persistTasks(); alive = false; ++pageGeneration; ++refreshGeneration;
      timer.dispose(); generationSubscription.dispose(); listeners.clear(); context.dirty(false); mounted.dispose();
    },
  };
}
