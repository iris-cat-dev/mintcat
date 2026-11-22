import type { ReactNode } from 'react';
import type { ThemeConfig } from 'antd';
import enUS from 'antd/locale/en_US';
import zhCN from 'antd/locale/zh_CN';
import type { Disposable } from './index';
import { getSharedRuntime } from './index';
import { nightCityConfig } from './night-city';
import { getUiLanguage, tr } from './language';

export type UiThemeName = 'Light' | 'Dark' | 'Pink' | 'Blue' | 'NightCity';
export type UiNavigationIcon = 'home' | 'search' | 'settings' | 'robot' | 'cloud' | 'install' | 'proxy' | 'local' | 'downloads';
export interface UiSelection { gameId: string; gameName: string; profileId?: string }
const themes: readonly UiThemeName[] = ['Light', 'Dark', 'Pink', 'Blue', 'NightCity'];

export function isDarkUiTheme(name: UiThemeName): boolean {
  return name === 'Dark' || name === 'NightCity';
}

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
  const dark = isDarkUiTheme(name);
  root.style.colorScheme = dark ? 'dark' : 'light';
  root.classList.toggle('dark-theme', dark);
  document.body.classList.toggle('dark-theme', dark);
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
  if (name === 'NightCity') return { ...nightCityConfig, algorithm: Antd.theme.darkAlgorithm };
  const dark = name === 'Dark';
  const accent = { Light: '#176b57', Dark: '#7ac8a9', Pink: '#ba376d', Blue: '#346883' }[name];
  const text = { Light: '#192b29', Dark: '#e3ebe5', Pink: '#42263a', Blue: '#24333e' }[name];
  const muted = { Light: '#52625c', Dark: '#b2bfb6', Pink: '#75566a', Blue: '#516573' }[name];
  const surface = { Light: '#fcfdf9', Dark: '#202d28', Pink: '#fffbfd', Blue: '#fbfcfd' }[name];
  const accentSoft = `${accent}22`;
  const accentHover = `${accent}12`;
  return {
    cssVar: { key: `mintcat-${name}` },
    ...(dark ? { algorithm: Antd.theme.darkAlgorithm } : {}),
    token: {
      colorPrimary: accent,
      colorInfo: '#376d90',
      colorSuccess: '#28815d',
      colorWarning: '#aa7423',
      colorError: '#b24245',
      colorText: text,
      colorTextSecondary: muted,
      colorTextTertiary: dark ? '#96a59d' : muted,
      colorBgContainer: surface,
      colorBgElevated: surface,
      colorBgLayout: 'transparent',
      colorBorder: `${text}38`,
      colorBorderSecondary: `${text}24`,
      borderRadius: 6,
      borderRadiusSM: 4,
      borderRadiusLG: 9,
      controlHeight: 36,
      controlHeightSM: 28,
      fontSize: 13,
      fontFamily: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Segoe UI", sans-serif',
    },
    components: {
      Layout: { bodyBg: 'transparent', footerBg: 'transparent', headerBg: 'transparent', siderBg: 'transparent' },
      Menu: {
        itemBg: 'transparent',
        itemColor: muted,
        itemHoverBg: accentHover,
        itemHoverColor: text,
        itemSelectedBg: accentSoft,
        itemSelectedColor: accent,
        itemBorderRadius: 6,
      },
      Table: {
        colorBgContainer: 'transparent',
        headerBg: `${text}09`,
        headerColor: muted,
        headerSplitColor: 'transparent',
        rowHoverBg: accentHover,
        rowSelectedBg: accentSoft,
        rowSelectedHoverBg: `${accent}30`,
        cellPaddingBlock: 8,
        cellPaddingInline: 12,
      },
      Card: {
        colorBgContainer: surface,
        headerBg: 'transparent',
        paddingLG: 16,
      },
      Collapse: {
        headerBg: `${text}08`,
        contentBg: 'transparent',
      },
      Button: {
        defaultBg: surface,
        defaultBorderColor: `${text}38`,
        defaultShadow: 'none',
        primaryShadow: 'none',
        primaryColor: dark ? '#13211c' : '#fff',
        fontWeight: 500,
      },
      Select: {
        selectorBg: surface,
        optionSelectedBg: accentSoft,
      },
      Input: {
        colorBgContainer: surface,
        activeShadow: `0 0 0 2px ${accent}24`,
      },
      Modal: {
        contentBg: surface,
        headerBg: 'transparent',
      },
      Drawer: {
        colorBgElevated: surface,
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
    render(): ReactNode { return this.state.error ? React.createElement(Antd.Result, { status: 'error', title: tr('页面加载失败', 'Page failed to load'), subTitle: this.state.error.message }) : this.props.children; }
  }
  const render = (): void => {
    if (!closed) renderer.render(React.createElement(Antd.ConfigProvider, {
      theme: themeConfig(getUiTheme()), locale: getUiLanguage() === 'en-US' ? enUS : zhCN,
    }, React.createElement(Antd.App, { className: 'mintcat-ui-view' }, React.createElement(ViewBoundary, null, node))));
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
let lastSelection: UiSelection | undefined;

export function getLastUiSelection(): UiSelection | undefined {
  return lastSelection ? { ...lastSelection } : undefined;
}
export function subscribeUiSelection(listener: (selection: UiSelection) => void): () => void {
  const handler = (event: Event): void => {
    const detail = (event as CustomEvent<UiSelection>).detail;
    if (detail) listener({ ...detail });
  };
  window.addEventListener('mintcat-ui-selection', handler);
  return () => window.removeEventListener('mintcat-ui-selection', handler);
}
export function publishUiSelection(selection: UiSelection): void {
  lastSelection = { ...selection };
  window.dispatchEvent(new CustomEvent<UiSelection>('mintcat-ui-selection', { detail: selection }));
}
export function navigateUi(icon: UiNavigationIcon): void {
  window.dispatchEvent(new CustomEvent<UiNavigationIcon>('mintcat-ui-navigate', { detail: icon }));
}
