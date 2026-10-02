import * as React from 'react';
import * as ReactDOMClient from 'react-dom/client';
import { createPortal, flushSync } from 'react-dom';
import * as Antd from 'antd';
import * as Icons from '@ant-design/icons';
import { bindSharedRuntime, kit, setUiTheme, getUiTheme } from '@mintcat/sdk-ui';

bindSharedRuntime({ React, ReactDOMClient, createPortal, flushSync, Antd, Icons, kit });
setUiTheme(getUiTheme());
