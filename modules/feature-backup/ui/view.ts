import type { ActionRequired, JobEvent, JobSnapshot } from '@mintcat/sdk-ts';
import type { Json } from '@mintcat/sdk-ui';
import { getSharedRuntime } from '@mintcat/sdk-ui';
import type { TableColumnsType } from 'antd';
import type { ReactElement, ReactNode } from 'react';

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
  const { Alert, Button, Card, Checkbox, Collapse, Divider, Flex, Form, Input, Select, Space, Table, Tag, Typography } = Antd;
  React.useSyncExternalStore(model.subscribe, model.getRevision, model.getRevision);
  const icon = (name: 'CloudUploadOutlined' | 'ReloadOutlined' | 'DownloadOutlined' | 'RollbackOutlined' | 'SafetyCertificateOutlined') => h(Icons[name]);
  const actionButton = (name: string, label: string, action: () => Promise<void>, options: { primary?: boolean; icon?: ReactElement; text?: boolean } = {}) => h(Button, {
    type: options.primary ? 'primary' : options.text ? 'text' : 'default',
    size: options.text ? 'small' : 'middle',
    icon: options.icon,
    disabled: !!model.busy,
    loading: model.busy === name,
    onClick: () => model.perform(name, action),
  }, label);
  const pathInput = (key: Exclude<keyof Fields, 'configFormat'>, label: string, placeholder: string) => h(Input, {
    value: model.fields[key], 'aria-label': label, placeholder,
    onChange: (event: { target: { value: string } }) => model.updateField(key, event.target.value),
  });
  const formItem = (label: string, child: ReactNode) => h(Form.Item, { label }, child);
  const code = (value: unknown) => h('pre', { className: 'mintcat-backup-json' }, JSON.stringify(value, null, 2));
  const error = (message: string) => message ? h(Alert, { type: 'error', showIcon: true, message, role: 'alert', className: 'mintcat-backup-feedback' }) : null;
  const columns: TableColumnsType<CloudBackupRecord> = [
    { title: '时间', dataIndex: 'createdAt', key: 'createdAt', width: 180, render: (value: string) => formatDate(value) },
    { title: '大小', dataIndex: 'size', key: 'size', width: 100, render: (value: number) => formatBytes(value) },
    { title: '版本', dataIndex: 'appVersion', key: 'appVersion', width: 80, render: (value?: string) => value ? h(Tag, { color: 'purple' }, value) : '-' },
    { title: '备注', dataIndex: 'note', key: 'note', ellipsis: true, render: (value?: string) => value || '-' },
    { title: '操作', key: 'actions', width: 130, render: (_, record) => actionButton(`download:${record.id}`, '下载', () => model.download(record.id), { text: true, icon: icon('DownloadOutlined') }) },
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
  return h('section', { className: 'mintcat-backup-page', 'aria-label': '备份设置' },
    h(Card, { title: '云备份', className: 'mintcat-backup-card' },
      h(Space.Compact, { className: 'mintcat-backup-cloud-controls' },
        pathInput('note', '云备份备注', '可选备注'),
        actionButton('upload', '立即备份', model.upload, { primary: true, icon: icon('CloudUploadOutlined') }),
        actionButton('cloud-refresh', '刷新', model.refresh, { icon: icon('ReloadOutlined') })),
      h(Typography.Text, { type: 'secondary' }, '上传当前 owner 的逻辑快照；后台任务完成后自动刷新历史。下载使用下方输出路径，下载完成后可在任务中发起恢复。'),
      error(model.historyError),
      h(Divider),
      h(Flex, { vertical: true, gap: 12 },
        h(Typography.Text, { strong: true }, '备份历史'),
        h(Table<CloudBackupRecord>, {
          columns, dataSource: model.records, rowKey: 'id', size: 'small', loading: model.historyLoading,
          pagination: false, scroll: { x: 650 }, locale: { emptyText: '暂无云备份' },
          expandable: { expandedRowRender: record => code(record) },
        }))),
    h(Card, { title: '离线便携归档', className: 'mintcat-backup-card' },
      h(Form, { labelCol: { span: 5 }, wrapperCol: { span: 19 }, colon: false },
        formItem('输出归档路径', pathInput('output', '输出归档路径', '新的 .json 文件完整路径（导出 / 云下载）')),
        formItem('待恢复归档', pathInput('source', '待恢复归档路径', '便携归档完整路径')),
        formItem('归档操作', h(Space, { wrap: true },
          actionButton('export', '导出离线备份', model.exportArchive, { icon: icon('DownloadOutlined') }),
          actionButton('restore', '请求导入当前空 owner', model.restoreArchive, { icon: icon('RollbackOutlined') })))),
      h(Collapse, { size: 'small', items: [{ key: 'scope', label: '归档范围与恢复安全说明', children: h(Typography.Paragraph, null,
        '便携归档包含 Library 配置、排序、版本与设置、Agent 对话及 Proxy 自定义规则 / 缓存配置。各 owner 在共同维护准入下生成逻辑快照，并非跨数据库事务。恢复要求 owner 为空，并独立人工审阅冻结输入、schema、模块闭包及写入栅栏。Vault 凭据、Proxy CA 私钥、系统 hosts 所有权、运行控制器、kernel 任务 / 操作 / 批准及安装回执均不包含；Agent 任务 ID 仅保留为非权威对话历史。恢复后需重新登录并重新批准本机操作。') }] })),
    h(Card, { title: '旧版 Library 导入', className: 'mintcat-backup-card' },
      h(Form, { labelCol: { span: 5 }, wrapperCol: { span: 19 }, colon: false },
        formItem('旧版 SQLite', pathInput('legacySource', '旧版 SQLite 路径', '旧版数据库完整路径')),
        formItem('导入操作', actionButton('legacy-import', '请求旧版 Library 导入', model.importLegacy))),
      h(Typography.Text, { type: 'secondary' }, '提交后请审阅后台冻结操作。旧版凭据必须明确勾选，并经过独立确认才会导入。')),
    h(Card, { title: '配置导入', className: 'mintcat-backup-card' },
      h(Form, { labelCol: { span: 5 }, wrapperCol: { span: 19 }, colon: false },
        formItem('配置快照', pathInput('configSource', '配置快照路径', '配置快照完整路径')),
        formItem('输入格式', h(Select<string>, {
          value: model.fields.configFormat, 'aria-label': '配置导入格式', className: 'mintcat-backup-format',
          options: [{ value: 'backup', label: '便携备份' }, { value: 'legacy-sqlite', label: '旧版 SQLite' }],
          onChange: (value: string) => model.updateField('configFormat', value),
        })),
        formItem('导入操作', actionButton('config-import', '请求配置导入', model.importConfig)))),
    h(Card, { title: '维护恢复', className: 'mintcat-backup-card' },
      h(Flex, { vertical: true, gap: 12 },
        h(Typography.Text, { type: 'secondary' }, '明确检查未解决的维护状态。恢复不会在较新的 owner 写入之上重新应用导入。'),
        actionButton('recover', '检查并恢复维护状态', model.recover))),
    model.status ? h(Alert, { type: model.statusError ? 'error' : 'info', showIcon: true, message: model.status, role: model.statusError ? 'alert' : 'status', className: 'mintcat-backup-feedback' }) : null,
    h(Card, { title: '持久后台任务', className: 'mintcat-backup-card' },
      h(Space.Compact, { className: 'mintcat-backup-job-controls' },
        h(Input, { value: model.existingJob, placeholder: '已有后台任务 ID', 'aria-label': '已有后台任务 ID', onChange: (event: { target: { value: string } }) => model.updateExistingJob(event.target.value) }),
        actionButton('attach', '接入任务', model.attach),
        actionButton('jobs-refresh', '刷新', model.poll, { icon: icon('ReloadOutlined') })),
      h(Typography.Paragraph, { type: 'secondary' }, '每秒轮询持久状态与分页事件；关闭页面不会取消任务。重新打开可恢复已登记任务与尚未确定响应的请求。'),
      ...model.tasks.pending.map(request => h(Card, { key: request.key, size: 'small', title: `${commandLabels[request.method]} · 响应尚未确定`, className: 'mintcat-backup-pending' },
        h(Typography.Text, { code: true }, request.key), code(request.params),
        actionButton(`pending:${request.key}`, '使用原幂等键恢复请求', () => model.resolvePending(request)))),
      jobItems.length ? h(Collapse, {
        items: jobItems, activeKey: [...model.expandedJobs],
        onChange: (keys: string | string[]) => model.expandJobs(typeof keys === 'string' ? [keys] : keys),
      }) : h(Typography.Text, { type: 'secondary' }, '暂无已登记的后台任务')));
}
