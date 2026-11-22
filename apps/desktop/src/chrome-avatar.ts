import type { ReactElement } from 'react';
import { getSharedRuntime, navigateUi, tr } from '@mintcat/sdk-ui';
import './chrome-avatar.css';

function GuestPortrait({ size }: { size: number }): ReactElement {
  const h = getSharedRuntime().React.createElement;
  return h('svg', { width: size, height: size, viewBox: '0 0 32 32', fill: 'none', 'aria-hidden': true, focusable: false },
    h('circle', { cx: 16, cy: 16, r: 15.5, fill: '#b8e4ca', stroke: '#70ab92' }),
    h('path', { d: 'M5.6 19.4 6.9 7.3l7.2 4.4a12 12 0 0 1 4.2 0l6.9-4.4 1.3 12.1c0 4.5-4.2 7.4-10.4 7.4S5.6 23.9 5.6 19.4Z', fill: '#225749' }),
    h('path', { d: 'm8.8 10.8 3.1 2-3.6 1.6Zm14.6 0-3.1 2 3.6 1.6Z', fill: '#82bda3' }),
    h('path', { d: 'M11.1 17.6v1.8m10-1.8v1.8', stroke: '#faf5d8', strokeWidth: 2.3, strokeLinecap: 'round' }),
    h('path', { d: 'M14.4 20.7h3.4l-1.7 1.8Z', fill: '#f4bfaa' }));
}

export function AvatarButton({ canInstall, canManageSources }: { canInstall: boolean; canManageSources: boolean }): ReactElement {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  const [open, setOpen] = React.useState(false);
  const label = tr('用户菜单', 'User menu');
  const identity = h('div', { className: 'chrome-avatar-identity' }, h(GuestPortrait, { size: 36 }),
    h('div', null, h('strong', null, tr('本机访客', 'Local guest')), h('small', null, tr('模组源凭据独立管理', 'Mod source credentials are managed separately'))));

  return h(Antd.Dropdown, {
    trigger: ['click'], placement: 'bottomRight', open, onOpenChange: setOpen, autoFocus: true,
    menu: {
      items: [{ type: 'group', key: 'local-identity', label: identity, children: [
        { key: 'sources', label: tr('模组源凭据', 'Mod source credentials'), icon: h(Icons.KeyOutlined), disabled: !canManageSources },
        { key: 'install', label: tr('安装与打包', 'Installation and packaging'), icon: h(Icons.DeploymentUnitOutlined), disabled: !canInstall },
      ] }],
      onClick: ({ key }) => {
        setOpen(false);
        if (key === 'sources') navigateUi('proxy');
        if (key === 'install') navigateUi('install');
      },
    },
  }, h(Antd.Button, {
    type: 'text', className: 'chrome-avatar-button', icon: h(GuestPortrait, { size: 28 }),
    title: label, 'aria-label': label, 'aria-haspopup': 'menu', 'aria-expanded': open,
  }));
}
