import type { ConfiguredSource } from '@mintcat/sdk-ts';
import {
  copyTextToClipboard,
  getSharedRuntime,
  getSourceAuthGuide,
  getSourceDisplayName,
  getUiTheme,
  isRemoteSourceId,
  mountReactView,
  openDataDir,
  openExternalUrl,
  presentUiError,
  setUiTheme,
  validateSourceCredentialInput,
} from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, RemoteSourceId, UiContext, UiThemeName } from '@mintcat/sdk-ui';

interface ThemeOption {
  id: UiThemeName;
  name: string;
  desc: string;
  windowBg: string;
  sidebarBg: string;
  cardBg: string;
  accent: string;
  text: string;
}

const THEME_OPTIONS: readonly ThemeOption[] = [
  {
    id: 'Light',
    name: '浅色玻璃',
    desc: '通透清爽的浅灰紫毛玻璃',
    windowBg: '#eef1f8',
    sidebarBg: '#f6f8fd',
    cardBg: '#ffffff',
    accent: '#ec4899',
    text: '#1e2433',
  },
  {
    id: 'Dark',
    name: '深色夜幕',
    desc: '低眩光深海蓝暗色工作台',
    windowBg: '#141c2e',
    sidebarBg: '#19233a',
    cardBg: '#212f4c',
    accent: '#f059a5',
    text: '#edf2fc',
  },
  {
    id: 'Pink',
    name: '草莓奶霜',
    desc: '奶霜白底、莓果粉按钮与手绘草莓纹样',
    windowBg: '#fff5f8',
    sidebarBg: '#fbe2eb',
    cardBg: '#fffbfd',
    accent: '#ba376d',
    text: '#42263a',
  },
  {
    id: 'Blue',
    name: '晴空冷萃',
    desc: '利落理性的冷调晴蓝配色',
    windowBg: '#ecf2fa',
    sidebarBg: '#f2f7fe',
    cardBg: '#ffffff',
    accent: '#3b82f6',
    text: '#1e293b',
  },
  {
    id: 'NightCity',
    name: '夜城霓虹',
    desc: '紫晶矿区、电黄主操作与冰青开关',
    windowBg: '#160e23',
    sidebarBg: '#221332',
    cardBg: '#261c32',
    accent: '#f2f52e',
    text: '#f5effb',
  },
];

const SOURCE_DOT_CLASSES: Record<string, string> = {
  modio: 'settings-source-dot--modio',
  modcat: 'settings-source-dot--modcat',
  nexusmods: 'settings-source-dot--nexusmods',
};

export function mountAppearanceSettings(root: HTMLElement): Disposable {
  const { React, Icons } = getSharedRuntime();
  const h = React.createElement;

  function AppearanceView() {
    const [theme, setThemeState] = React.useState<UiThemeName>(getUiTheme());

    React.useEffect(() => {
      const handler = (): void => setThemeState(getUiTheme());
      window.addEventListener('mintcat-ui-theme', handler);
      return () => window.removeEventListener('mintcat-ui-theme', handler);
    }, []);

    return h('div', { className: 'settings-page-shell' },
      h('div', { className: 'settings-section-header' },
        h('div', null,
          h('h2', { className: 'settings-section-title' }, '外观偏好'),
          h('p', { className: 'settings-section-desc' }, '选择适合您的工作台色彩风格与视觉体验，切换后立即全局生效。'))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, '主题配色'),
            h('p', { className: 'settings-group-subtitle' }, '切换后立即生效，并记住本机的选择。'))),
        h('div', { className: 'settings-theme-grid', role: 'radiogroup', 'aria-label': '界面主题选择' },
          ...THEME_OPTIONS.map(option => {
            const selected = theme === option.id;
            return h('button', {
              key: option.id,
              type: 'button',
              role: 'radio',
              'aria-checked': selected,
              className: `settings-theme-card${selected ? ' settings-theme-card--active' : ''}`,
              onClick: () => setUiTheme(option.id),
            },
            h('div', { className: 'settings-theme-preview', style: { background: option.windowBg } },
              h('div', { className: 'settings-theme-preview-sidebar', style: { background: option.sidebarBg } },
                h('span', { className: 'settings-theme-preview-dot', style: { background: option.accent } }),
                h('span', { className: 'settings-theme-preview-bar', style: { background: option.accent, opacity: 0.24 } }),
                h('span', { className: 'settings-theme-preview-bar', style: { background: option.text, opacity: 0.14 } })),
              h('div', { className: 'settings-theme-preview-main' },
                h('div', { className: 'settings-theme-preview-top', style: { background: option.cardBg } },
                  h('span', { className: 'settings-theme-preview-cta', style: { background: option.accent } })),
                h('div', { className: 'settings-theme-preview-body', style: { background: option.cardBg } },
                  h('span', { className: 'settings-theme-preview-line', style: { background: option.text, opacity: 0.18 } }),
                  h('span', { className: 'settings-theme-preview-line settings-theme-preview-line--short', style: { background: option.text, opacity: 0.11 } })))),
            h('div', { className: 'settings-theme-meta' },
              h('div', { className: 'settings-theme-title-row' },
                h('span', { className: 'settings-theme-name' }, option.name),
                selected ? h('span', { className: 'settings-theme-check' }, h(Icons.CheckCircleFilled)) : null),
              h('span', { className: 'settings-theme-desc' }, option.desc)));
          }))),

      h('section', { className: 'settings-group-card settings-row-list' },
        h('div', { className: 'settings-flat-row' },
          h('div', { className: 'settings-flat-row-info' },
            h('div', { className: 'settings-flat-row-title' }, '原生窗口毛玻璃材质'),
            h('div', { className: 'settings-flat-row-desc' }, '自动跟随系统「减弱透明度」无障碍选项切换原生半透明背景与纯色背板。')),
          h('span', { className: 'settings-status-chip' }, '跟随系统设置')),
        h('div', { className: 'settings-flat-row' },
          h('div', { className: 'settings-flat-row-info' },
            h('div', { className: 'settings-flat-row-title' }, '全局搜索快捷键'),
            h('div', { className: 'settings-flat-row-desc' }, '在模组管理页面按下快捷键即可立即聚焦筛选输入框。')),
          h('kbd', { className: 'settings-kbd-badge' }, '⌘ / Ctrl + K'))));
  }

  return mountReactView(root, h(AppearanceView));
}

export function mountRuntimeSettings(
  root: HTMLElement,
  context: UiContext,
  onNavigateRoute: (route: string) => void,
  onOpenRescue: () => void,
): Disposable {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  let alive = true;
  let loading = true;
  let inventory: GameModuleList | undefined;
  let sources: ConfiguredSource[] = [];
  let editingSourceId: RemoteSourceId | undefined;
  let secretInput = '';
  let modalError = '';
  let saving = false;
  let clearingSourceId = '';
  let copiedUrl = '';
  let copyResetTimer: ReturnType<typeof setTimeout> | undefined;
  let proxyMode: 'direct' | 'manual' = 'direct';
  let proxyUrl = '';
  let proxyRevision = 0;
  let proxySaving = false;
  let proxyMessage = '';
  let proxyError = '';
  let gcRunning = false;
  let gcMessage = '';
  let dataDirPath = '';
  let redraw = (): void => {};

  const active = (): boolean => alive && !context.signal.aborted;
  const clearCopyTimer = (): void => {
    if (copyResetTimer !== undefined) {
      clearTimeout(copyResetTimer);
      copyResetTimer = undefined;
    }
    copiedUrl = '';
  };
  const closeModal = (): void => {
    if (saving) return;
    editingSourceId = undefined;
    secretInput = '';
    modalError = '';
    if (active()) redraw();
  };
  const openModal = (sourceId: RemoteSourceId): void => {
    editingSourceId = sourceId;
    secretInput = '';
    modalError = '';
    if (active()) redraw();
  };
  const openPortal = (url: string): void => {
    void openExternalUrl(url).catch(() => {
      if (active()) context.notify('无法自动打开浏览器，请点击「复制地址」后手动访问。');
    });
  };
  const copyPortal = (url: string): void => {
    void copyTextToClipboard(url).then(copied => {
      if (!active()) return;
      if (!copied) { context.notify('复制失败，请手动选中地址复制。'); return; }
      clearCopyTimer();
      copiedUrl = url;
      context.notify('已复制凭据获取地址。');
      redraw();
      copyResetTimer = setTimeout(() => { copyResetTimer = undefined; copiedUrl = ''; if (active()) redraw(); }, 2_000);
    });
  };
  const load = async (): Promise<void> => {
    loading = true;
    if (active()) redraw();
    try {
      const [mods, src, netSettings, sysStatus] = await Promise.all([
        context.request<GameModuleList>('modules.list', {}),
        context.call('sources.list', {}),
        context.request<{ values: Record<string, unknown>; revision: number }>('settings.get', { scope: { kind: 'global' }, section: 'network' }).catch(() => ({ values: {} as Record<string, unknown>, revision: 0 })),
        context.call('system.status', {}).catch(() => undefined),
      ]);
      if (!active()) return;
      inventory = mods;
      sources = src.sources;
      const netValues = netSettings.values as Record<string, unknown>;
      proxyRevision = netSettings.revision;
      proxyMode = netValues.mode === 'manual' ? 'manual' : 'direct';
      proxyUrl = typeof netValues.proxyUrl === 'string' ? netValues.proxyUrl : '';
      if (sysStatus?.dataDir) dataDirPath = sysStatus.dataDir;
    } finally {
      if (active()) {
        loading = false;
        redraw();
      }
    }
  };
  const saveProxy = async (): Promise<void> => {
    if (proxySaving) return;
    const trimmed = proxyUrl.trim();
    if (proxyMode === 'manual' && !/^https?:\/\/.+/i.test(trimmed)) {
      proxyError = '手动代理模式下请输入有效的 http:// 或 https:// 代理地址（例如 http://127.0.0.1:7890）。';
      proxyMessage = '';
      if (active()) redraw();
      return;
    }
    proxySaving = true;
    proxyError = '';
    proxyMessage = '';
    if (active()) redraw();
    try {
      const saved = await context.request<{ values: Record<string, unknown>; revision: number }>('settings.update', {
        scope: { kind: 'global' },
        section: 'network',
        expectedRevision: proxyRevision,
        values: { mode: proxyMode, proxyUrl: proxyMode === 'manual' ? trimmed : '' },
      }, crypto.randomUUID());
      if (!active()) return;
      proxyRevision = saved.revision;
      proxyMessage = proxyMode === 'manual' ? `已启用 HTTP 代理 ${trimmed}，所有在线请求立即生效。` : '已切换为直连模式，立即生效。';
      context.notify(proxyMessage);
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause);
      proxyError = `${presented.title}：${presented.description}`;
    } finally {
      if (active()) {
        proxySaving = false;
        redraw();
      }
    }
  };
  const runCacheGc = async (): Promise<void> => {
    if (gcRunning) return;
    gcRunning = true;
    gcMessage = '';
    if (active()) redraw();
    try {
      const result = await context.call('updates.gc', {}, crypto.randomUUID());
      if (!active()) return;
      const removedCount = result && typeof result === 'object' && 'removed' in result && Array.isArray(result.removed)
        ? result.removed.length
        : 0;
      gcMessage = removedCount > 0
        ? `已安全清理 ${removedCount} 个未被引用的过期缓存/插件包。`
        : '当前所有缓存工件均被活跃配置、安装收据或回滚租约引用，无需清理。';
      context.notify(gcMessage);
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause);
      gcMessage = `${presented.title}：${presented.description}`;
    } finally {
      if (active()) {
        gcRunning = false;
        redraw();
      }
    }
  };
  const saveCredential = async (): Promise<void> => {
    if (!editingSourceId || saving) return;
    const targetSourceId = editingSourceId;
    const validated = validateSourceCredentialInput(secretInput);
    if (!validated.ok) {
      modalError = validated.message;
      if (active()) redraw();
      return;
    }
    saving = true;
    modalError = '';
    if (active()) redraw();
    try {
      const result = await context.call('sources.credentials.set', { sourceId: targetSourceId, secret: validated.value }, crypto.randomUUID());
      if (!active()) { secretInput = ''; return; }
      sources = sources.map(item => item.sourceId === result.sourceId
        ? { ...item, credentialConfigured: result.credentialConfigured }
        : item);
      secretInput = '';
      modalError = '';
      editingSourceId = undefined;
      context.invalidate('');
      context.notify(`已保存 ${getSourceDisplayName(targetSourceId)} 凭据到本机系统钥匙串。`);
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause, 'search');
      modalError = `${presented.title}：${presented.description}`;
    } finally {
      if (active()) {
        saving = false;
        redraw();
      }
    }
  };
  const clearCredential = async (sourceId: RemoteSourceId): Promise<void> => {
    if (clearingSourceId) return;
    clearingSourceId = sourceId;
    if (active()) redraw();
    try {
      const result = await context.call('sources.credentials.clear', { sourceId }, crypto.randomUUID());
      if (!active()) return;
      sources = sources.map(item => item.sourceId === result.sourceId
        ? { ...item, credentialConfigured: result.credentialConfigured }
        : item);
      context.invalidate('');
      context.notify(`已清除 ${getSourceDisplayName(sourceId)} 已保存的凭据。`);
    } finally {
      if (active()) {
        clearingSourceId = '';
        redraw();
      }
    }
  };

  function RuntimeView() {
    const [, setTick] = React.useState(0);
    redraw = () => setTick(v => v + 1);
    const gameModules = (inventory?.modules ?? []).filter(item => item.game);
    const modalGuide = editingSourceId ? getSourceAuthGuide(editingSourceId) : undefined;
    const editingSource = editingSourceId ? sources.find(item => item.sourceId === editingSourceId) : undefined;

    return h('div', { className: 'settings-page-shell' },
      h('div', { className: 'settings-section-header' },
        h('div', null,
          h('h2', { className: 'settings-section-title' }, '扩展与后台服务'),
          h('p', { className: 'settings-section-desc' }, '查看已签名游戏支持插件、远程模组源连接器凭据及独立守护进程运行状态。')),
        h(Antd.Button, { size: 'small', icon: h(Icons.ReloadOutlined, { spin: loading }), onClick: () => context.guard(load)() }, '刷新状态')),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, '游戏支持插件'),
            h('p', { className: 'settings-group-subtitle' }, '提供游戏安装目录识别、专属运行参数与模组打包能力'))),
        gameModules.length
          ? h('div', { className: 'settings-row-list' }, ...gameModules.map(mod => {
            const game = mod.game!;
            const variants = game.descriptor?.variants.length ?? 0;
            return h('div', { key: mod.moduleId, className: 'settings-flat-row' },
              h('div', { className: 'settings-flat-row-info' },
                h('div', { className: 'settings-item-title-row' },
                  h('span', { className: 'settings-flat-row-title' }, game.displayName),
                  h(Antd.Tag, { color: mod.enabled ? 'success' : 'default' }, mod.enabled ? '已启用' : '未启用')),
                h('div', { className: 'settings-flat-row-desc' }, `${game.gameId}${variants ? ` · 支持 ${variants} 个发行变体` : ''}`)),
              mod.enabled && game.descriptor
                ? h(Antd.Button, { size: 'small', onClick: () => onNavigateRoute(`/${mod.moduleId}/settings`) }, '游戏设置')
                : null);
          }))
          : h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '当前未安装游戏支持插件'),
              h('div', { className: 'settings-flat-row-desc' }, '安装已签名游戏扩展包后，此处将自动列出游戏插件并提供专属设置入口。')),
            h('span', { className: 'settings-status-chip' }, '无游戏插件'))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, '在线模组源连接器'),
            h('p', { className: 'settings-group-subtitle' }, '管理远程模组源的访问凭据；凭据仅加密保存在本机系统钥匙串中，不写入配置文件'))),
        sources.length
          ? h('div', { className: 'settings-row-list' }, ...sources.map(source => {
            const guide = getSourceAuthGuide(source.sourceId);
            const dotClass = SOURCE_DOT_CLASSES[source.sourceId] ?? '';
            const configured = Boolean(source.credentialConfigured);
            const remoteId = isRemoteSourceId(source.sourceId) ? source.sourceId : undefined;
            return h('div', { key: source.sourceId, className: 'settings-flat-row' },
              h('div', { className: 'settings-flat-row-info' },
                h('div', { className: 'settings-item-title-row' },
                  h('span', { className: `settings-source-dot ${dotClass}` }),
                  h('span', { className: 'settings-flat-row-title' }, guide?.sourceName ?? getSourceDisplayName(source.sourceId)),
                  h(Antd.Tag, { color: configured ? 'success' : 'warning' }, configured ? '已配置凭据' : '未配置凭据')),
                h('div', { className: 'settings-flat-row-desc' }, guide
                  ? `${guide.description} · ${guide.credentialLabel}`
                  : '在线模组源服务')),
              remoteId ? h('div', { className: 'settings-source-actions' },
                h(Antd.Button, {
                  size: 'small',
                  icon: h(Icons.KeyOutlined),
                  disabled: clearingSourceId === remoteId,
                  onClick: () => openModal(remoteId),
                }, configured ? '更换凭据' : '配置凭据'),
                configured ? h(Antd.Button, {
                  size: 'small',
                  danger: true,
                  loading: clearingSourceId === remoteId,
                  onClick: () => context.guard(() => clearCredential(remoteId))(),
                }, '清除凭据') : null) : null);
          }))
          : h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '暂无已启用的在线来源'),
              h('div', { className: 'settings-flat-row-desc' }, '仍可通过本地导入管理离线模组文件。')))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, '网络代理设置'),
            h('p', { className: 'settings-group-subtitle' }, '配置全局 HTTP/HTTPS 代理（如 Clash / V2Ray 本地端口），保存后立即对在线搜索、下载与 Agent 请求生效'))),
        h('div', { className: 'settings-row-list' },
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '代理模式'),
              h('div', { className: 'settings-flat-row-desc' }, '直连模式不读取环境变量；手动模式通过指定 HTTP/HTTPS 代理转发外部请求。')),
            h(Antd.Select<'direct' | 'manual'>, {
              size: 'small',
              style: { width: 170 },
              value: proxyMode,
              'aria-label': '代理模式',
              disabled: proxySaving,
              options: [
                { value: 'direct', label: '直连（默认）' },
                { value: 'manual', label: '手动 HTTP 代理' },
              ],
              onChange: (value: 'direct' | 'manual') => { proxyMode = value; proxyError = ''; proxyMessage = ''; redraw(); },
            })),
          proxyMode === 'manual' ? h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '代理服务器地址'),
              h('div', { className: 'settings-flat-row-desc' }, '支持 http:// 或 https:// 协议，例如 http://127.0.0.1:7890。')),
            h(Antd.Input, {
              size: 'small',
              style: { width: 260 },
              value: proxyUrl,
              placeholder: 'http://127.0.0.1:7890',
              'aria-label': '代理服务器地址',
              disabled: proxySaving,
              onChange: (event: { target: { value: string } }) => { proxyUrl = event.target.value; proxyError = ''; redraw(); },
            })) : null,
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              proxyError
                ? h(Antd.Typography.Text, { type: 'danger' }, proxyError)
                : proxyMessage
                  ? h(Antd.Typography.Text, { type: 'success' }, proxyMessage)
                  : h('div', { className: 'settings-flat-row-desc' }, '凭据仍受来源域名白名单隔离，不会泄露给未授权地址。')),
            h(Antd.Button, {
              size: 'small',
              type: 'primary',
              className: 'glass-primary-action',
              loading: proxySaving,
              onClick: () => context.guard(saveProxy)(),
            }, '保存代理设置')))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, '缓存清理与数据目录'),
            h('p', { className: 'settings-group-subtitle' }, '按引用计数安全回收未被引用的缓存文件与旧版插件包，保护所有活跃配置与安装收据'))),
        h('div', { className: 'settings-row-list' },
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '清理无用缓存（Artifact GC）'),
              h('div', { className: 'settings-flat-row-desc' }, gcMessage || '扫描并清理孤立的下载工件、历史模块包与过期暂存文件。')),
            h(Antd.Button, {
              size: 'small',
              icon: h(Icons.ClearOutlined),
              loading: gcRunning,
              onClick: () => context.guard(runCacheGc)(),
            }, '立即清理')),
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '独立数据目录'),
              h('div', { className: 'settings-flat-row-desc' }, dataDirPath || '由桌面运行环境自动隔离管理')),
            h(Antd.Button, {
              size: 'small',
              icon: h(Icons.FolderOpenOutlined),
              disabled: !dataDirPath,
              onClick: () => {
                void openDataDir(dataDirPath).catch(() => {
                  if (active()) context.notify('无法自动打开数据目录，请手动前往上述路径。');
                });
              },
            }, '打开目录')),
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '后台守护进程诊断与恢复'),
              h('div', { className: 'settings-flat-row-desc' }, '查看独立后台进程运行状态、运行日志、安全模式与本地已确认恢复集。')),
            h(Antd.Button, { size: 'small', icon: h(Icons.ToolOutlined), onClick: onOpenRescue }, '打开诊断与恢复')))),

      h(Antd.Modal, {
        open: Boolean(editingSourceId && modalGuide),
        title: modalGuide ? `${editingSource?.credentialConfigured ? '更换' : '配置'} ${modalGuide.sourceName} 凭据` : '配置来源凭据',
        okText: '保存凭据',
        cancelText: '取消',
        confirmLoading: saving,
        destroyOnHidden: true,
        okButtonProps: { disabled: saving || !secretInput.trim(), className: 'glass-primary-action' },
        onCancel: closeModal,
        onOk: () => context.guard(saveCredential)(),
      }, modalGuide ? h('div', { className: 'settings-cred-modal' },
        h('div', { className: 'settings-cred-portal' },
          h('div', { className: 'settings-cred-portal-info' },
            h('span', { className: 'settings-cred-portal-label' }, `官方获取入口 · ${modalGuide.portalName}`),
            h('a', {
              href: modalGuide.portalUrl,
              target: '_blank',
              rel: 'noopener noreferrer',
              className: 'settings-cred-portal-url',
              onClick: (event: { preventDefault(): void }) => { event.preventDefault(); openPortal(modalGuide.portalUrl); },
            }, modalGuide.portalUrl, ' ', h(Icons.ExportOutlined))),
          h('div', { className: 'settings-cred-portal-actions' },
            h(Antd.Button, { size: 'small', icon: h(Icons.ExportOutlined), onClick: () => openPortal(modalGuide.portalUrl) }, modalGuide.actionLabel),
            h(Antd.Button, { size: 'small', icon: h(copiedUrl === modalGuide.portalUrl ? Icons.CheckOutlined : Icons.CopyOutlined), onClick: () => copyPortal(modalGuide.portalUrl) }, copiedUrl === modalGuide.portalUrl ? '已复制' : '复制地址'))),
        h('ol', { className: 'settings-cred-steps' }, ...modalGuide.steps.map((step, index) => h('li', { key: index }, step))),
        h('div', { className: 'settings-cred-field' },
          h('label', { className: 'settings-cred-field-label', htmlFor: 'settings-source-cred-input' }, modalGuide.credentialLabel),
          h(Antd.Input.Password, {
            id: 'settings-source-cred-input',
            value: secretInput,
            placeholder: modalGuide.placeholder,
            'aria-label': '来源凭据输入框',
            autoComplete: 'off',
            spellCheck: false,
            allowClear: true,
            disabled: saving,
            onChange: (event: { target: { value: string } }) => {
              secretInput = event.target.value;
              if (modalError) modalError = '';
              redraw();
            },
            onPressEnter: () => {
              if (!saving && secretInput.trim()) context.guard(saveCredential)();
            },
          }),
          modalError ? h('div', { className: 'settings-cred-error', role: 'alert' }, modalError) : null),
        h('div', { className: 'settings-cred-note' },
          h(Icons.LockOutlined),
          h('span', null, '凭据将直接加密保存在本机系统钥匙串中，不会以明文写入配置文件、数据库或日志。'))) : null));
  }

  const view = mountReactView(root, h(RuntimeView));
  const sub = context.onInventoryChanged(() => { context.guard(load)(); });
  context.guard(load)();

  return {
    dispose() {
      alive = false;
      clearCopyTimer();
      secretInput = '';
      modalError = '';
      sub.dispose();
      view.dispose();
    },
  };
}
