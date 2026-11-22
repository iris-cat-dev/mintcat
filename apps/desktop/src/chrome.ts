import { getSharedRuntime, getUiTheme, mountReactView, navigateUi, openExternalUrl, setUiTheme, tr } from '@mintcat/sdk-ui';
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
}

export function mountChrome(header: HTMLElement, footer: HTMLElement, brandRoot: HTMLElement, openRescue: () => void, native: boolean): Disposable {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  function Brand(): ReactElement {
    const [collapsed, setCollapsed] = React.useState(() => {
      try { return window.localStorage.getItem('mintcat.sidebar.collapsed') === 'true'; }
      catch { return false; }
    });
    const label = collapsed ? tr('展开侧栏', 'Expand sidebar') : tr('收起侧栏', 'Collapse sidebar');
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
    const agentLabel = `MintCat Agent${agent.waiting ? tr('，等待回复', ', waiting for a reply') : ''}${agent.unread ? tr('，有未读回复', ', unread reply') : ''}`;
    return h('div', { className: 'title-bar' },
      h('div', { className: 'window-drag-strip', 'data-tauri-drag-region': true }),
      h('div', { className: 'toolbar-region' }, h('div', { id: 'mc-page-toolbar', className: 'page-toolbar' }),
        isSettings
          ? h('div', { className: 'chrome-fallback-toolbar chrome-context-toolbar' },
            h('div', { className: 'chrome-context-breadcrumb' },
              h('span', { className: 'chrome-context-badge' }, h(Icons.SettingOutlined)),
              h('span', { className: 'chrome-context-root' }, tr('设置', 'Settings'))),
            h('span', { className: 'chrome-context-spacer', 'data-tauri-drag-region': true }),
            h(Antd.Button, { className: 'chrome-back-btn', icon: h(Icons.ArrowLeftOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, tr('返回模组管理', 'Back to Mod Library')))
          : isInstall
          ? h('div', { className: 'chrome-fallback-toolbar chrome-context-toolbar' },
            h('div', { className: 'chrome-context-breadcrumb' },
              h('span', { className: 'chrome-context-badge' }, h(Icons.DeploymentUnitOutlined)),
              h('span', { className: 'chrome-context-root' }, tr('安装与打包', 'Install & Package'))),
            h('span', { className: 'chrome-context-spacer', 'data-tauri-drag-region': true }),
            h(Antd.Button, { className: 'chrome-back-btn', icon: h(Icons.ArrowLeftOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, tr('返回模组管理', 'Back to Mod Library')))
          : h('div', { className: 'chrome-fallback-toolbar' },
            h(Antd.Button, { className: 'chrome-game', icon: h(Icons.AppstoreOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, game?.displayName ?? tr('选择游戏', 'Select game')),
            h(Antd.Button, { className: 'chrome-search', icon: h(Icons.SearchOutlined), disabled: !available('search'), onClick: () => navigateUi('search') },
              h('span', { className: 'chrome-search-placeholder' }, tr('搜索在线模组、作者或描述…', 'Search mods, authors, or descriptions…')),
              h('kbd', { className: 'chrome-search-kbd', 'aria-hidden': true }, '⌘K')))),
      h('div', { className: 'chrome-utilities', role: 'group', 'aria-label': tr('工作台工具', 'Workbench tools') },
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
        h(Antd.Button, { type: 'text', 'aria-label': tr('帮助文档', 'Help'), title: tr('帮助文档', 'Help'), icon: h(Icons.QuestionCircleOutlined),
          onClick: () => { void openExternalUrl('https://www.mintcat.work/docs/').catch(() => { void message.error(tr('无法打开浏览器，请访问 www.mintcat.work/docs/', 'Could not open the browser. Visit www.mintcat.work/docs/.')); }); } }),
        h(Antd.Popover, { trigger: 'click', placement: 'bottomRight', open: skinOpen, onOpenChange: setSkinOpen, arrow: false,
          title: tr('界面皮肤', 'Theme'), content: h('div', { className: 'chrome-skin-options' }, ...THEME_OPTIONS.map(option =>
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
          h(Antd.Button, { type: 'text', icon: h(Icons.SkinOutlined), title: tr('皮肤', 'Theme'), 'aria-label': tr('皮肤', 'Theme'), 'aria-expanded': skinOpen })),
        h(Antd.Button, { type: 'text', title: tr('云同步', 'Cloud sync'), 'aria-label': tr('云同步', 'Cloud sync'), icon: h(Icons.CloudSyncOutlined), disabled: !available('cloud'),
          onClick: () => window.dispatchEvent(new Event('mintcat-open-cloud-sync')) }),
        h(AvatarButton, { canInstall: available('install'), canManageSources: available('proxy') })));
  }
  function StatusBar(): ReactElement {
    const readyLabel = tr('已连接独立后台', 'Connected to daemon');
    const [text, update] = React.useState(native ? tr('正在连接独立后台…', 'Connecting to daemon…') : tr('浏览器预览 · 未连接原生后台', 'Browser preview · no native daemon'));
    const [daemonReady, setDaemonReady] = React.useState(false);
    React.useEffect(() => {
      const handler = (event: Event): void => {
        const detail = (event as CustomEvent<string>).detail;
        if (detail === readyLabel) setDaemonReady(true);
        update(detail);
      };
      window.addEventListener('mintcat-ui-notice', handler); return () => window.removeEventListener('mintcat-ui-notice', handler);
    }, [readyLabel]);
    const connected = daemonReady || text === readyLabel;
    const secondaryNotice = connected && text !== readyLabel ? text : '';
    return h('div', { className: 'status-bar' },
      h('div', { className: 'status-left' },
        h('span', { role: 'status', className: `status-pill ${connected ? 'status-pill--connected' : 'status-pill--idle'}`, title: connected ? readyLabel : text },
          h('span', { className: `status-dot ${connected ? 'status-dot--connected' : 'status-dot--idle'}` }),
          connected ? readyLabel : text),
        h('span', { id: 'mc-profile-status' }),
        secondaryNotice ? h('span', { className: 'status-secondary-notice', title: secondaryNotice }, secondaryNotice) : null),
      h(Antd.Tooltip, { title: tr('后台状态、日志与恢复', 'Daemon status, logs, and recovery') },
        h(Antd.Button, { type: 'text', size: 'small', className: 'status-rescue-btn', icon: h(Icons.ToolOutlined), onClick: openRescue, 'aria-label': tr('后台状态、日志与恢复', 'Daemon status, logs, and recovery') }, tr('诊断与恢复', 'Diagnostics & Recovery'))));
  }
  const title = mountReactView(header, h(TitleBar)); const status = mountReactView(footer, h(StatusBar)); const brand = mountReactView(brandRoot, h(Brand));
  return { dispose() { title.dispose(); status.dispose(); brand.dispose(); } };
}
