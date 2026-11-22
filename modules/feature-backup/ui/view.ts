import type { ActionRequired, JobEvent, JobSnapshot } from '@mintcat/sdk-ts';
import type { Json } from '@mintcat/sdk-ui';
import { getSharedRuntime, getUiLanguage, pickLocalPath, pickSavePath, tr } from '@mintcat/sdk-ui';
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
  cloudUnconfigured: boolean;
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
  clearCompletedTasks(): void;
}

export const legacyImportScope = tr('导入旧版 SQLite 中的模组库数据，保留已注册的游戏目录，不改动旧版数据库，也不覆盖已有配置档案或其他库数据。', 'Import mod library data from a legacy SQLite database. Registered game folders and the old database are left unchanged, and existing profiles or other library data are not overwritten.');
export const portableRestoreScope = tr('将便携归档恢复到空模组库。便携归档不包含凭据。', 'Restore a portable archive to an empty mod library. Portable archives do not contain credentials.');
export function isLegacyImportAction(action: ActionRequired): boolean {
  return action.kind === 'backup-owner-import' && (action.target.method === 'library.import-legacy'
    || (action.target.method === 'library.config-import' && action.target.ownerId === 'mintcat.library'));
}

function isCompletedLegacyImport(snapshot?: JobSnapshot): boolean {
  if (snapshot?.state !== 'succeeded') return false;
  if (snapshot.command === 'library.import-legacy') return true;
  // Configuration imports share a command; only SQLite jobs return a legacy import report.
  const report = snapshot.result;
  return snapshot.command === 'library.config-import' && typeof report === 'object' && report !== null
    && 'alreadyImported' in report && typeof report.alreadyImported === 'boolean';
}
const stateLabels: Record<string, string> = {
  queued: tr('排队中', 'Queued'), running: tr('执行中', 'Running'), 'awaiting-approval': tr('等待人工批准', 'Awaiting manual approval'),
  succeeded: tr('已完成', 'Completed'), failed: tr('失败', 'Failed'), cancelled: tr('已取消', 'Cancelled'),
  preparing: tr('准备中', 'Preparing'), committing: tr('提交中', 'Committing'),
  cancelling: tr('正在取消', 'Cancelling'), 'recovery-required': tr('需要恢复', 'Recovery required'),
};
const commandLabels: Record<BackupMethod, string> = {
  'backup.export': tr('离线导出', 'Offline export'), 'backup.restore': tr('归档恢复', 'Archive restore'), 'backup.recover': tr('维护恢复', 'Maintenance recovery'),
  'backup.cloud.upload': tr('云端上传', 'Cloud upload'), 'backup.cloud.download': tr('云端下载', 'Cloud download'),
  'library.import-legacy': tr('旧版 Library 导入', 'Legacy library import'), 'library.config-import': tr('配置导入', 'Configuration import'),
};
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return '-';
  if (bytes < 1024) return `${bytes.toLocaleString(getUiLanguage(), { useGrouping: false })} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; ++unit; }
  return `${value.toLocaleString(getUiLanguage(), { minimumFractionDigits: 2, maximumFractionDigits: 2 })} ${units[unit]}`;
}
function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value || '-' : date.toLocaleString(getUiLanguage());
}

export function BackupView({ model, guided = false, detectedLegacyPath }: { model: BackupModel; guided?: boolean; detectedLegacyPath?: string }) {
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
  const browseSave = async (key: 'output'): Promise<void> => {
    const picked = await pickSavePath({ title: tr('选择离线备份导出位置', 'Choose where to save the offline backup'), defaultName: 'mintcat-backup.json', extensions: ['json'] });
    if (picked) model.updateField(key, picked);
  };
  const browseOpen = async (key: 'source' | 'legacySource' | 'configSource', title: string, extensions: string[]): Promise<void> => {
    const picked = await pickLocalPath({ kind: 'file', title, extensions });
    if (picked) model.updateField(key, picked);
  };
  const hasTerminalJobs = model.tasks.jobs.some(task => ['succeeded', 'failed', 'cancelled'].includes(model.views.get(task.jobId)?.snapshot?.state ?? ''));
  const code = (value: unknown) => h('pre', { className: 'mintcat-backup-json' }, JSON.stringify(value, null, 2));
  const error = (message: string) => message ? h(Alert, { type: 'error', showIcon: true, message, role: 'alert', className: 'mintcat-backup-feedback' }) : null;

  const columns: TableColumnsType<CloudBackupRecord> = [
    { title: tr('创建时间', 'Created'), dataIndex: 'createdAt', key: 'createdAt', width: 172, render: (value: string) => formatDate(value) },
    { title: tr('体积', 'Size'), dataIndex: 'size', key: 'size', width: 92, render: (value: number) => formatBytes(value) },
    { title: tr('版本', 'Version'), dataIndex: 'appVersion', key: 'appVersion', width: 84, render: (value?: string) => value ? h(Tag, { color: 'purple' }, value) : '-' },
    { title: tr('备注', 'Note'), dataIndex: 'note', key: 'note', ellipsis: true, render: (value?: string) => value || '-' },
    { title: tr('操作', 'Actions'), key: 'actions', width: 108, render: (_, record) => actionButton(`download:${record.id}`, tr('下载', 'Download'), () => model.download(record.id), { text: true, icon: icon('DownloadOutlined') }) },
  ];

  const jobItems = model.tasks.jobs.map(task => {
    const view = model.views.get(task.jobId);
    const snapshot = view?.snapshot;
    const action = view?.action;
    const state = snapshot?.state;
    const legacyApproval = action !== undefined && isLegacyImportAction(action);
    const legacyCompleted = isCompletedLegacyImport(snapshot);
    return {
      key: task.jobId,
      label: h(Space, { size: 8, wrap: true },
        h(Typography.Text, { strong: true }, snapshot ? commandLabels[snapshot.command as BackupMethod] ?? snapshot.command : tr('后台任务', 'Background job')),
        h(Tag, { color: state === 'succeeded' ? 'success' : state === 'failed' ? 'error' : state === 'awaiting-approval' ? 'warning' : 'processing' }, legacyCompleted ? tr('旧版数据已导入', 'Legacy data imported') : state ? stateLabels[state] ?? state : tr('读取中', 'Loading')),
        h(Typography.Text, { type: 'secondary', code: true }, task.jobId)),
      children: h(Flex, { vertical: true, gap: 12 },
        error(view?.error ?? ''),
        legacyCompleted ? h(Alert, { type: 'success', showIcon: true, message: tr('旧版数据导入完成', 'Legacy data imported'), description: tr('已注册的游戏目录保持不变。请返回模组库查看迁移结果。', 'Registered game folders were not changed. Return to the mod library to see the migration results.') }) : null,
        code({ targetPath: task.targetPath || undefined, snapshot }),
        h(Collapse, { size: 'small', items: [{ key: 'events', label: tr(`任务事件（${view?.events.length ?? 0}）`, `Job events (${view?.events.length ?? 0})`), children: code(view?.events ?? []) }] }),
        state === 'awaiting-approval' && !action ? h(Alert, { type: 'warning', showIcon: true, message: tr('保留的事件中尚无批准操作 ID，不会猜测或绕过人工批准。', 'The retained events do not contain an approval action ID yet. Manual approval cannot be inferred or bypassed.') }) : null,
        action ? h(Card, { size: 'small', title: h(Space, null, icon('SafetyCertificateOutlined'), tr('独立人工批准', 'Separate manual approval')) },
          action.kind === 'backup-owner-import' ? h(Typography.Paragraph, null, legacyApproval ? legacyImportScope : portableRestoreScope) : null,
          h(Typography.Paragraph, { type: 'secondary' }, tr('请核对冻结输入、schema、模块依赖与写入栅栏。批准前会再次读取并逐项比较当前操作，提交 expectedRevision；确认不是任务完成。', 'Review the frozen inputs, schema, module dependencies, and write safeguards. Before submitting approval with expectedRevision, MintCat reloads the operation and checks that it has not changed. Approval does not mean the job is complete.')),
          code(action),
          action.state === 'pending' && action.kind === 'backup-owner-import' ? h(Flex, { vertical: true, gap: 12 },
            legacyApproval ? h(Checkbox, {
              checked: model.tasks.credentials[action.actionId] === true,
              onChange: (event: { target: { checked: boolean } }) => model.setCredentials(action.actionId, event.target.checked),
            }, tr('明确同意将旧版凭据导入系统凭据库（默认不导入）', 'I agree to import legacy credentials into the system credential store (off by default).')) : null,
            actionButton(`approve:${action.actionId}`, tr('批准已审阅的冻结导入', 'Approve the reviewed import'), () => model.approve(task, action, legacyApproval && model.tasks.credentials[action.actionId] === true), { primary: true })) : null) : null,
        state === 'succeeded' && snapshot?.command === 'backup.cloud.download' && task.targetPath
          ? h(Flex, { vertical: true, gap: 8 },
            h(Typography.Text, { type: 'secondary' }, tr(`已下载至 ${task.targetPath}。恢复将创建新的导入任务，仍需独立人工批准。`, `Downloaded to ${task.targetPath}. Restoring creates a new import job that still requires separate manual approval.`)),
            actionButton(`restore:${task.jobId}`, tr('请求恢复已下载的归档', 'Request Restore from Download'), () => model.restoreDownload(task), { icon: icon('RollbackOutlined') })) : null),
    };
  });

  const hasActiveOrPendingTasks = model.tasks.pending.length > 0 || jobItems.length > 0;
  const cloudUnconfigured = model.cloudUnconfigured && model.records.length === 0;

  const restoreRow = h('div', { className: 'mintcat-backup-row' },
    h('div', { className: 'mintcat-backup-row-info' },
      h('div', { className: 'mintcat-backup-row-title' }, tr('从归档文件恢复', 'Restore from Archive')),
      h('div', { className: 'mintcat-backup-row-desc' }, tr('将便携 .json 归档恢复至当前空模组库，提交后经人工核对确认生效。', 'Restore a portable .json archive to the current empty mod library. The request takes effect after manual review and approval.'))),
    h(Space.Compact, { className: 'mintcat-backup-row-control' },
      pathInput('source', tr('待恢复归档路径', 'Archive path to restore'), tr('输入待恢复的 .json 归档完整路径', 'Enter the full .json archive path')),
      h(Button, { icon: icon('FolderOpenOutlined'), disabled: !!model.busy, onClick: () => void browseOpen('source', tr('选择便携备份归档 (.json)', 'Select a portable backup archive (.json)'), ['json']) }, tr('浏览', 'Browse')),
      actionButton('restore', tr('恢复归档', 'Restore Archive'), model.restoreArchive, { icon: icon('RollbackOutlined') })));
  const legacyRow = h('div', { className: 'mintcat-backup-row' },
    h('div', { className: 'mintcat-backup-row-info' },
      h('div', { className: 'mintcat-backup-row-title' }, tr('旧版数据库迁移 (SQLite)', 'Legacy database migration (SQLite)')),
      h('div', { className: 'mintcat-backup-row-desc' }, guided
        ? detectedLegacyPath && model.fields.legacySource === detectedLegacyPath
          ? tr('已检测到旧版 mintcat.sqlite。直接点击迁移即可审阅，不会改动旧文件。', 'A legacy mintcat.sqlite was found. Select Migrate Legacy Data to review the import. The old file will not be changed.')
          : tr('可选择旧版 mintcat.sqlite，Windows 上通常位于 %APPDATA%\\com.mint.cat。', 'Select a legacy mintcat.sqlite file. On Windows, it is usually in %APPDATA%\\com.mint.cat.')
        : tr('从旧版 MintCat 数据库导入模组列表与配置，账号凭据需在批准时单独勾选。', 'Import the mod list and profiles from an older MintCat database. To import account credentials, opt in separately when approving.'))),
    h(Space.Compact, { className: 'mintcat-backup-row-control' },
      pathInput('legacySource', tr('旧版 SQLite 路径', 'Legacy SQLite path'), tr('旧版 SQLite 数据库完整路径', 'Full path to the legacy SQLite database')),
      h(Button, { icon: icon('FolderOpenOutlined'), disabled: !!model.busy, onClick: () => void browseOpen('legacySource', tr('选择旧版 MintCat SQLite 数据库', 'Select a legacy MintCat SQLite database'), ['sqlite', 'db', 'sqlite3']) }, tr('浏览', 'Browse')),
      actionButton('legacy-import', tr('迁移旧版数据', 'Migrate Legacy Data'), model.importLegacy)));

  return h('section', { className: guided ? 'mintcat-backup-page mintcat-setup-backup' : 'mintcat-backup-page', 'aria-label': guided ? tr('配置导入', 'Configuration import') : tr('备份设置', 'Backup settings') },
    guided ? null : h('div', { className: 'mintcat-backup-header' },
      h('div', { className: 'mintcat-backup-heading' },
        h('h2', { className: 'mintcat-backup-title' }, tr('备份与恢复', 'Backup and Restore')),
        h('p', { className: 'mintcat-backup-subtitle' }, tr('导出或恢复本机便携备份文件，按需同步云端快照或从旧版迁移配置。', 'Export or restore portable backups, sync cloud snapshots on demand, or migrate settings from an older version.')))),

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
          h('h3', { className: 'mintcat-backup-group-title' }, tr('当前备份与恢复任务', 'Current Backup and Restore Jobs')),
          h(Tag, { color: 'processing' }, tr(`${model.tasks.pending.length + jobItems.length} 项`, `${model.tasks.pending.length + jobItems.length} total`))),
        h(Space, { size: 6 },
          hasTerminalJobs ? h(Button, { size: 'small', onClick: () => model.clearCompletedTasks() }, tr('清除已完成', 'Clear Finished')) : null,
          actionButton('jobs-refresh', tr('刷新状态', 'Refresh Status'), model.poll, { small: true, icon: icon('ReloadOutlined') }))),
      h('div', { className: 'mintcat-backup-group-body' },
        ...model.tasks.pending.map(request => h('div', { key: request.key, className: 'mintcat-backup-pending' },
          h('div', { className: 'mintcat-backup-pending-head' },
            h('strong', null, `${commandLabels[request.method]} · ${tr('响应尚未确定', 'Response not yet confirmed')}`),
            h(Typography.Text, { code: true }, request.key)),
          code(request.params),
          actionButton(`pending:${request.key}`, tr('使用原幂等键恢复请求', 'Retry with Original Idempotency Key'), () => model.resolvePending(request), { primary: true }))),
        jobItems.length ? h(Collapse, {
          size: 'small',
          items: jobItems,
          activeKey: [...model.expandedJobs],
          onChange: (keys: string | string[]) => model.expandJobs(typeof keys === 'string' ? [keys] : keys),
        }) : null)) : null,

    /* Group 1: Clean Flat Rows for Local Portable Archive */
    guided ? h('div', { className: 'mintcat-backup-group' },
      h('div', { className: 'mintcat-backup-rows' }, legacyRow, restoreRow)) : h('div', { className: 'mintcat-backup-group' },
      h('div', { className: 'mintcat-backup-group-header' },
        h('div', { className: 'mintcat-backup-group-title-row' },
          h('h3', { className: 'mintcat-backup-group-title' }, tr('离线便携归档', 'Portable Offline Archives')),
          h(Tag, { color: 'success' }, tr('推荐 · 本机可用', 'Recommended · Works offline')))),
      h('div', { className: 'mintcat-backup-rows' },
        h('div', { className: 'mintcat-backup-row' },
          h('div', { className: 'mintcat-backup-row-info' },
            h('div', { className: 'mintcat-backup-row-title' }, tr('导出本机备份', 'Export a Local Backup')),
            h('div', { className: 'mintcat-backup-row-desc' }, tr('将模组配置、排序与对话记录保存为单个 .json 文件（亦作为云下载保存路径）。', 'Save mod profiles, load order, and conversations in one .json file. This path is also used for cloud downloads.'))),
          h(Space.Compact, { className: 'mintcat-backup-row-control' },
            pathInput('output', tr('输出归档路径', 'Output archive path'), '/Users/name/Backups/mintcat-backup.json'),
            h(Button, { icon: icon('FolderOpenOutlined'), disabled: !!model.busy, onClick: () => void browseSave('output') }, tr('浏览', 'Browse')),
            actionButton('export', tr('导出离线备份', 'Export Offline Backup'), model.exportArchive, { primary: true, icon: icon('DownloadOutlined') }))),

        restoreRow)),

    /* Group 2: Cloud Backup */
    guided ? null : h('div', { className: 'mintcat-backup-group', id: 'cloud-snapshot-sync', role: 'region', 'aria-label': tr('云端快照同步', 'Cloud snapshot sync'), tabIndex: -1 },
      h('div', { className: 'mintcat-backup-group-header' },
        h('div', { className: 'mintcat-backup-group-title-row' },
          h('h3', { className: 'mintcat-backup-group-title' }, tr('云端快照同步', 'Cloud Snapshot Sync')),
          cloudUnconfigured ? h(Tag, null, tr('未启用', 'Not enabled')) : null),
        (!cloudUnconfigured || showCloudDetails) ? actionButton('cloud-refresh', tr('刷新', 'Refresh'), model.refresh, { small: true, icon: icon('ReloadOutlined') }) : null),
      cloudUnconfigured && !showCloudDetails
        ? h('div', { className: 'mintcat-backup-rows' },
          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, tr('当前环境未配置远程云备份服务', 'Cloud backup is not configured')),
              h('div', { className: 'mintcat-backup-row-desc' }, tr('日常备份与换机迁移直接使用上方「离线便携归档」即可完整保存所有配置。', 'Use Portable Offline Archives above to save all settings for regular backups or to move to a new computer.'))),
            h(Button, { size: 'small', onClick: () => setShowCloudDetails(true) }, tr('展开云端选项', 'Show Cloud Options'))))
        : h('div', { className: 'mintcat-backup-group-body' },
          h(Space.Compact, { className: 'mintcat-backup-cloud-controls' },
            pathInput('note', tr('云备份备注', 'Cloud backup note'), tr('可选快照备注（例如：第五赛季更新前）', 'Optional note (e.g., before the Season 5 update)')),
            actionButton('upload', tr('立即备份至云端', 'Back Up to Cloud Now'), model.upload, { primary: true, icon: icon('CloudUploadOutlined') })),
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
            locale: { emptyText: tr('暂无云端备份记录', 'No cloud backups yet') },
            expandable: { expandedRowRender: record => code(record) },
          }))),

    /* Group 3: Folded Advanced Migration & Recovery Tools */
    guided ? null : h(Collapse, {
      className: 'mintcat-backup-advanced',
      items: [{
        key: 'advanced',
        label: h('div', { className: 'mintcat-backup-advanced-label' },
          h('span', { className: 'mintcat-backup-advanced-title' }, tr('高级数据迁移与维护工具', 'Advanced Migration and Maintenance')),
          h('span', { className: 'mintcat-backup-advanced-sub' }, tr('旧版 SQLite 数据库迁移 · 配置快照导入 · 维护状态自检 · 关联任务 ID', 'Migrate legacy SQLite · Import snapshots · Check maintenance state · Attach jobs'))),
        children: h('div', { className: 'mintcat-backup-rows mintcat-backup-rows--flush' },
          legacyRow,

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, tr('导入配置快照', 'Import a Configuration Snapshot')),
              h('div', { className: 'mintcat-backup-row-desc' }, tr('从便携备份或旧版 SQLite 文件单独导入配置结构。', 'Import just the configuration structure from a portable backup or legacy SQLite file.'))),
            h(Space.Compact, { className: 'mintcat-backup-row-control' },
              h(Select<string>, {
                value: model.fields.configFormat,
                'aria-label': tr('配置导入格式', 'Configuration import format'),
                style: { width: 126 },
                options: [{ value: 'backup', label: tr('便携备份', 'Portable backup') }, { value: 'legacy-sqlite', label: tr('旧版 SQLite', 'Legacy SQLite') }],
                onChange: (value: string) => model.updateField('configFormat', value),
              }),
              pathInput('configSource', tr('配置快照路径', 'Configuration snapshot path'), tr('配置快照完整路径', 'Full path to the configuration snapshot')),
              h(Button, { icon: icon('FolderOpenOutlined'), disabled: !!model.busy, onClick: () => void browseOpen('configSource', tr('选择配置快照文件', 'Select a configuration snapshot'), model.fields.configFormat === 'legacy-sqlite' ? ['sqlite', 'db', 'sqlite3'] : ['json']) }, tr('浏览', 'Browse')),
              actionButton('config-import', tr('导入快照', 'Import Snapshot'), model.importConfig))),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, tr('维护状态自检与恢复', 'Maintenance Check and Recovery')),
              h('div', { className: 'mintcat-backup-row-desc' }, tr('检查并清理异常中断遗留的维护锁定状态，绝不覆盖较新的写入数据。', 'Check for and clear maintenance locks left by interrupted operations without overwriting newer data.'))),
            actionButton('recover', tr('检查并恢复维护状态', 'Check and Recover'), model.recover)),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-title' }, tr('关联后台任务', 'Attach a Background Job')),
              h('div', { className: 'mintcat-backup-row-desc' }, tr('输入已知的守护进程备份或导入任务 ID 以订阅进度与批准动作。', 'Enter an existing backup or import job ID to follow its progress and approval requests.'))),
            h(Space.Compact, { className: 'mintcat-backup-row-control' },
              h(Input, {
                value: model.existingJob,
                placeholder: tr('已有后台任务 ID', 'Existing background job ID'),
                'aria-label': tr('已有后台任务 ID', 'Existing background job ID'),
                onChange: (event: { target: { value: string } }) => model.updateExistingJob(event.target.value),
              }),
              actionButton('attach', tr('接入任务', 'Attach Job'), model.attach))),

          h('div', { className: 'mintcat-backup-row' },
            h('div', { className: 'mintcat-backup-row-info' },
              h('div', { className: 'mintcat-backup-row-desc' },
                tr('安全说明：便携归档不包含登录凭据、代理 CA 私钥或系统级授权记录；恢复要求当前模组库为空并经人工独立审阅确认。', 'Security note: Portable archives exclude login credentials, proxy CA private keys, and system authorization records. Restoring requires an empty mod library and separate manual approval.'))))),
      }],
    }));
}
