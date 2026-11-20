import * as React from 'react';
import * as ReactDOMClient from 'react-dom/client';
import { createPortal, flushSync } from 'react-dom';
import * as Antd from 'antd';
import * as Icons from '@ant-design/icons';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { bindSharedRuntime, kit, setUiTheme, getUiTheme } from '@mintcat/sdk-ui';

async function openExternalUrl(url: string): Promise<void> {
  if (isTauri()) {
    await invoke('open_external_url', { url });
    return;
  }
  window.open(url, '_blank', 'noopener,noreferrer');
}

bindSharedRuntime({ React, ReactDOMClient, createPortal, flushSync, Antd, Icons, kit, openExternalUrl });
setUiTheme(getUiTheme());
