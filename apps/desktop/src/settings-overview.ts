import type { ConfiguredSource } from '@mintcat/sdk-ts';
import { getSharedRuntime, getUiTheme, mountReactView, setUiTheme } from '@mintcat/sdk-ui';
import type { Disposable, GameModuleList, UiContext, UiThemeName } from '@mintcat/sdk-ui';

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
    name: '樱粉微光',
    desc: '柔和暖调樱花粉氛围配色',
    windowBg: '#f6edf4',
    sidebarBg: '#fbf1f8',
    cardBg: '#ffffff',
    accent: '#e84393',
    text: '#2b1e28',
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
];

const SOURCE_LABELS: Record<string, { label: string; desc: string; dotClass: string }> = {
  modio: { label: 'mod.io', desc: '官方合作模组仓库连接器', dotClass: 'settings-source-dot--modio' },
  modcat: { label: 'ModCat', desc: '中文社区镜像与索引服务', dotClass: 'settings-source-dot--modcat' },
  nexusmods: { label: 'Nexus Mods', desc: 'Nexus 开放模组生态接入', dotClass: 'settings-source-dot--nexusmods' },
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
            h('p', { className: 'settings-group-subtitle' }, '支持浅色、深色及两套氛围强调色方案，偏好自动保存在本机'))),
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
  let redraw = (): void => {};

  const active = (): boolean => alive && !context.signal.aborted;
  const load = async (): Promise<void> => {
    loading = true;
    if (active()) redraw();
    try {
      const [mods, src] = await Promise.all([
        context.request<GameModuleList>('modules.list', {}),
        context.call('sources.list', {}),
      ]);
      if (!active()) return;
      inventory = mods;
      sources = src.sources;
    } finally {
      if (active()) {
        loading = false;
        redraw();
      }
    }
  };

  function RuntimeView() {
    const [, setTick] = React.useState(0);
    redraw = () => setTick(v => v + 1);
    const gameModules = (inventory?.modules ?? []).filter(item => item.game);

    return h('div', { className: 'settings-page-shell' },
      h('div', { className: 'settings-section-header' },
        h('div', null,
          h('h2', { className: 'settings-section-title' }, '扩展与后台服务'),
          h('p', { className: 'settings-section-desc' }, '查看已签名游戏支持插件、远程模组源连接器及独立守护进程运行状态。')),
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
            h('p', { className: 'settings-group-subtitle' }, '当前后台已加载的在线模组检索与下载来源'))),
        sources.length
          ? h('div', { className: 'settings-row-list' }, ...sources.map(source => {
            const meta = SOURCE_LABELS[source.sourceId] ?? { label: source.sourceId, desc: '在线模组源服务', dotClass: '' };
            return h('div', { key: source.sourceId, className: 'settings-flat-row' },
              h('div', { className: 'settings-flat-row-info' },
                h('div', { className: 'settings-item-title-row' },
                  h('span', { className: `settings-source-dot ${meta.dotClass}` }),
                  h('span', { className: 'settings-flat-row-title' }, meta.label)),
                h('div', { className: 'settings-flat-row-desc' }, meta.desc)),
              h(Antd.Tag, { color: 'success' }, '已就绪'));
          }))
          : h('div', { className: 'settings-flat-row' },
            h('div', { className: 'settings-flat-row-info' },
              h('div', { className: 'settings-flat-row-title' }, '暂无已启用的在线来源'),
              h('div', { className: 'settings-flat-row-desc' }, '仍可通过本地导入管理离线模组文件。')))),

      h('section', { className: 'settings-group-card' },
        h('div', { className: 'settings-flat-row' },
          h('div', { className: 'settings-flat-row-info' },
            h('div', { className: 'settings-flat-row-title' }, '后台守护进程诊断与恢复'),
            h('div', { className: 'settings-flat-row-desc' }, '查看独立后台进程运行状态、运行日志、安全模式与本地已确认恢复集。')),
          h(Antd.Button, { icon: h(Icons.ToolOutlined), onClick: onOpenRescue }, '打开诊断与恢复'))));
  }

  const view = mountReactView(root, h(RuntimeView));
  const sub = context.onInventoryChanged(() => { context.guard(load)(); });
  context.guard(load)();

  return {
    dispose() {
      alive = false;
      sub.dispose();
      view.dispose();
    },
  };
}
