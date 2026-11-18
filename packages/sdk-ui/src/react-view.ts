import type { ReactNode } from 'react';
import type { ThemeConfig } from 'antd';
import type { Disposable } from './index';
import { getSharedRuntime } from './index';

export type UiThemeName = 'Light' | 'Dark' | 'Pink' | 'Blue';
export type UiNavigationIcon = 'home' | 'search' | 'settings' | 'robot' | 'cloud' | 'install' | 'proxy' | 'local' | 'downloads';
export interface UiSelection { gameId: string; gameName: string; profileId?: string }
const themes: readonly UiThemeName[] = ['Light', 'Dark', 'Pink', 'Blue'];

export function getUiTheme(): UiThemeName {
  const current = document.documentElement.dataset.uiTheme;
  if (themes.includes(current as UiThemeName)) return current as UiThemeName;
  try { const name = localStorage.getItem('theme'); if (themes.includes(name as UiThemeName)) return name as UiThemeName; } catch { /* UI preferences are optional, not business authority. */ }
  return 'Light';
}
export function setUiTheme(name: UiThemeName): void {
  if (!themes.includes(name)) throw new Error('Unknown UI theme');
  try { localStorage.setItem('theme', name); } catch { /* Theme remains usable without persistence. */ }
  const root = document.documentElement;
  root.dataset.themeSwitching = 'true';
  root.dataset.uiTheme = name;
  root.style.colorScheme = name === 'Dark' ? 'dark' : 'light';
  root.classList.toggle('dark-theme', name === 'Dark');
  document.body.classList.toggle('dark-theme', name === 'Dark');
  document.querySelector('#theme-style')?.remove();
  window.dispatchEvent(new CustomEvent('mintcat-ui-theme', { detail: name }));
  if (typeof requestAnimationFrame === 'function') {
    requestAnimationFrame(() => { delete root.dataset.themeSwitching; });
  } else {
    delete root.dataset.themeSwitching;
  }
}
function themeConfig(name: UiThemeName): ThemeConfig {
  const { Antd } = getSharedRuntime();
  const dark = name === 'Dark';
  const accent = name === 'Blue' ? '#3b82f6' : name === 'Pink' ? '#e84393' : '#ec4899';
  const accentSoft = name === 'Blue'
    ? (dark ? 'rgba(59, 130, 246, .22)' : 'rgba(59, 130, 246, .11)')
    : (dark ? 'rgba(236, 72, 153, .22)' : 'rgba(236, 72, 153, .11)');
  const accentHover = name === 'Blue'
    ? (dark ? 'rgba(59, 130, 246, .14)' : 'rgba(59, 130, 246, .075)')
    : (dark ? 'rgba(236, 72, 153, .13)' : 'rgba(236, 72, 153, .075)');
  return {
    cssVar: { key: `mintcat-${name}` },
    ...(dark ? { algorithm: Antd.theme.darkAlgorithm } : {}),
    token: {
      colorPrimary: accent,
      colorInfo: '#3b82f6',
      colorSuccess: '#10b981',
      colorWarning: '#f59e0b',
      colorError: '#ef4444',
      colorText: dark ? '#edf2fc' : '#1e2433',
      colorTextSecondary: dark ? '#9aa8c2' : '#5b6478',
      colorTextTertiary: dark ? '#73829e' : '#8590a6',
      colorBgContainer: dark ? 'rgba(28, 39, 62, .62)' : 'rgba(255, 255, 255, .74)',
      colorBgElevated: dark ? '#1f2b44' : '#ffffff',
      colorBgLayout: 'transparent',
      colorBorder: dark ? 'rgba(177, 196, 230, .16)' : 'rgba(100, 116, 148, .18)',
      colorBorderSecondary: dark ? 'rgba(177, 196, 230, .11)' : 'rgba(100, 116, 148, .12)',
      borderRadius: 10,
      borderRadiusSM: 7,
      borderRadiusLG: 14,
      controlHeight: 36,
      controlHeightSM: 28,
      fontSize: 13,
      fontFamily: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Segoe UI", sans-serif',
    },
    components: {
      Layout: { bodyBg: 'transparent', footerBg: 'transparent', headerBg: 'transparent', siderBg: 'transparent' },
      Menu: {
        itemBg: 'transparent',
        itemColor: dark ? '#9aa8c2' : '#5b6478',
        itemHoverBg: dark ? 'rgba(148, 172, 214, .10)' : 'rgba(100, 116, 148, .08)',
        itemHoverColor: dark ? '#edf2fc' : '#1e2433',
        itemSelectedBg: accentSoft,
        itemSelectedColor: dark ? '#ff9ed0' : (name === 'Blue' ? '#1d4ed8' : '#be185d'),
        itemBorderRadius: 10,
      },
      Table: {
        colorBgContainer: 'transparent',
        headerBg: dark ? 'rgba(30, 42, 66, .88)' : 'rgba(241, 244, 251, .92)',
        headerColor: dark ? '#9aa8c2' : '#5b6478',
        headerSplitColor: 'transparent',
        rowHoverBg: accentHover,
        rowSelectedBg: accentSoft,
        rowSelectedHoverBg: dark ? 'rgba(236, 72, 153, .30)' : 'rgba(236, 72, 153, .17)',
        cellPaddingBlock: 8,
        cellPaddingInline: 12,
      },
      Card: {
        colorBgContainer: dark ? 'rgba(26, 37, 58, .56)' : 'rgba(255, 255, 255, .72)',
        headerBg: 'transparent',
        paddingLG: 16,
      },
      Collapse: {
        headerBg: dark ? 'rgba(32, 45, 70, .46)' : 'rgba(246, 248, 254, .72)',
        contentBg: 'transparent',
      },
      Button: {
        defaultBg: dark ? 'rgba(140, 165, 206, .12)' : 'rgba(255, 255, 255, .78)',
        defaultBorderColor: dark ? 'rgba(177, 196, 230, .18)' : 'rgba(100, 116, 148, .20)',
        primaryShadow: '0 4px 14px rgba(236, 72, 153, .28)',
        fontWeight: 500,
      },
      Select: {
        selectorBg: dark ? 'rgba(140, 165, 206, .12)' : 'rgba(255, 255, 255, .78)',
        optionSelectedBg: accentSoft,
      },
      Input: {
        colorBgContainer: dark ? 'rgba(140, 165, 206, .12)' : 'rgba(255, 255, 255, .78)',
        activeShadow: '0 0 0 3px rgba(236, 72, 153, .14)',
      },
      Modal: {
        contentBg: dark ? '#1d2840' : '#fcfdff',
        headerBg: 'transparent',
      },
      Drawer: {
        colorBgElevated: dark ? 'rgba(24, 34, 54, .92)' : 'rgba(250, 251, 255, .92)',
      },
      Switch: { handleBg: '#fff' },
    },
  };
}
export function mountReactView(root: HTMLElement, node: ReactNode): Disposable {
  const { React, ReactDOMClient, flushSync, Antd } = getSharedRuntime();
  const renderer = ReactDOMClient.createRoot(root);
  let closed = false, failure: Error | undefined;
  class ViewBoundary extends React.Component<{ children?: ReactNode }, { error?: Error }> {
    state: { error?: Error } = {};
    static getDerivedStateFromError(cause: unknown): { error: Error } { return { error: cause instanceof Error ? cause : new Error(String(cause)) }; }
    componentDidCatch(cause: Error): void {
      const error = cause instanceof Error ? cause : new Error(String(cause));
      failure = error;
      queueMicrotask(() => { if (!closed) window.dispatchEvent(new CustomEvent('mintcat-ui-render-error', { detail: { root, error } })); });
    }
    render(): ReactNode { return this.state.error ? React.createElement(Antd.Result, { status: 'error', title: '页面加载失败', subTitle: this.state.error.message }) : this.props.children; }
  }
  const render = (): void => {
    if (!closed) renderer.render(React.createElement(Antd.ConfigProvider, { theme: themeConfig(getUiTheme()) }, React.createElement(Antd.App, { className: 'mintcat-ui-view' }, React.createElement(ViewBoundary, null, node))));
  };
  // Distinct palette keys prevent another root's cached light variables from
  // overriding a dark portal. Commit before observers see the new DOM theme.
  const themeChanged = (): void => { flushSync(render); };
  const dispose = (): void => { if (closed) return; closed = true; window.removeEventListener('mintcat-ui-theme', themeChanged); renderer.unmount(); };
  window.addEventListener('mintcat-ui-theme', themeChanged);
  try {
    // Report initial render failures synchronously to the page owner.
    flushSync(render);
    if (failure) throw failure;
  } catch (error) { dispose(); throw error; }
  return { dispose };
}
export function publishUiSelection(selection: UiSelection): void {
  window.dispatchEvent(new CustomEvent<UiSelection>('mintcat-ui-selection', { detail: selection }));
}
export function navigateUi(icon: UiNavigationIcon): void {
  window.dispatchEvent(new CustomEvent<UiNavigationIcon>('mintcat-ui-navigate', { detail: icon }));
}
