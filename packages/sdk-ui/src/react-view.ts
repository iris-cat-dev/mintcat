import type { ReactNode } from 'react';
import type { ThemeConfig } from 'antd';
import type { Disposable, NavigationContribution } from './index';
import { getSharedRuntime } from './index';

export type UiThemeName = 'Light' | 'Dark' | 'Pink' | 'Blue';
export type UiNavigationIcon = NonNullable<NavigationContribution['icon']>;
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
  document.documentElement.dataset.uiTheme = name;
  document.documentElement.style.colorScheme = name === 'Dark' ? 'dark' : 'light';
  document.documentElement.classList.toggle('dark-theme', name === 'Dark');
  document.body.classList.toggle('dark-theme', name === 'Dark');
  document.querySelector('#theme-style')?.remove();
  window.dispatchEvent(new CustomEvent('mintcat-ui-theme', { detail: name }));
}
function themeConfig(name: UiThemeName): ThemeConfig {
  const { Antd } = getSharedRuntime();
  const dark = name === 'Dark';
  return {
    cssVar: { key: `mintcat-${name}` },
    ...(dark ? { algorithm: Antd.theme.darkAlgorithm } : {}),
    token: {
      colorPrimary: '#ef4da0', colorInfo: '#60a5d7', colorSuccess: '#36c98a',
      colorText: dark ? '#e8edf8' : '#30313b', colorTextSecondary: dark ? '#adb8cd' : '#676d7d',
      colorBgContainer: dark ? 'rgba(34,47,70,.30)' : 'rgba(255,255,255,.38)',
      colorBgElevated: dark ? '#263249' : '#f8f9ff', colorBgLayout: 'transparent',
      colorBorder: dark ? 'rgba(177,194,224,.16)' : 'rgba(114,128,158,.19)',
      colorBorderSecondary: dark ? 'rgba(177,194,224,.12)' : 'rgba(114,128,158,.12)',
      borderRadius: 9, borderRadiusLG: 14, controlHeight: 34, controlHeightSM: 26, fontSize: 14,
      fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
    },
    components: {
      Layout: { bodyBg: 'transparent', footerBg: 'transparent', headerBg: 'transparent', siderBg: 'transparent' },
      Menu: { itemBg: 'transparent', itemSelectedBg: dark ? 'rgba(239,77,160,.24)' : 'rgba(239,77,160,.13)', itemSelectedColor: dark ? '#ffb3d8' : '#c42e7c' },
      Table: {
        colorBgContainer: 'transparent', headerBg: dark ? '#263249' : '#eef0f8',
        headerColor: dark ? '#adb8cd' : '#676d7d', headerSplitColor: 'transparent',
        rowHoverBg: dark ? 'rgba(239,77,160,.13)' : 'rgba(239,77,160,.085)',
        rowSelectedBg: dark ? 'rgba(239,77,160,.28)' : 'rgba(239,77,160,.19)',
        rowSelectedHoverBg: dark ? 'rgba(239,77,160,.34)' : 'rgba(239,77,160,.24)',
        cellPaddingBlock: 5, cellPaddingInline: 12,
      },
      Button: { defaultBg: dark ? 'rgba(140,165,206,.10)' : 'rgba(255,255,255,.44)', primaryShadow: '0 3px 14px rgba(239,77,160,.28)' },
      Select: { selectorBg: dark ? 'rgba(140,165,206,.10)' : 'rgba(255,255,255,.44)', optionSelectedBg: dark ? '#59324f' : '#fbe0ee' },
      Input: { colorBgContainer: dark ? 'rgba(140,165,206,.10)' : 'rgba(255,255,255,.44)', activeShadow: '0 0 0 2px rgba(239,77,160,.12)' },
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
    render(): ReactNode { return this.state.error ? React.createElement(Antd.Result, { status: 'error', title: '模块界面加载失败', subTitle: this.state.error.message }) : this.props.children; }
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
    // Admission must observe the initial React commit, not merely a queued render.
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
