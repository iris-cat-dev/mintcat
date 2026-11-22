import { getSharedRuntime, getUiTheme, mountReactView, navigateUi, openExternalUrl, setUiTheme } from '@mintcat/sdk-ui';
import type { Disposable, UiNavigationIcon, UiSelection } from '@mintcat/sdk-ui';
import type { ReactElement } from 'react';
import type { AgentIndicatorStatus } from '../../../modules/feature-agent/ui';
import { NotificationsButton } from './chrome-notifications';
import { AvatarButton } from './chrome-avatar';
import { THEME_OPTIONS } from './settings-overview';

export interface ChromeContributions { icons: UiNavigationIcon[]; games: Array<{ gameId: string; displayName: string }> }
interface ActivePageDetail {
  route: string;
  title: string;
  placement: 'sidebar' | 'titlebar' | 'settings';
  settingsTabs?: Array<{ route: string; title: string; icon: string }>;
}

export function mountChrome(header: HTMLElement, footer: HTMLElement, brandRoot: HTMLElement, openRescue: () => void, native: boolean): Disposable {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  function Brand(): ReactElement {
    const [collapsed, setCollapsed] = React.useState(() => {
      try { return window.localStorage.getItem('mintcat.sidebar.collapsed') === 'true'; }
      catch { return false; }
    });
    const label = collapsed ? '展开侧栏' : '收起侧栏';
    return h('div', { className: 'sidebar-brand', 'data-tauri-drag-region': true },
      h('div', { className: 'brand-lockup' },
        h('span', { className: 'brand-icon-shell', 'aria-hidden': true },
          h('svg', { width: 29, height: 29, viewBox: '0 0 32 32', fill: 'none', stroke: 'currentColor', strokeWidth: 1.55, strokeLinejoin: 'round', strokeLinecap: 'round' },
            h('path', { d: 'M5 21 6.4 8.5l6.2 4h7l6-4L27 21l-5.3 5H10.3Z' }),
            h('path', { d: 'm14 8.5 1.1-4.2 5.3-.8-1.6 4.2Z', fill: 'currentColor', stroke: 'none' }),
            h('path', { d: 'M10.8 18.7h1.4m7.6 0h1.4m-6.5 2.6 1.3 1 1.3-1' }))),
        h('div', { className: 'brand-meta' },
          h('h1', null, 'MintCat'),
          h('span', { className: 'app-version' }, 'v0.6.0'))),
      h('button', {
        type: 'button', className: 'sidebar-toggle',
        'aria-label': label, title: label,
        'aria-expanded': !collapsed, 'aria-controls': 'mc-sidebar-navigation',
        onClick: () => {
          setCollapsed(!collapsed);
          try { window.localStorage.setItem('mintcat.sidebar.collapsed', String(!collapsed)); }
          catch { /* The rail still toggles when preferences cannot be saved. */ }
        },
      }, h('svg', { width: 20, height: 20, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 1.65, strokeLinejoin: 'round', strokeLinecap: 'round', 'aria-hidden': true },
        h('rect', { x: 2.5, y: 4.5, width: 19, height: 15, rx: 3 }),
        h('path', { d: 'M9 5v14' }),
        h('path', { d: collapsed ? 'm14 9.5 2.5 2.5-2.5 2.5' : 'm16.5 9.5-2.5 2.5 2.5 2.5' }))));
  }
  function TitleBar(): ReactElement {
    const [contributions, updateContributions] = React.useState<ChromeContributions>({ icons: [], games: [] });
    const [selection, updateSelection] = React.useState<UiSelection>();
    const [activePage, updateActivePage] = React.useState<ActivePageDetail>();
    const [agent, updateAgent] = React.useState<AgentIndicatorStatus>({ open: false, waiting: false, unread: false });
    React.useEffect(() => {
      const changed = (event: Event): void => updateContributions((event as CustomEvent<ChromeContributions>).detail);
      const selected = (event: Event): void => updateSelection((event as CustomEvent<UiSelection>).detail);
      const paged = (event: Event): void => updateActivePage((event as CustomEvent<ActivePageDetail>).detail);
      const agentChanged = (event: Event): void => updateAgent((event as CustomEvent<AgentIndicatorStatus>).detail);
      window.addEventListener('mintcat-agent-indicator', agentChanged);
      window.addEventListener('mintcat-ui-contributions', changed);
      window.addEventListener('mintcat-ui-selection', selected);
      window.addEventListener('mintcat-ui-active-page', paged);
      const onGlobalSearchKey = (event: KeyboardEvent): void => {
        if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== 'k') return;
        const pageSearch = document.querySelector<HTMLInputElement>('.library-search input, .mintcat-search-box input:not(:disabled)');
        if (pageSearch) {
          event.preventDefault();
          pageSearch.focus();
          pageSearch.select();
          return;
        }
        event.preventDefault();
        navigateUi('search');
      };
      window.addEventListener('keydown', onGlobalSearchKey);
      return () => {
        window.removeEventListener('mintcat-ui-contributions', changed);
        window.removeEventListener('mintcat-ui-selection', selected);
        window.removeEventListener('mintcat-ui-active-page', paged);
        window.removeEventListener('mintcat-agent-indicator', agentChanged);
        window.removeEventListener('keydown', onGlobalSearchKey);
      };
    }, []);
    const available = (icon: UiNavigationIcon): boolean => contributions.icons.includes(icon);
    const game = contributions.games.find(item => item.gameId === selection?.gameId);
    const isSettings = activePage?.placement === 'settings';
    const isInstall = activePage?.route === '/mintcat.install/tasks';
    const { message } = Antd.App.useApp();
    const theme = getUiTheme();
    const [skinOpen, setSkinOpen] = React.useState(false);
    const agentLabel = `MintCat Agent${agent.waiting ? '，等待回复' : ''}${agent.unread ? '，有未读回复' : ''}`;
    return h('div', { className: 'title-bar' },
      h('div', { className: 'window-drag-strip', 'data-tauri-drag-region': true }),
      h('div', { className: 'toolbar-region' }, h('div', { id: 'mc-page-toolbar', className: 'page-toolbar' }),
        isSettings
          ? h('div', { className: 'chrome-fallback-toolbar chrome-context-toolbar' },
            h('div', { className: 'chrome-context-breadcrumb' },
              h('span', { className: 'chrome-context-badge' }, h(Icons.SettingOutlined)),
              h('span', { className: 'chrome-context-root' }, '设置')),
            h('div', { className: 'chrome-settings-pills', role: 'tablist', 'aria-label': '设置分类' },
              ...(activePage?.settingsTabs ?? []).map(tab => {
                const selected = tab.route === activePage?.route;
                const TabIcon = tab.icon === 'settings' ? Icons.BgColorsOutlined
                  : tab.icon === 'cloud' ? Icons.CloudSyncOutlined
                  : tab.icon === 'proxy' ? Icons.ApiOutlined
                  : Icons.ControlOutlined;
                return h('button', {
                  key: tab.route,
                  type: 'button',
                  role: 'tab',
                  'aria-selected': selected,
                  className: `chrome-settings-pill${selected ? ' chrome-settings-pill--active' : ''}`,
                  onClick: () => { window.location.hash = `#${tab.route}`; },
                }, h(TabIcon), h('span', null, tab.title));
              })),
            h('span', { className: 'chrome-context-spacer', 'data-tauri-drag-region': true }),
            h(Antd.Button, { className: 'chrome-back-btn', icon: h(Icons.ArrowLeftOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, '返回模组管理'))
          : isInstall
          ? h('div', { className: 'chrome-fallback-toolbar chrome-context-toolbar' },
            h('div', { className: 'chrome-context-breadcrumb' },
              h('span', { className: 'chrome-context-badge' }, h(Icons.DeploymentUnitOutlined)),
              h('span', { className: 'chrome-context-root' }, '安装与打包')),
            h('span', { className: 'chrome-context-spacer', 'data-tauri-drag-region': true }),
            h(Antd.Button, { className: 'chrome-back-btn', icon: h(Icons.ArrowLeftOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, '返回模组管理'))
          : h('div', { className: 'chrome-fallback-toolbar' },
            h(Antd.Button, { className: 'chrome-game', icon: h(Icons.AppstoreOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, game?.displayName ?? '选择游戏'),
            h(Antd.Button, { className: 'chrome-search', icon: h(Icons.SearchOutlined), disabled: !available('search'), onClick: () => navigateUi('search') },
              h('span', { className: 'chrome-search-placeholder' }, '搜索在线模组、作者或描述…'),
              h('kbd', { className: 'chrome-search-kbd', 'aria-hidden': true }, '⌘K')))),
      h('div', { className: 'chrome-utilities', role: 'group', 'aria-label': '工作台工具' },
        h(Antd.Button, { type: 'text', className: `chrome-agent-btn${agent.unread ? ' chrome-agent-btn--unread' : ''}`, 'aria-label': agentLabel, title: agentLabel,
          'aria-expanded': agent.open, 'aria-controls': agent.open ? 'mintcat-agent-drawer' : undefined,
          'aria-busy': agent.waiting, disabled: !available('robot'), onClick: () => navigateUi('robot'),
          icon: h('svg', { className: 'chrome-agent-glyph', width: 21, height: 21, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 1.6, strokeLinecap: 'round', 'aria-hidden': true },
            h('path', { d: 'M12 6V3m-1.5 0h3M2.5 12v3m19-3v3' }),
            h('rect', { x: 5, y: 6.5, width: 14, height: 13, rx: 3.5 }),
            h('path', { d: 'M10.5 16h3' }),
            h('circle', { className: 'agent-eye', cx: 9, cy: 11.5, r: 1.55 }),
            h('circle', { className: 'agent-eye agent-eye-right', cx: 15, cy: 11.5, r: 1.55 })) }),
        h(NotificationsButton),
        h(Antd.Button, { type: 'text', 'aria-label': '帮助文档', title: '帮助文档', icon: h(Icons.QuestionCircleOutlined),
          onClick: () => { void openExternalUrl('https://www.mintcat.work/docs/').catch(() => { void message.error('无法打开浏览器，请访问 www.mintcat.work/docs/'); }); } }),
        h(Antd.Popover, { trigger: 'click', placement: 'bottomRight', open: skinOpen, onOpenChange: setSkinOpen, arrow: false,
          title: '界面皮肤', content: h('div', { className: 'chrome-skin-options' }, ...THEME_OPTIONS.map(option =>
            h('button', { key: option.id, type: 'button', 'aria-pressed': theme === option.id,
              style: { backgroundColor: option.windowBg, color: option.text, borderColor: theme === option.id ? option.accent : undefined },
              onClick: () => {
                setSkinOpen(false);
                if (theme === option.id) return;
                if (document.startViewTransition && window.matchMedia('(prefers-reduced-motion: no-preference)').matches) {
                  document.startViewTransition(() => setUiTheme(option.id));
                } else setUiTheme(option.id);
              } },
            h('span', { className: 'chrome-skin-swatch', style: { backgroundColor: option.accent } },
              theme === option.id ? h(Icons.CheckOutlined) : null), option.name))) },
          h(Antd.Button, { type: 'text', icon: h(Icons.SkinOutlined), title: '皮肤', 'aria-label': '皮肤', 'aria-expanded': skinOpen })),
        h(Antd.Button, { type: 'text', title: '云同步', 'aria-label': '云同步', icon: h(Icons.CloudSyncOutlined), disabled: !available('cloud'),
          onClick: () => window.dispatchEvent(new Event('mintcat-open-cloud-sync')) }),
        h(AvatarButton, { canInstall: available('install'), canManageSources: available('proxy') })));
  }
  function StatusBar(): ReactElement {
    const [text, update] = React.useState(native ? '正在连接独立后台…' : '浏览器预览 · 未连接原生后台');
    const [daemonReady, setDaemonReady] = React.useState(false);
    React.useEffect(() => {
      const handler = (event: Event): void => {
        const detail = (event as CustomEvent<string>).detail;
        if (detail.startsWith('已连接独立后台')) setDaemonReady(true);
        update(detail);
      };
      window.addEventListener('mintcat-ui-notice', handler); return () => window.removeEventListener('mintcat-ui-notice', handler);
    }, []);
    const connected = daemonReady || text.startsWith('已连接独立后台');
    const secondaryNotice = connected && !text.startsWith('已连接独立后台') ? text : '';
    return h('div', { className: 'status-bar' },
      h('div', { className: 'status-left' },
        h('span', { role: 'status', className: `status-pill ${connected ? 'status-pill--connected' : 'status-pill--idle'}`, title: connected ? '已连接独立后台' : text },
          h('span', { className: `status-dot ${connected ? 'status-dot--connected' : 'status-dot--idle'}` }),
          connected ? '已连接独立后台' : text),
        h('span', { id: 'mc-profile-status' }),
        secondaryNotice ? h('span', { className: 'status-secondary-notice', title: secondaryNotice }, secondaryNotice) : null),
      h(Antd.Tooltip, { title: '后台状态、日志与恢复' },
        h(Antd.Button, { type: 'text', size: 'small', className: 'status-rescue-btn', icon: h(Icons.ToolOutlined), onClick: openRescue, 'aria-label': '后台状态、日志与恢复' }, '诊断与恢复')));
  }
  const title = mountReactView(header, h(TitleBar)); const status = mountReactView(footer, h(StatusBar)); const brand = mountReactView(brandRoot, h(Brand));
  return { dispose() { title.dispose(); status.dispose(); brand.dispose(); } };
}
