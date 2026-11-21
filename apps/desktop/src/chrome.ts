import { getSharedRuntime, getUiTheme, mountReactView, navigateUi, setUiTheme } from '@mintcat/sdk-ui';
import type { Disposable, UiNavigationIcon, UiSelection, UiThemeName } from '@mintcat/sdk-ui';
import type { ReactElement } from 'react';
import logoUrl from '../src-tauri/icons/icon.png?url';

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
    return h('div', { className: 'sidebar-brand', 'data-tauri-drag-region': true },
      h('div', { className: 'brand-lockup' },
        h('div', { className: 'brand-icon-shell' }, h('img', { src: logoUrl, width: 34, height: 34, alt: 'MintCat' })),
        h('div', { className: 'brand-meta' },
          h('h1', null, 'MINTCAT'),
          h('span', { className: 'app-version' }, 'v0.6.0'))));
  }
  function TitleBar(): ReactElement {
    const [contributions, updateContributions] = React.useState<ChromeContributions>({ icons: [], games: [] });
    const [selection, updateSelection] = React.useState<UiSelection>();
    const [activePage, updateActivePage] = React.useState<ActivePageDetail>();
    const [theme, updateTheme] = React.useState(getUiTheme());
    React.useEffect(() => {
      const changed = (event: Event): void => updateContributions((event as CustomEvent<ChromeContributions>).detail);
      const selected = (event: Event): void => updateSelection((event as CustomEvent<UiSelection>).detail);
      const paged = (event: Event): void => updateActivePage((event as CustomEvent<ActivePageDetail>).detail);
      const themed = (): void => updateTheme(getUiTheme());
      window.addEventListener('mintcat-ui-contributions', changed);
      window.addEventListener('mintcat-ui-selection', selected);
      window.addEventListener('mintcat-ui-active-page', paged);
      window.addEventListener('mintcat-ui-theme', themed);
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
        window.removeEventListener('mintcat-ui-theme', themed);
        window.removeEventListener('keydown', onGlobalSearchKey);
      };
    }, []);
    const available = (icon: UiNavigationIcon): boolean => contributions.icons.includes(icon);
    const game = contributions.games.find(item => item.gameId === selection?.gameId);
    const isSettings = activePage?.placement === 'settings';
    const isInstall = activePage?.route === '/mintcat.install/tasks';
    const utilities = [
      { key: 'robot', label: 'MintCat Agent', icon: h(Icons.RobotOutlined), disabled: !available('robot') },
      { key: 'cloud', label: '备份与迁移', icon: h(Icons.CloudOutlined), disabled: !available('cloud') },
      { key: 'install', label: '安装与打包', icon: h(Icons.DeploymentUnitOutlined), disabled: !available('install') },
      { key: 'settings', label: '设置中心', icon: h(Icons.SettingOutlined), disabled: !available('settings') },
      { type: 'divider' as const },
      {
        key: 'themes', label: '界面主题', icon: h(Icons.BgColorsOutlined),
        children: [
          { key: 'theme:Light', label: '浅色玻璃 (Light)' },
          { key: 'theme:Dark', label: '深色夜幕 (Dark)' },
          { key: 'theme:Pink', label: '樱粉微光 (Pink)' },
          { key: 'theme:Blue', label: '晴空冷萃 (Blue)' },
        ],
      },
      { type: 'divider' as const },
      { key: 'rescue', label: '后台状态、日志与恢复', icon: h(Icons.ToolOutlined) },
    ];
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
              h('kbd', { className: 'chrome-search-kbd', 'aria-hidden': true }, '⌘K')),
            h(Antd.Button, { type: 'primary', className: 'glass-primary-action chrome-install-btn', icon: h(Icons.DeploymentUnitOutlined), disabled: !available('install'), onClick: () => navigateUi('install') }, '安装模组'))),
      h('div', { className: 'chrome-utilities' },
        h(Antd.Tooltip, { title: 'Agent 助手' }, h(Antd.Button, { type: 'text', icon: h(Icons.RobotOutlined), 'aria-label': 'Agent 助手', disabled: !available('robot'), onClick: () => navigateUi('robot') })),
        h(Antd.Tooltip, { title: '安装与打包' }, h(Antd.Button, { type: 'text', icon: h(Icons.DeploymentUnitOutlined), 'aria-label': '安装与打包', disabled: !available('install'), onClick: () => navigateUi('install') })),
        h('span', { className: 'chrome-utility-divider', 'aria-hidden': true }),
        h(Antd.Tooltip, { title: theme === 'Dark' ? '切换浅色主题' : '切换深色主题' }, h(Antd.Button, { type: 'text', className: 'chrome-theme-btn', icon: h(theme === 'Dark' ? Icons.SunOutlined : Icons.MoonOutlined), 'aria-label': '切换主题', onClick: () => setUiTheme(theme === 'Dark' ? 'Light' : 'Dark') })),
        h(Antd.Dropdown, { trigger: ['click'], menu: { selectedKeys: [`theme:${theme}`], items: utilities, onClick: ({ key }) => {
          if (key === 'rescue') openRescue();
          else if (key.startsWith('theme:')) setUiTheme(key.slice(6) as UiThemeName);
          else navigateUi(key as UiNavigationIcon);
        } } },
          h(Antd.Button, { type: 'text', icon: h(Icons.MoreOutlined), 'aria-label': '更多功能' }))));
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
        secondaryNotice ? h('span', { className: 'status-secondary-notice', title: secondaryNotice }, secondaryNotice) : null),
      h(Antd.Tooltip, { title: '后台状态、日志与恢复' },
        h(Antd.Button, { type: 'text', size: 'small', className: 'status-rescue-btn', icon: h(Icons.ToolOutlined), onClick: openRescue, 'aria-label': '后台状态、日志与恢复' }, '诊断与恢复')));
  }
  const title = mountReactView(header, h(TitleBar)); const status = mountReactView(footer, h(StatusBar)); const brand = mountReactView(brandRoot, h(Brand));
  return { dispose() { title.dispose(); status.dispose(); brand.dispose(); } };
}
