import { getSharedRuntime, getUiTheme, mountReactView, navigateUi, setUiTheme } from '@mintcat/sdk-ui';
import type { Disposable, UiNavigationIcon, UiSelection } from '@mintcat/sdk-ui';
import type { ReactElement } from 'react';
import logoUrl from './assets/icon.ico?url';

export interface ChromeContributions { icons: UiNavigationIcon[]; games: Array<{ gameId: string; displayName: string }> }
export function mountChrome(header: HTMLElement, footer: HTMLElement, brandRoot: HTMLElement, openRescue: () => void, native: boolean): Disposable {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  function Brand(): ReactElement {
    return h('div', { className: 'sidebar-brand' },
      h('div', { className: 'brand-lockup' }, h('img', { src: logoUrl, width: 40, height: 40, alt: 'MintCat' }),
        h('div', null, h('h1', null, 'MINTCAT'), h('span', { className: 'app-version' }, 'v0.1.0'))));
  }
  function TitleBar(): ReactElement {
    const [contributions, updateContributions] = React.useState<ChromeContributions>({ icons: [], games: [] });
    const [selection, updateSelection] = React.useState<UiSelection>();
    const [theme, updateTheme] = React.useState(getUiTheme());
    React.useEffect(() => {
      const changed = (event: Event): void => updateContributions((event as CustomEvent<ChromeContributions>).detail);
      const selected = (event: Event): void => updateSelection((event as CustomEvent<UiSelection>).detail);
      const themed = (): void => updateTheme(getUiTheme());
      window.addEventListener('mintcat-ui-contributions', changed); window.addEventListener('mintcat-ui-selection', selected); window.addEventListener('mintcat-ui-theme', themed);
      return () => { window.removeEventListener('mintcat-ui-contributions', changed); window.removeEventListener('mintcat-ui-selection', selected); window.removeEventListener('mintcat-ui-theme', themed); };
    }, []);
    const available = (icon: UiNavigationIcon): boolean => contributions.icons.includes(icon);
    const game = contributions.games.find(item => item.gameId === selection?.gameId);
    const utilities = [
      { key: 'robot', label: 'MintCat Agent', icon: h(Icons.RobotOutlined), disabled: !available('robot') },
      { key: 'cloud', label: '备份与恢复', icon: h(Icons.CloudOutlined), disabled: !available('cloud') },
      { key: 'install', label: '安装任务', icon: h(Icons.InboxOutlined), disabled: !available('install') },
      { key: 'settings', label: '设置', icon: h(Icons.SettingOutlined), disabled: !available('settings') },
      { key: 'rescue', label: '后台状态、日志与恢复', icon: h(Icons.ToolOutlined) },
    ];
    return h('div', { className: 'title-bar' },
      h('div', { className: 'window-drag-strip', 'data-tauri-drag-region': true }),
      h('div', { className: 'toolbar-region' }, h('div', { id: 'mc-page-toolbar', className: 'page-toolbar' }),
        h('div', { className: 'chrome-fallback-toolbar' },
          h(Antd.Button, { className: 'chrome-game', icon: h(Icons.PictureOutlined), disabled: !available('home'), onClick: () => navigateUi('home') }, game?.displayName ?? '选择游戏'),
          h(Antd.Button, { className: 'chrome-search', icon: h(Icons.SearchOutlined), disabled: !available('search'), onClick: () => navigateUi('search') }, '搜索模组名称、作者或描述…'),
          h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.PlayCircleOutlined), disabled: !available('install'), onClick: () => navigateUi('install') }, '安装模组'))),
      h('div', { className: 'chrome-utilities' },
        h(Antd.Tooltip, { title: theme === 'Dark' ? '切换浅色主题' : '切换深色主题' }, h(Antd.Button, { type: 'text', icon: h(theme === 'Dark' ? Icons.SunOutlined : Icons.MoonOutlined), 'aria-label': '切换主题', onClick: () => setUiTheme(theme === 'Dark' ? 'Light' : 'Dark') })),
        h(Antd.Dropdown, { trigger: ['click'], menu: { items: utilities, onClick: ({ key }) => { if (key === 'rescue') openRescue(); else navigateUi(key as UiNavigationIcon); } } },
          h(Antd.Button, { type: 'text', icon: h(Icons.MoreOutlined), 'aria-label': '更多功能' }))));
  }
  function StatusBar(): ReactElement {
    const [text, update] = React.useState(native ? '正在连接独立后台…' : '浏览器预览 · 未连接原生后台');
    React.useEffect(() => {
      const handler = (event: Event): void => update((event as CustomEvent<string>).detail);
      window.addEventListener('mintcat-ui-notice', handler); return () => window.removeEventListener('mintcat-ui-notice', handler);
    }, []);
    const connected = text.startsWith('已连接独立后台');
    return h('div', { className: 'status-bar' }, h('span', { role: 'status', className: 'status-text', title: text },
      h(connected ? Icons.CheckCircleFilled : Icons.InfoCircleOutlined, { className: connected ? 'status-connected' : 'status-info' }),
      connected ? '已连接独立后台' : text),
      h(Antd.Tooltip, { title: '后台状态、日志与恢复' }, h(Antd.Button, { type: 'text', size: 'small', icon: h(Icons.ToolOutlined), onClick: openRescue, 'aria-label': '后台状态、日志与恢复' })));
  }
  const title = mountReactView(header, h(TitleBar)); const status = mountReactView(footer, h(StatusBar)); const brand = mountReactView(brandRoot, h(Brand));
  return { dispose() { title.dispose(); status.dispose(); brand.dispose(); } };
}
