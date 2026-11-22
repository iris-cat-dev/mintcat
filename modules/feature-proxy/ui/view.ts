import { getSharedRuntime, tr } from '@mintcat/sdk-ui';
import type { ActionRequired, JobEvent, JobSnapshot } from '@mintcat/sdk-ts';
import type { ChangeEvent, CSSProperties, ReactElement } from 'react';

export type ProxyOperation = 'start' | 'stop' | 'credentials' | 'installCertificate' | 'removeCertificate' | 'cleanupHosts' | 'applyHosts' | 'authorizePort' | 'attach' | 'refresh';
export interface ProxyModel {
  status: null | { running: boolean; identity: null | { moduleDigest: string; moduleVersion: string; protocol: string }; address: string | null; pid: number | null };
  loading: boolean;
  busy: string | null;
  error: string;
  result: string;
  digest: string;
  port: number;
  offline: boolean;
  existing: string;
  tasks: Array<{ snapshot: JobSnapshot; reviewed?: ActionRequired; events: JobEvent[] }>;
}
interface ProxyViewProps {
  initial: ProxyModel;
  subscribe(listener: (value: ProxyModel) => void): () => void;
  change(value: Partial<Pick<ProxyModel, 'digest' | 'port' | 'offline' | 'existing'>>): void;
  operate(operation: ProxyOperation): void;
  approve(reviewed: ActionRequired, jobId: string): void;
}
const stateLabels: Record<JobSnapshot['state'], string> = {
  queued: tr('排队中', 'Queued'), preparing: tr('准备中', 'Preparing'), 'awaiting-approval': tr('等待人工批准', 'Awaiting manual approval'),
  running: tr('执行中', 'Running'), committing: tr('提交中', 'Committing'), succeeded: tr('已完成', 'Completed'),
  cancelling: tr('正在取消', 'Cancelling'), cancelled: tr('已取消', 'Cancelled'), failed: tr('失败', 'Failed'), 'recovery-required': tr('需要恢复', 'Recovery required'),
};
const approvalStateLabels: Record<ActionRequired['state'], string> = {
  pending: tr('待批准', 'Pending'), approved: tr('已批准', 'Approved'), rejected: tr('已拒绝', 'Rejected'),
  expired: tr('已过期', 'Expired'), consumed: tr('已使用', 'Used'),
};

export function ProxyView(props: ProxyViewProps) {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  const { Alert, Badge, Button, Card, Collapse, Empty, Flex, Form, Input, InputNumber, Space, Spin, Switch, Tag, Typography, theme } = Antd;
  const { ThunderboltOutlined, SafetyCertificateOutlined, SettingOutlined, ReloadOutlined, KeyOutlined, FileTextOutlined } = Icons;
  const { token } = theme.useToken();
  const [model, setModel] = React.useState(props.initial);
  const [advanced, setAdvanced] = React.useState<string[]>([]);
  const [openJobs, setOpenJobs] = React.useState<string[]>([]);
  React.useEffect(() => props.subscribe(setModel), [props.subscribe]);
  const pendingSignature = model.tasks.filter(task => task.reviewed?.state === 'pending').map(task => task.snapshot.jobId).join('\n');
  const pending = pendingSignature !== '';
  React.useEffect(() => {
    if (pendingSignature) {
      setAdvanced(['advanced']);
      setOpenJobs(previous => [...previous, ...pendingSignature.split('\n').filter(id => !previous.includes(id))]);
    }
  }, [pendingSignature]);
  const status = model.status;
  const running = status?.running === true;
  const disabled = model.busy !== null;
  const toggling = model.busy === 'start' || model.busy === 'stop';
  const quick = (operation: ProxyOperation, label: string, icon?: ReactElement) => h(Button, {
    key: operation, icon, disabled, loading: model.busy === operation, onClick: () => props.operate(operation),
  }, label);
  const style = {
    '--accelerator-primary': `var(--mc-accent, ${token.colorPrimary})`,
    '--accelerator-primary-hover': `color-mix(in srgb, var(--mc-accent, ${token.colorPrimaryHover}) 82%, white)`,
    '--accelerator-primary-active': `color-mix(in srgb, var(--mc-accent, ${token.colorPrimaryActive}) 82%, #753057)`,
    '--accelerator-page-bg': `var(--mc-page-bg, ${token.colorBgLayout})`,
    '--accelerator-surface': `var(--mc-panel-bg, ${token.colorBgContainer})`,
    '--accelerator-muted': `var(--mc-muted, ${token.colorTextTertiary})`,
    '--accelerator-button-text': token.colorTextLightSolid,
  } as CSSProperties;
  const logs = model.tasks.flatMap(task => task.events.map(event => `${task.snapshot.jobId} #${event.seq} · ${event.phase} · ${stateLabels[event.state]}${event.messageKey ? ` · ${event.messageKey}` : ''}${event.details ? `\n${JSON.stringify(event.details, null, 2)}` : ''}`)).slice(-200);
  const taskItems = model.tasks.map(task => {
    const { snapshot, reviewed } = task;
    const canApprove = snapshot.command === 'proxy.privilege' && reviewed?.state === 'pending' && reviewed.kind === 'proxy-system-operation' && reviewed.operationId === `proxy:${snapshot.jobId}`;
    const approvalState = reviewed && (approvalStateLabels[reviewed.state] ?? reviewed.state);
    return {
      key: snapshot.jobId,
      label: h(Space, { wrap: true }, h(Tag, { color: snapshot.state === 'failed' ? 'error' : snapshot.state === 'awaiting-approval' ? 'warning' : snapshot.state === 'succeeded' ? 'success' : 'processing' }, stateLabels[snapshot.state]), h('span', null, snapshot.command), h(Typography.Text, { type: 'secondary', className: 'accelerator-page__task-id' }, snapshot.jobId)),
      children: h(Flex, { vertical: true, gap: 12 },
        h(Card, { size: 'small', title: tr('任务快照', 'Job Snapshot') }, h('pre', { className: 'accelerator-page__log' }, JSON.stringify(snapshot, null, 2))),
        reviewed ? h(Card, { size: 'small', title: tr('独立人工批准', 'Separate Manual Approval') },
          h(Alert, { type: 'warning', showIcon: true, message: tr('仅批准本次审核的不可变系统操作', 'Approve only the exact system operation reviewed'), description: tr('批准提交 {"approved":true}，不会绕过原生系统授权。证书信任、hosts 修改、凭据及特权端口均需分别审核。', 'Approval submits {"approved":true}; it does not bypass OS authorization. Certificate trust, hosts file changes, credentials, and privileged ports each require review.') }),
          h('pre', { className: 'accelerator-page__log accelerator-page__review' }, JSON.stringify(reviewed, null, 2)),
          canApprove ? h(Button, { type: 'primary', disabled, loading: model.busy === `approve:${snapshot.jobId}`, onClick: () => props.approve(reviewed, snapshot.jobId) }, tr('人工批准已审核的代理操作', 'Approve the Reviewed Proxy Operation')) : h(Typography.Text, { type: 'secondary' }, tr(`批准状态：${approvalState}；仅匹配此任务的待批准系统操作可提交。`, `Approval status: ${approvalState}. Only pending system operations that match this job can be approved.`)),
        ) : null,
      ),
    };
  });
  return h('div', { className: 'accelerator-page', style },
    h('div', { className: 'accelerator-page__header' },
      h(Typography.Title, { level: 5, className: 'accelerator-page__title' }, h(Space, null, h(ThunderboltOutlined), tr('加速器', 'Accelerator'))),
      h('div', { className: 'accelerator-page__header-actions' }, h(Space, { wrap: true, className: 'accelerator-page__quick-actions' },
        quick('credentials', tr('创建 CA 凭据', 'Create CA Credentials'), h(KeyOutlined)),
        quick('installCertificate', tr('安装 CA 证书', 'Install CA Certificate'), h(SafetyCertificateOutlined)),
        quick('refresh', tr('刷新状态', 'Refresh Status'), h(ReloadOutlined)),
      )),
    ),
    h(Alert, { type: 'warning', showIcon: true, message: tr('HTTPS 代理会解密本机流量', 'The HTTPS proxy decrypts local traffic'), description: tr('当前原始 IP 加速策略不校验上游 TLS 证书。凭据、证书信任、hosts 修改及特权端口必须独立人工批准；关闭页面不会停止守护进程中的服务。', 'Direct IP acceleration does not verify upstream TLS certificates. Credentials, certificate trust, hosts file changes, and privileged ports each require separate manual approval. Closing this page does not stop the service.') }),
    model.error ? h(Alert, { type: 'error', showIcon: true, className: 'accelerator-page__error', message: model.error }) : null,
    h('div', { className: 'accelerator-page__hero' },
      h('div', { className: 'accelerator-page__orb' },
        running ? h(React.Fragment, null,
          ...['ring--outer', 'ring--inner', 'pulse--one', 'pulse--two', 'particle--one', 'particle--two', 'particle--three'].map(name => h('span', { key: name, className: `accelerator-page__${name.split('--')[0]} accelerator-page__${name}` })),
        ) : null,
        h('button', { type: 'button', className: `accelerator-page__button accelerator-page__button--${!status ? 'disabled' : running ? 'running' : 'idle'}`, disabled: disabled || !status, onClick: () => props.operate(running ? 'stop' : 'start'), 'aria-label': running ? tr('停止加速', 'Stop Acceleration') : tr('启动加速', 'Start Acceleration') },
          h('span', { className: 'accelerator-page__button-icon' }, toggling || model.loading ? h(Spin, { size: 'large' }) : h(ThunderboltOutlined)),
          h('span', { className: 'accelerator-page__button-primary' }, running ? tr('停止加速', 'Stop Acceleration') : status ? tr('启动加速', 'Start Acceleration') : tr('获取状态', 'Checking Status')),
          h('span', { className: 'accelerator-page__button-secondary' }, running ? tr('服务正在运行', 'Service is running') : status ? tr('使用已签名模块启动', 'Start with a signed module') : tr('等待守护进程连接', 'Waiting for the daemon')),
        ),
      ),
      h(Space, { className: 'accelerator-page__hero-state-tags', wrap: true },
        h(Tag, { color: status?.identity ? 'green' : 'default' }, status?.identity ? tr('模块身份已加载', 'Module identity loaded') : tr('模块身份待获取', 'Module identity not loaded')),
        h(Tag, { color: running ? 'processing' : 'default' }, status ? running ? tr('运行中', 'Running') : tr('已停止', 'Stopped') : tr('状态未知', 'Unknown status')),
      ),
      h(Flex, { wrap: true, gap: 12, align: 'center', justify: 'center', className: 'accelerator-page__status-strip' },
        h(Space, { size: 6 }, h(Badge, { status: running ? 'processing' : status ? 'default' : 'warning' }), h(Typography.Text, null, `${tr('版本：', 'Version: ')}${status?.identity?.moduleVersion ?? tr('未知', 'Unknown')}`)),
        status?.address ? h(Typography.Text, { type: 'secondary' }, tr(`监听：${status.address}`, `Listening on: ${status.address}`)) : null,
        status?.pid ? h(Typography.Text, { type: 'secondary' }, tr(`PID：${status.pid}`, `PID: ${status.pid}`)) : null,
      ),
    ),
    h(Collapse, { ghost: true, className: 'accelerator-page__advanced', activeKey: advanced, onChange: keys => setAdvanced(Array.isArray(keys) ? keys.map(String) : [String(keys)]), items: [{
      key: 'advanced', label: h(Space, null, h(SettingOutlined), tr('高级设置与任务日志', 'Advanced Settings and Job Logs'), pending ? h(Tag, { color: 'warning' }, stateLabels['awaiting-approval']) : null),
      children: h(Flex, { vertical: true, gap: 16 },
        h(Card, { size: 'small', title: tr('启动配置', 'Startup Settings') },
          h(Form, { layout: 'vertical' },
            h(Form.Item, { label: tr('已验证签名的模块 SHA-256 摘要', 'Verified signed module SHA-256 digest'), extra: tr('通过签名发布 / 模块管理器安装及更新模块；不信任 MD5 清单或任意下载的可执行文件。', 'Install and update modules from signed releases or through the module manager. Do not trust MD5 manifests or executables downloaded from other sources.') }, h(Input, { value: model.digest, disabled, placeholder: tr('填写已安装签名制品的 SHA-256 摘要', 'Enter the installed signed artifact’s SHA-256 digest'), onChange: (event: ChangeEvent<HTMLInputElement>) => props.change({ digest: event.target.value }) })),
            h(Flex, { wrap: true, gap: 24, align: 'center' },
              h(Form.Item, { label: tr('HTTPS 监听端口', 'HTTPS listen port') }, h(InputNumber<number>, { value: model.port, min: 1, max: 65535, disabled, onChange: (value: number | null) => props.change({ port: value ?? 0 }) })),
              h(Form.Item, { label: tr('仅使用缓存规则（离线）', 'Use cached rules only (offline)') }, h(Switch, { checked: model.offline, disabled, onChange: (value: boolean) => props.change({ offline: value }) })),
            ),
          ),
        ),
        h(Card, { size: 'small', title: tr('系统操作 · 独立人工批准', 'System Operations · Separate Manual Approval') },
          h(Typography.Paragraph, { type: 'secondary' }, tr('以下按钮创建持久任务，并非直接执行系统修改；请审核对应任务中的目标、修订与签名后逐项批准。', 'These buttons create background jobs without applying system changes immediately. Review each job’s target, revision, and signature before approving it.')),
          h(Space, { wrap: true }, quick('removeCertificate', tr('移除 CA 信任', 'Remove CA Trust')), quick('applyHosts', tr('应用 hosts 规则', 'Apply Hosts Rules')), quick('cleanupHosts', tr('清理 hosts', 'Clean Up Hosts File')), quick('authorizePort', tr('授权监听端口', 'Authorize Listen Port'))),
        ),
        h(Card, { size: 'small', title: tr('持久代理任务', 'Background Proxy Jobs') },
          h(Space.Compact, { block: true, className: 'accelerator-page__attach' },
            h(Input, { value: model.existing, disabled, 'aria-label': tr('现有守护进程代理任务 ID', 'Existing proxy job ID'), placeholder: tr('输入现有代理任务 ID', 'Enter an existing proxy job ID'), onChange: (event: ChangeEvent<HTMLInputElement>) => props.change({ existing: event.target.value }), onPressEnter: () => props.operate('attach') }),
            quick('attach', tr('关联任务', 'Attach Job')),
          ),
          taskItems.length ? h(Collapse, { items: taskItems, activeKey: openJobs, onChange: keys => setOpenJobs(Array.isArray(keys) ? keys.map(String) : [String(keys)]) }) : h(Empty, { image: Empty.PRESENTED_IMAGE_SIMPLE, description: tr('暂无持久代理任务', 'No background proxy jobs yet') }),
        ),
        h(Card, { size: 'small', title: h(Space, null, h(FileTextOutlined), tr('运行日志 · 任务事件', 'Runtime Logs · Job Events')), extra: h(Typography.Text, { type: 'secondary' }, tr('守护进程每 2 秒刷新', 'Refreshes from the daemon every 2 seconds')) }, logs.length ? h('pre', { className: 'accelerator-page__log' }, logs.join('\n\n')) : h(Typography.Text, { type: 'secondary' }, tr('尚无代理任务日志', 'No proxy job logs yet'))),
        h(Card, { size: 'small', title: tr('真实运行状态与操作结果', 'Live Status and Operation Results') },
          h('pre', { className: 'accelerator-page__log' }, status ? JSON.stringify(status, null, 2) : tr('尚未取得守护进程状态', 'Daemon status not available yet')),
          model.result ? h('pre', { className: 'accelerator-page__log accelerator-page__review' }, model.result) : null,
        ),
      ),
    }] }),
  );
}
