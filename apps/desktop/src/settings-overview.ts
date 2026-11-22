import type { ConfiguredSource } from '@mintcat/sdk-ts';
import {
  copyTextToClipboard,
  getSharedRuntime,
  getSourceAuthGuide,
  getSourceDisplayName,
  getUiLanguage,
  getUiTheme,
  isRemoteSourceId,
  mountReactView,
  openDataDir,
  openExternalUrl,
  presentUiError,
  setUiTheme,
  tr,
  validateSourceCredentialInput,
} from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, RemoteSourceId, UiContext, UiThemeName } from '@mintcat/sdk-ui';
import { LanguagePreference, WindowGlassPreference } from './appearance-preferences';

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

export const THEME_OPTIONS: readonly ThemeOption[] = [
  {
    id: 'Light',
    name: tr('矿纸白', 'Mineral White'),
    desc: tr('浅石灰底色，翡翠绿强调色', 'Pale limestone with emerald accents'),
    windowBg: '#f4f6f2',
    sidebarBg: '#ebeee7',
    cardBg: '#fcfdf9',
    accent: '#176b57',
    text: '#192b29',
  },
  {
    id: 'Dark',
    name: tr('石墨黑', 'Graphite Black'),
    desc: tr('深石墨底色，浅玉绿强调色', 'Dark graphite with pale jade accents'),
    windowBg: '#17211e',
    sidebarBg: '#121b18',
    cardBg: '#202d28',
    accent: '#7ac8a9',
    text: '#e3ebe5',
  },
  {
    id: 'Pink',
    name: tr('草莓奶霜', 'Strawberry Frosting'),
    desc: tr('奶霜白底、莓果粉按钮与手绘草莓纹样', 'Cream white, berry-pink buttons, and hand-drawn strawberries'),
    windowBg: '#fff5f8',
    sidebarBg: '#fbe2eb',
    cardBg: '#fffbfd',
    accent: '#ba376d',
    text: '#42263a',
  },
  {
    id: 'Blue',
    name: tr('青金蓝', 'Lapis Blue'),
    desc: tr('浅雾蓝底色，青金蓝强调色', 'Misty blue with lapis blue accents'),
    windowBg: '#f2f5f6',
    sidebarBg: '#e6ecef',
    cardBg: '#fbfcfd',
    accent: '#346883',
    text: '#24333e',
  },
  {
    id: 'NightCity',
    name: tr('夜城霓虹', 'Neon Night City'),
    desc: tr('电黄街头海报、珊瑚导航与冰青控件', 'Electric-yellow street posters, coral navigation, and icy cyan controls'),
    windowBg: '#160e23',
    sidebarBg: '#221332',
    cardBg: '#261c32',
    accent: '#f2f52e',
    text: '#f5effb',
  },
];

const countFormat = new Intl.NumberFormat(getUiLanguage());

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
          h('h2', { className: 'settings-section-title' }, tr('外观偏好', 'Appearance')),
          h('p', { className: 'settings-section-desc' }, tr('选择适合您的工作台色彩风格与视觉体验，切换后立即全局生效。', 'Customize how MintCat looks. Changes take effect immediately throughout the app.')))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, tr('主题配色', 'Color theme')),
            h('p', { className: 'settings-group-subtitle' }, tr('切换后立即生效，并记住本机的选择。', 'Changes take effect immediately and are saved on this device.')))),
        h('div', { className: 'settings-theme-grid', role: 'radiogroup', 'aria-label': tr('界面主题选择', 'Color theme') },
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
        h(LanguagePreference),
        h(WindowGlassPreference),
        h('div', { className: 'settings-flat-row' },
          h('div', { className: 'settings-flat-row-info' },
            h('div', { className: 'settings-flat-row-title' }, tr('全局搜索快捷键', 'Global search shortcut')),
            h('div', { className: 'settings-flat-row-desc' }, tr('在模组管理页面按下快捷键即可立即聚焦筛选输入框。', 'Use this shortcut on the mod management page to focus the filter field.'))),
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
      if (active()) context.notify(tr('无法自动打开浏览器，请点击「复制地址」后手动访问。', 'Could not open the browser. Select “Copy link” and open the link manually.'));
    });
  };
  const copyPortal = (url: string): void => {
    void copyTextToClipboard(url).then(copied => {
      if (!active()) return;
      if (!copied) { context.notify(tr('复制失败，请手动选中地址复制。', 'Could not copy the link. Select and copy it manually.')); return; }
      clearCopyTimer();
      copiedUrl = url;
      context.notify(tr('已复制凭据获取地址。', 'Credential portal link copied.'));
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
      proxyError = tr('手动代理模式下请输入有效的 http:// 或 https:// 代理地址（例如 http://127.0.0.1:7890）。', 'Enter a valid http:// or https:// proxy address, such as http://127.0.0.1:7890.');
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
      proxyMessage = proxyMode === 'manual'
        ? tr(`已启用 HTTP 代理 ${trimmed}，所有在线请求立即生效。`, `All online requests now use the HTTP proxy at ${trimmed}.`)
        : tr('已切换为直连模式，立即生效。', 'Requests now connect directly.');
      context.notify(proxyMessage, tr('网络代理设置已更新', 'Proxy settings updated'));
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause);
      proxyError = `${presented.title}${tr('：', ': ')}${presented.description}`;
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
      const count = countFormat.format(removedCount);
      const cacheItems = removedCount === 1 ? 'cache item or plugin package' : 'cache items or plugin packages';
      gcMessage = removedCount > 0
        ? tr(`已安全清理 ${count} 个未被引用的过期缓存/插件包。`, `Safely removed ${count} unused, expired ${cacheItems}.`)
        : tr('当前所有缓存工件均被活跃配置、安装收据或回滚租约引用，无需清理。', 'Nothing to clean up. Active profiles, install receipts, or rollback leases still reference all cached files.');
      context.notify(gcMessage, removedCount
        ? tr(`已清理 ${count} 个过期缓存或插件包`, `Removed ${count} expired ${cacheItems}`)
        : tr('缓存检查完成，无需清理', 'Cache checked. Nothing to clean up.'));
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause);
      gcMessage = `${presented.title}${tr('：', ': ')}${presented.description}`;
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
      const sourceName = getSourceDisplayName(targetSourceId);
      context.notify(tr(`已保存 ${sourceName} 凭据到本机系统钥匙串。`, `Saved ${sourceName} credentials to this device’s system keychain.`));
    } catch (cause) {
      if (!active()) return;
      const presented = presentUiError(cause, 'search');
      modalError = `${presented.title}${tr('：', ': ')}${presented.description}`;
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
      const sourceName = getSourceDisplayName(sourceId);
      context.notify(tr(`已清除 ${sourceName} 已保存的凭据。`, `Cleared saved ${sourceName} credentials.`));
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
          h('h2', { className: 'settings-section-title' }, tr('扩展与后台服务', 'Extensions and background services')),
          h('p', { className: 'settings-section-desc' }, tr('查看已签名游戏支持插件、远程模组源连接器凭据及独立守护进程运行状态。', 'View signed game plugins, remote mod source credentials, and background service status.'))),
        h(Antd.Button, { size: 'small', icon: h(Icons.ReloadOutlined, { spin: loading }), onClick: () => context.guard(load)() }, tr('刷新状态', 'Refresh status'))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, tr('游戏支持插件', 'Game support plugins')),
            h('p', { className: 'settings-group-subtitle' }, tr('提供游戏安装目录识别、专属运行参数与模组打包能力', 'Detect game installations, configure game-specific launch options, and package mods.')))),
        gameModules.length
          ? h('div', { className: 'settings-row-list' }, ...gameModules.map(mod => {
            const game = mod.game!;
            const variants = game.descriptor?.variants.length ?? 0;
            return h('div', { key: mod.moduleId, className: 'settings-flat-row' },
              h('div', { className: 'settings-flat-row-info' },
                h('div', { className: 'settings-item-title-row' },
                  h('span', { className: 'settings-flat-row-title' }, game.displayName),
                  h(Antd.Tag, { color: mod.enabled ? 'success' : 'default' }, mod.enabled ? tr('已启用', 'Enabled') : tr('未启用', 'Disabled'))),
                h('div', { className: 'settings-flat-row-desc' }, game.gameId, variants
                  ? tr(` · 支持 ${countFormat.format(variants)} 个发行变体`, ` · Supports ${countFormat.format(variants)} ${variants === 1 ? 'release variant' : 'release variants'}`)
                  : '')),
              mod.enabled && game.descriptor
                ? h(Antd.Button, { size: 'small', onClick: () => onNavigateRoute(`/${mod.moduleId}/settings`) }, tr('游戏设置', 'Game settings'))
                : null);
          }))
          : h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('当前未安装游戏支持插件', 'No game support plugins installed')),
              h('div', { className: 'settings-flat-row-desc' }, tr('安装已签名游戏扩展包后，此处将自动列出游戏插件并提供专属设置入口。', 'Install a signed game extension to see its plugin and access its settings here.'))),
            h('span', { className: 'settings-status-chip' }, tr('无游戏插件', 'No game plugins')))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, tr('在线模组源连接器', 'Online mod source connectors')),
            h('p', { className: 'settings-group-subtitle' }, tr('管理远程模组源的访问凭据；凭据仅加密保存在本机系统钥匙串中，不写入配置文件', 'Manage remote mod source credentials. They’re encrypted in this device’s system keychain, not stored in configuration files.')))),
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
                  h(Antd.Tag, { color: configured ? 'success' : 'warning' }, configured ? tr('已配置凭据', 'Credentials configured') : tr('未配置凭据', 'No credentials configured'))),
                h('div', { className: 'settings-flat-row-desc' }, guide
                  ? `${guide.description} · ${guide.credentialLabel}`
                  : tr('在线模组源服务', 'Online mod source'))),
              remoteId ? h('div', { className: 'settings-source-actions' },
                h(Antd.Button, {
                  size: 'small',
                  icon: h(Icons.KeyOutlined),
                  disabled: clearingSourceId === remoteId,
                  onClick: () => openModal(remoteId),
                }, configured ? tr('更换凭据', 'Replace credentials') : tr('配置凭据', 'Set credentials')),
                configured ? h(Antd.Button, {
                  size: 'small',
                  danger: true,
                  loading: clearingSourceId === remoteId,
                  onClick: () => context.guard(() => clearCredential(remoteId))(),
                }, tr('清除凭据', 'Clear credentials')) : null) : null);
          }))
          : h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('暂无已启用的在线来源', 'No enabled online sources')),
              h('div', { className: 'settings-flat-row-desc' }, tr('仍可通过本地导入管理离线模组文件。', 'You can still import local mod files.'))))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, tr('网络代理设置', 'Network proxy')),
            h('p', { className: 'settings-group-subtitle' }, tr('配置全局 HTTP/HTTPS 代理（如 Clash / V2Ray 本地端口），保存后立即对在线搜索、下载与 Agent 请求生效', 'Configure a global HTTP/HTTPS proxy, such as a local Clash or V2Ray port. Changes apply to search, downloads, and agent requests as soon as you save.')))),
        h('div', { className: 'settings-row-list' },
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('代理模式', 'Proxy mode')),
              h('div', { className: 'settings-flat-row-desc' }, tr('直连模式不读取环境变量；手动模式通过指定 HTTP/HTTPS 代理转发外部请求。', 'Direct mode ignores environment variables. Manual mode sends external requests through the specified HTTP/HTTPS proxy.'))),
            h(Antd.Select<'direct' | 'manual'>, {
              size: 'small',
              style: { width: 170 },
              value: proxyMode,
              'aria-label': tr('代理模式', 'Proxy mode'),
              disabled: proxySaving,
              options: [
                { value: 'direct', label: tr('直连（默认）', 'Direct (default)') },
                { value: 'manual', label: tr('手动 HTTP 代理', 'Manual HTTP proxy') },
              ],
              onChange: (value: 'direct' | 'manual') => { proxyMode = value; proxyError = ''; proxyMessage = ''; redraw(); },
            })),
          proxyMode === 'manual' ? h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('代理服务器地址', 'Proxy server address')),
              h('div', { className: 'settings-flat-row-desc' }, tr('支持 http:// 或 https:// 协议，例如 http://127.0.0.1:7890。', 'Use http:// or https://, for example http://127.0.0.1:7890.'))),
            h(Antd.Input, {
              size: 'small',
              style: { width: 260 },
              value: proxyUrl,
              placeholder: 'http://127.0.0.1:7890',
              'aria-label': tr('代理服务器地址', 'Proxy server address'),
              disabled: proxySaving,
              onChange: (event: { target: { value: string } }) => { proxyUrl = event.target.value; proxyError = ''; redraw(); },
            })) : null,
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              proxyError
                ? h(Antd.Typography.Text, { type: 'danger' }, proxyError)
                : proxyMessage
                  ? h(Antd.Typography.Text, { type: 'success' }, proxyMessage)
                  : h('div', { className: 'settings-flat-row-desc' }, tr('凭据仍受来源域名白名单隔离，不会泄露给未授权地址。', 'MintCat only sends credentials to domains on the source’s allowlist.'))),
            h(Antd.Button, {
              size: 'small',
              type: 'primary',
              className: 'glass-primary-action',
              loading: proxySaving,
              onClick: () => context.guard(saveProxy)(),
            }, tr('保存代理设置', 'Save proxy settings'))))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-group-head' },
          h('div', null,
            h('h3', { className: 'settings-group-title' }, tr('缓存清理与数据目录', 'Cache cleanup and data directory')),
            h('p', { className: 'settings-group-subtitle' }, tr('按引用计数安全回收未被引用的缓存文件与旧版插件包，保护所有活跃配置与安装收据', 'Remove unused cached files and old plugin packages without affecting active profiles or install receipts.')))),
        h('div', { className: 'settings-row-list' },
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('清理无用缓存（Artifact GC）', 'Clean up unused cache files')),
              h('div', { className: 'settings-flat-row-desc' }, gcMessage || tr('扫描并清理孤立的下载工件、历史模块包与过期暂存文件。', 'Find and remove unused downloads, old module packages, and expired staging files.'))),
            h(Antd.Button, {
              size: 'small',
              icon: h(Icons.ClearOutlined),
              loading: gcRunning,
              onClick: () => context.guard(runCacheGc)(),
            }, tr('立即清理', 'Clean up now'))),
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('独立数据目录', 'Dedicated data directory')),
              h('div', { className: 'settings-flat-row-desc' }, dataDirPath || tr('由桌面运行环境自动隔离管理', 'The desktop app manages this directory automatically.'))),
            h(Antd.Button, {
              size: 'small',
              icon: h(Icons.FolderOpenOutlined),
              disabled: !dataDirPath,
              onClick: () => {
                void openDataDir(dataDirPath).catch(() => {
                  if (active()) context.notify(tr('无法自动打开数据目录，请手动前往上述路径。', 'Could not open the data directory. Open the path shown above manually.'));
                });
              },
            }, tr('打开目录', 'Open directory'))),
          h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, tr('后台守护进程诊断与恢复', 'Background service diagnostics and recovery')),
              h('div', { className: 'settings-flat-row-desc' }, tr('查看独立后台进程运行状态、运行日志、安全模式与本地已确认恢复集。', 'View background service status, logs, safe mode, and locally confirmed recovery sets.'))),
            h(Antd.Button, { size: 'small', icon: h(Icons.ToolOutlined), onClick: onOpenRescue }, tr('打开诊断与恢复', 'Open diagnostics and recovery'))))),

      h(Antd.Modal, {
        open: Boolean(editingSourceId && modalGuide),
        title: modalGuide
          ? editingSource?.credentialConfigured
            ? tr(`更换 ${modalGuide.sourceName} 凭据`, `Replace ${modalGuide.sourceName} credentials`)
            : tr(`配置 ${modalGuide.sourceName} 凭据`, `Set ${modalGuide.sourceName} credentials`)
          : tr('配置来源凭据', 'Set source credentials'),
        okText: tr('保存凭据', 'Save credentials'),
        cancelText: tr('取消', 'Cancel'),
        confirmLoading: saving,
        destroyOnHidden: true,
        okButtonProps: { disabled: saving || !secretInput.trim(), className: 'glass-primary-action' },
        onCancel: closeModal,
        onOk: () => context.guard(saveCredential)(),
      }, modalGuide ? h('div', { className: 'settings-cred-modal' },
        h('div', { className: 'settings-cred-portal' },
          h('div', { className: 'settings-cred-portal-info' },
            h('span', { className: 'settings-cred-portal-label' }, tr(`官方获取入口 · ${modalGuide.portalName}`, `Official credential portal · ${modalGuide.portalName}`)),
            h('a', {
              href: modalGuide.portalUrl,
              target: '_blank',
              rel: 'noopener noreferrer',
              className: 'settings-cred-portal-url',
              onClick: (event: { preventDefault(): void }) => { event.preventDefault(); openPortal(modalGuide.portalUrl); },
            }, modalGuide.portalUrl, ' ', h(Icons.ExportOutlined))),
          h('div', { className: 'settings-cred-portal-actions' },
            h(Antd.Button, { size: 'small', icon: h(Icons.ExportOutlined), onClick: () => openPortal(modalGuide.portalUrl) }, modalGuide.actionLabel),
            h(Antd.Button, { size: 'small', icon: h(copiedUrl === modalGuide.portalUrl ? Icons.CheckOutlined : Icons.CopyOutlined), onClick: () => copyPortal(modalGuide.portalUrl) }, copiedUrl === modalGuide.portalUrl ? tr('已复制', 'Copied') : tr('复制地址', 'Copy link')))),
        h('ol', { className: 'settings-cred-steps' }, ...modalGuide.steps.map((step, index) => h('li', { key: index }, step))),
        h('div', { className: 'settings-cred-field' },
          h('label', { className: 'settings-cred-field-label', htmlFor: 'settings-source-cred-input' }, modalGuide.credentialLabel),
          h(Antd.Input.Password, {
            id: 'settings-source-cred-input',
            value: secretInput,
            placeholder: modalGuide.placeholder,
            'aria-label': tr('来源凭据输入框', 'Source credentials'),
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
          h('span', null, tr('凭据将直接加密保存在本机系统钥匙串中，不会以明文写入配置文件、数据库或日志。', 'Credentials are encrypted in this device’s system keychain and never written in plain text to configuration files, databases, or logs.')))) : null));
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
