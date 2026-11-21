import * as React from 'react';
import * as ReactDOMClient from 'react-dom/client';
import { createPortal, flushSync } from 'react-dom';
import * as Antd from 'antd';
import * as Icons from '@ant-design/icons';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { bindSharedRuntime, kit, setUiTheme, getUiTheme } from '@mintcat/sdk-ui';
import type { PickLocalPathOptions, PickSavePathOptions } from '@mintcat/sdk-ui';

async function openExternalUrl(url: string): Promise<void> {
  if (isTauri()) {
    await invoke('open_external_url', { url });
    return;
  }
  window.open(url, '_blank', 'noopener,noreferrer');
}

async function openDataDir(dataDir: string): Promise<void> {
  if (!isTauri()) throw new Error('仅桌面客户端支持打开本地数据目录');
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
  if (!isTauri()) throw new Error('仅桌面客户端支持直接启动 Steam 游戏');
  await invoke('launch_steam_game', { appId });
}

function onFileDrop(handler: (paths: string[]) => void): () => void {
  if (!isTauri()) return () => {};
  let disposed = false;
  let unlisten: (() => void) | undefined;
  void getCurrentWebview().onDragDropEvent(event => {
    if (disposed) return;
    if (event.payload.type === 'drop' && Array.isArray(event.payload.paths) && event.payload.paths.length > 0) {
      handler(event.payload.paths);
    }
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
  pickLocalPath,
  pickSavePath,
  launchSteamGame,
  onFileDrop,
});
setUiTheme(getUiTheme());
