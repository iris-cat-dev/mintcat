import * as React from 'react';
import * as ReactDOMClient from 'react-dom/client';
import { createPortal, flushSync } from 'react-dom';
import * as Antd from 'antd';
import * as Icons from '@ant-design/icons';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { bindSharedRuntime, kit, setUiTheme, getUiTheme, getUiLanguage, tr } from '@mintcat/sdk-ui';
import type { FileDragEvent, PickLocalPathOptions, PickSavePathOptions } from '@mintcat/sdk-ui';

async function openExternalUrl(url: string): Promise<void> {
  if (isTauri()) {
    await invoke('open_external_url', { url });
    return;
  }
  window.open(url, '_blank', 'noopener,noreferrer');
}

async function openDataDir(dataDir: string): Promise<void> {
  if (!isTauri()) throw new Error(tr('仅桌面客户端支持打开本地数据目录', 'Opening the local data folder requires the desktop app'));
  await invoke('open_data_dir', { dataDir });
}

async function pickLocalPath(options: PickLocalPathOptions): Promise<string | null> {
  if (!isTauri()) return null;
  return await invoke<string | null>('pick_local_path', {
    kind: options.kind,
    title: options.title ?? null,
    extensions: options.extensions ?? null,
  });
}

async function pickSavePath(options: PickSavePathOptions): Promise<string | null> {
  if (!isTauri()) return null;
  return await invoke<string | null>('pick_save_path', {
    title: options.title ?? null,
    defaultName: options.defaultName ?? null,
    extensions: options.extensions ?? null,
  });
}

async function launchSteamGame(appId: string): Promise<void> {
  if (!isTauri()) throw new Error(tr('仅桌面客户端支持直接启动 Steam 游戏', 'Launching Steam games requires the desktop app'));
  await invoke('launch_steam_game', { appId });
}

function onFileDrag(handler: (event: FileDragEvent) => void): () => void {
  if (!isTauri()) return () => {};
  let disposed = false;
  let carriesFiles = false;
  let unlisten: (() => void) | undefined;
  void getCurrentWebview().onDragDropEvent(event => {
    if (disposed) return;
    const payload = event.payload;
    if (payload.type === 'leave') {
      carriesFiles = false;
      handler({ type: 'leave' });
      return;
    }
    if (payload.type === 'enter' || payload.type === 'drop') carriesFiles = payload.paths.length > 0;
    if (!carriesFiles) return;
    // Wry 0.53's WKWebView sends NSDraggingInfo points, despite the PhysicalPosition type.
    // WebView2 sends device pixels; account for the Windows display scale there.
    const scale = /Win/i.test(navigator.platform) ? window.devicePixelRatio : 1;
    const x = payload.position.x / scale, y = payload.position.y / scale;
    if (payload.type === 'drop') {
      carriesFiles = false;
      handler({ type: 'drop', x, y, paths: payload.paths });
    } else handler({ type: payload.type, x, y });
  }).then(fn => {
    if (disposed) fn();
    else unlisten = fn;
  }).catch(() => { /* WebView drag-drop registration failed; manual picker remains available. */ });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

bindSharedRuntime({
  React,
  ReactDOMClient,
  createPortal,
  flushSync,
  Antd,
  Icons,
  kit,
  openExternalUrl,
  openDataDir,
  pickLocalPath: isTauri() ? pickLocalPath : undefined,
  pickSavePath,
  launchSteamGame,
  onFileDrag,
});
setUiTheme(getUiTheme());
document.documentElement.lang = getUiLanguage();
