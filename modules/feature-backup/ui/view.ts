import type { ActionRequired, JobEvent, JobSnapshot } from '@mintcat/sdk-ts';
import type { Json } from '@mintcat/sdk-ui';
import { getSharedRuntime } from '@mintcat/sdk-ui';
import type { TableColumnsType } from 'antd';
import type { ReactElement } from 'react';

type BackupMethod = 'backup.export' | 'backup.restore' | 'backup.recover' | 'backup.cloud.upload' | 'backup.cloud.download' | 'library.import-legacy' | 'library.config-import';
export type TrackedTask = { jobId: string; actionId: string; targetPath: string };
export type PendingRequest = { key: string; method: BackupMethod; params: Record<string, Json>; targetPath: string };
export type CloudBackupRecord = { id: string; createdAt: string; size: number; checksum?: string; appVersion?: string; note?: string };
export type TaskView = { afterSeq: number; events: JobEvent[]; error: string; snapshot?: JobSnapshot; action?: ActionRequired };
type Fields = { output: string; source: string; note: string; legacySource: string; configSource: string; configFormat: 'backup' | 'legacy-sqlite' };
export interface BackupModel {
  fields: Fields;
  tasks: { jobs: TrackedTask[]; pending: PendingRequest[]; credentials: Record<string, boolean> };
  records: CloudBackupRecord[];
  views: Map<string, TaskView>;
  expandedJobs: Set<string>;
  busy: string;
  status: string;
  statusError: boolean;
  historyLoading: boolean;
  historyError: string;
  existingJob: string;
  subscribe(listener: () => void): () => void;
  getRevision(): number;
  updateField(key: keyof Fields, value: string): void;
  updateExistingJob(value: string): void;
  expandJobs(keys: string[]): void;
  setCredentials(actionId: string, value: boolean): void;
  perform(name: string, action: () => Promise<void>): void;
  exportArchive(): Promise<void>;
  restoreArchive(): Promise<void>;
  importLegacy(): Promise<void>;
  importConfig(): Promise<void>;
  upload(): Promise<void>;
  refresh(): Promise<void>;
  download(backupId: string): Promise<void>;
  recover(): Promise<void>;
  resolvePending(request: PendingRequest): Promise<void>;
  approve(task: TrackedTask, reviewed: ActionRequired, importCredentials: boolean): Promise<void>;
  restoreDownload(task: TrackedTask): Promise<void>;
  poll(): Promise<void>;
  attach(): Promise<void>;
}
const stateLabels: Record<string, string> = {
  queued: '排队中', running: '执行中', 'awaiting-approval': '等待人工批准', succeeded: '已完成', failed: '失败', cancelled: '已取消',
};
const commandLabels: Record<BackupMethod, string> = {
  'backup.export': '离线导出', 'backup.restore': '归档恢复', 'backup.recover': '维护恢复',
  'backup.cloud.upload': '云端上传', 'backup.cloud.download': '云端下载',
  'library.import-legacy': '旧版 Library 导入', 'library.config-import': '配置导入',
};
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return '-';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; ++unit; }
  return `${value.toFixed(2)} ${units[unit]}`;
}
function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value || '-' : date.toLocaleString();
}

export function BackupView({ model }: { model: BackupModel }) {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  const { Alert, Button, Card, Checkbox, Collapse, Flex, Input, Select, Space, Table, Tag, Typography } = Antd;
  React.useSyncExternalStore(model.subscribe, model.getRevision, model.getRevision);
  const [showCloudDetails, setShowCloudDetails] = React.useState(false);

  const icon = (name: keyof typeof Icons) => h(Icons[name] as typeof Icons.CloudOutlined);
  const actionButton = (name: string, label: string, action: () => Promise<void>, options: { primary?: boolean; icon?: ReactElement; text?: boolean; small?: boolean } = {}) => h(Button, {
    type: options.primary ? 'primary' : options.text ? 'text' : 'default',
    size: options.text || options.small ? 'small' : 'middle',
    className: options.primary ? 'glass-primary-action' : undefined,
    icon: options.icon,
    disabled: !!model.busy,
    loading: model.busy === name,
    onClick: () => model.perform(name, action),
  }, label);
  const pathInput = (key: Exclude<keyof Fields, 'configFormat'>, label: string, placeholder: string) => h(Input, {
    value: model.fields[key], 'aria-label': label, placeholder,
    onChange: (event: { target: { value: string } }) => model.updateField(key, event.target.value),
  });
  const code = (value: unknown) => h('pre', { className: 'mintcat-backup-json' }, JSON.stringify(value, null, 2));
  const error = (message: string) => message ? h(Alert, { type: 'error', showIcon: true, message, role: 'alert', className: 'mintcat-backup-feedback' }) : null;

  const columns: TableColumnsType<CloudBackupRecord> = [
    { title: '创建时间', dataIndex: 'createdAt', key: 'createdAt', width: 172, render: (value: string) => formatDate(value) },
    { title: '体积', dataIndex: 'size', key: 'size', width: 92, render: (value: number) => formatBytes(value) },
    { title: '版本', dataIndex: 'appVersion', key: 'appVersion', width: 84, render: (value?: string) => value ? h(Tag, { color: 'purple' }, value) : '-' },
    { title: '备注', dataIndex: 'note', key: 'note', ellipsis: true, render: (value?: string) => value || '-' },
    { title: '操作', key: 'actions', width: 108, render: (_, record) => actionButton(`download:${record.id}`, '下载', () => model.download(record.id), { text: true, icon: icon('DownloadOutlined') }) },
  ];

  const jobItems = model.tasks.jobs.map(task => {
    const view = model.views.get(task.jobId);
    const snapshot = view?.snapshot;
    const action = view?.action;
    const state = snapshot?.state;
    return {
      key: task.jobId,
      label: h(Space, { size: 8, wrap: true },
        h(Typography.Text, { strong: true }, snapshot ? commandLabels[snapshot.command as BackupMethod] ?? snapshot.command : '后台任务'),
        h(Tag, { color: state === 'succeeded' ? 'success' : state === 'failed' ? 'error' : state === 'awaiting-approval' ? 'warning' : 'processing' }, state ? stateLabels[state] ?? state : '读取中'),
        h(Typography.Text, { type: 'secondary', code: true }, task.jobId)),
      children: h(Flex, { vertical: true, gap: 12 },
        error(view?.error ?? ''),
        code({ targetPath: task.targetPath || undefined, snapshot }),
        h(Collapse, { size: 'small', items: [{ key: 'events', label: `任务事件（${view?.events.length ?? 0}）`, children: code(view?.events ?? []) }] }),
        state === 'awaiting-approval' && !action ? h(Alert, { type: 'warning', showIcon: true, message: '保留的事件中尚无批准操作 ID，不会猜测或绕过人工批准。' }) : null,
        action ? h(Card, { size: 'small', title: h(Space, null, icon('SafetyCertificateOutlined'), '独立人工批准') },
          h(Typography.Paragraph, { type: 'secondary' }, '请核对冻结输入、schema、模块依赖与写入栅栏。批准前会再次读取并逐项比较当前操作，提交 expectedRevision；确认不是任务完成。'),
          code(action),
          action.state === 'pending' && action.kind === 'backup-owner-import' ? h(Flex, { vertical: true, gap: 12 },
            h(Checkbox, {
              checked: model.tasks.credentials[action.actionId] === true,
              onChange: (event: { target: { checked: boolean } }) => model.setCredentials(action.actionId, event.target.checked),
            }, '明确导入旧版输入中的凭据（便携归档不包含凭据）'),
            actionButton(`approve:${action.actionId}`, '批准已审阅的冻结导入', () => model.approve(task, action, model.tasks.credentials[action.actionId] === true), { primary: true })) : null) : null,
        state === 'succeeded' && snapshot?.command === 'backup.cloud.download' && task.targetPath
          ? h(Flex, { vertical: true, gap: 8 },
            h(Typography.Text, { type: 'secondary' }, `已下载至 ${task.targetPath}。恢复将创建新的导入任务，仍需独立人工批准。`),
            actionButton(`restore:${task.jobId}`, '请求恢复已下载的归档', () => model.restoreDownload(task), { icon: icon('RollbackOutlined') })) : null),
    };
  });

  const hasActiveOrPendingTasks = model.tasks.pending.length > 0 || jobItems.length > 0;
  const cloudUnconfigured = model.historyError.includes('云备份未配置') && model.records.length === 0;

  return h('section', { className: 'mintcat-backup-page', 'aria-label': '备份设置' },
    h('div', { className: 'mintcat-backup-header' },
      h('div', { className: 'mintcat-backup-heading' },
        h('h2', { className: 'mintcat-backup-title' }, '备份与恢复'),
        h('p', { className: 'mintcat-backup-subtitle' }, '导出或恢复本机便携备份文件，按需同步云端快照或从旧版迁移配置。'))),

    model.status ? h(Alert, {
      type: model.statusError ? 'error' : 'info',
      showIcon: true,
      closable: true,
      message: model.status,
      role: model.statusError ? 'alert' : 'status',
      className: 'mintcat-backup-feedback',
    }) : null,

    hasActiveOrPendingTasks ? h('div', { className: 'mintcat-backup-group mintcat-backup-group--highlight' },
      h('div', { className: 'mintcat-backup-group-header' },
        h('div', { className: 'mintcat-backup-group-title-row' },
          h('h3', { className: 'mintcat-backup-group-title' }, '当前备份与恢复任务'),
          h(Tag, { color: 'processing' }, `${model.tasks.pending.length + jobItems.length} 项`)),
        actionButton('jobs-refresh', '刷新状态', model.poll, { small: true, icon: icon('ReloadOutlined') })),
      h('div', { className: 'mintcat-backup-group-body' },
        ...model.tasks.pending.map(request => h('div', { key: request.key, className: 'mintcat-backup-pending' },
          h('div', { className: 'mintcat-backup-pending-head' },
            h('strong', null, `${commandLabels[request.method]} · 响应尚未确定`),
            h(Typography.Text, { code: true }, request.key)),
          code(request.params),
          actionButton(`pending:${request.key}`, '使用原幂等键恢复请求', () => model.resolvePending(request), { primary: true }))),
        jobItems.length ? h(Collapse, {
          size: 'small',
          items: jobItems,
          activeKey: [...model.expandedJobs],
          onChange: (keys: string | string[]) => model.expandJobs(typeof keys === 'string' ? [keys] : keys),
        }) : null)) : null,

    /* Group 1: Clean Flat Rows for Local Portable Archive */
    h('div', { className: 'mintcat-backup-group' },
      h('div', { className: 'mintcat-backup-group-header' },
        h('div', { className: 'mintcat-backup-group-title-row' },
          h('h3', { className: 'mintcat-backup-group-title' }, '离线便携归档'),
          h(Tag, { color: 'success' }, '推荐 · 本机可用'))),
      h('div', { className: 'mintcat-backup-rows' },
        h('div', { className: 'mintcat-backup-row' },
          h('div', { className: 'mintcat-backup-row-info' },
            h('div', { className: 'mintcat-backup-row-title' }, '导出本机备份'),
            h('div', { className: 'mintcat-backup-row-desc' }, '将模组配置、排序与对话记录保存为单个 .json 文件（亦作为云下载保存路径）。')),
          h(Space.Compact, { className: 'mintcat-backup-row-control' },
            pathInput('output', '输出归档路径', '/Users/name/Backups/mintcat-backup.json'),
            actionButton('export', '导出离线备份', model.exportArchive, { primary: true, icon: icon('DownloadOutlined') }))),

        h('div', { className: 'mintcat-backup-row' },
          h('div', { className: 'mintcat-backup-row-info' },
            h('div', { className: 'mintcat-backup-row-title' }, '从归档文件恢复'),
            h('div', { className: 'mintcat-backup-row-desc' }, '将便携 .json 归档恢复至当前空模组库，提交后经人工核对确认生效。')),
          h(Space.Compact, { className: 'mintcat-backup-row-control' },
            pathInput('source', '待恢复归档路径', '输入待恢复的 .json 归档完整路径'),
            actionButton('restore', '恢复归档', model.restoreArchive, { icon: icon('RollbackOutlined') }))))),

    /* Group 2: Cloud Backup */
    h('div', { className: 'mintcat-backup-group' },
      h('div', { className: 'mintcat-backup-group-header' },
        h('div', { className: 'mintcat-backup-group-title-row' },
          h('h3', { className: 'mintcat-backup-group-title' }, '云端快照同步'),
          cloudUnconfigured ? h(Tag, null, '未启用') : null),
        (!cloudUnconfigured || showCloudDetails) ? actionButton('cloud-refresh', '刷新', model.refresh, { small: true, icon: icon('ReloadOutlined') }) : null),
      cloudUnconfigured && !showCloudDetails
        ? h('div', { className: 'mintcat-backup-rows' },
          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, '当前环境未配置远程云备份服务'),
              h('div', { className: 'mintcat-backup-row-desc' }, '日常备份与换机迁移直接使用上方「离线便携归档」即可完整保存所有配置。')),
            h(Button, { size: 'small', onClick: () => setShowCloudDetails(true) }, '展开云端选项')))
        : h('div', { className: 'mintcat-backup-group-body' },
          h(Space.Compact, { className: 'mintcat-backup-cloud-controls' },
            pathInput('note', '云备份备注', '可选快照备注（例如：第五赛季更新前）'),
            actionButton('upload', '立即备份至云端', model.upload, { primary: true, icon: icon('CloudUploadOutlined') })),
          cloudUnconfigured
            ? h(Alert, { type: 'info', showIcon: true, message: model.historyError })
            : error(model.historyError),
          h(Table<CloudBackupRecord>, {
            columns,
            dataSource: model.records,
            rowKey: 'id',
            size: 'small',
            loading: model.historyLoading,
            pagination: false,
            scroll: { x: 600 },
            locale: { emptyText: '暂无云端备份记录' },
            expandable: { expandedRowRender: record => code(record) },
          }))),

    /* Group 3: Folded Advanced Migration & Recovery Tools */
    h(Collapse, {
      className: 'mintcat-backup-advanced',
      items: [{
        key: 'advanced',
        label: h('div', { className: 'mintcat-backup-advanced-label' },
          h('span', { className: 'mintcat-backup-advanced-title' }, '高级数据迁移与维护工具'),
          h('span', { className: 'mintcat-backup-advanced-sub' }, '旧版 SQLite 数据库迁移 · 配置快照导入 · 维护状态自检 · 关联任务 ID')),
        children: h('div', { className: 'mintcat-backup-rows mintcat-backup-rows--flush' },
          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, '旧版数据库迁移 (SQLite)'),
              h('div', { className: 'mintcat-backup-row-desc' }, '从旧版 MintCat 数据库导入模组列表与配置，账号凭据需在批准时单独勾选。')),
            h(Space.Compact, { className: 'mintcat-backup-row-control' },
              pathInput('legacySource', '旧版 SQLite 路径', '旧版 SQLite 数据库完整路径'),
              actionButton('legacy-import', '迁移旧版数据', model.importLegacy))),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, '导入配置快照'),
              h('div', { className: 'mintcat-backup-row-desc' }, '从便携备份或旧版 SQLite 文件单独导入配置结构。')),
            h(Space.Compact, { className: 'mintcat-backup-row-control' },
              h(Select<string>, {
                value: model.fields.configFormat,
                'aria-label': '配置导入格式',
                style: { width: 126 },
                options: [{ value: 'backup', label: '便携备份' }, { value: 'legacy-sqlite', label: '旧版 SQLite' }],
                onChange: (value: string) => model.updateField('configFormat', value),
              }),
              pathInput('configSource', '配置快照路径', '配置快照完整路径'),
              actionButton('config-import', '导入快照', model.importConfig))),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, '维护状态自检与恢复'),
              h('div', { className: 'mintcat-backup-row-desc' }, '检查并清理异常中断遗留的维护锁定状态，绝不覆盖较新的写入数据。')),
            actionButton('recover', '检查并恢复维护状态', model.recover)),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, '关联后台任务'),
              h('div', { className: 'mintcat-backup-row-desc' }, '输入已知的守护进程备份或导入任务 ID 以订阅进度与批准动作。')),
            h(Space.Compact, { className: 'mintcat-backup-row-control' },
              h(Input, {
                value: model.existingJob,
                placeholder: '已有后台任务 ID',
                'aria-label': '已有后台任务 ID',
                onChange: (event: { target: { value: string } }) => model.updateExistingJob(event.target.value),
              }),
              actionButton('attach', '接入任务', model.attach))),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-desc' },
                '安全说明：便携归档不包含登录凭据、代理 CA 私钥或系统级授权记录；恢复要求当前模组库为空并经人工独立审阅确认。')))),
      }],
    }));
}
