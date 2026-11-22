import type { ChangeEvent, FormEvent, ReactElement } from 'react';
import { getSharedRuntime } from '@mintcat/sdk-ui';
import type { LibraryActions, LibraryState } from './library';
import './profile-manager.css';

type Mode = 'list' | 'edit' | 'create' | 'clone';
const titles = { list: '管理配置', edit: '编辑配置', create: '新建配置', clone: '复制配置' };

export function ProfileManager({ state, actions, writable, onClose, onDelete }: {
  state: LibraryState; actions: LibraryActions; writable: boolean; onClose(): void; onDelete(): void;
}): ReactElement {
  const { React, Antd, Icons } = getSharedRuntime();
  const h = React.createElement;
  const [mode, setMode] = React.useState<Mode>('list');
  const [name, setName] = React.useState('');
  const [description, setDescription] = React.useState('');
  const [saving, setSaving] = React.useState(false);
  const [error, setError] = React.useState('');
  const current = state.profiles.find(item => item.profileId === state.profileId);
  const others = state.profiles.filter(item => item !== current);
  const game = state.games.find(item => item.gameId === state.gameId);
  const openForm = (next: Exclude<Mode, 'list'>): void => {
    setName(next === 'edit' ? current!.displayName : next === 'clone' ? `${current!.displayName} 副本` : '');
    setDescription(next === 'edit' ? current!.description : '');
    setError(''); setMode(next);
  };
  const submit = (event: FormEvent): void => {
    event.preventDefault();
    if (!name.trim() || saving) return;
    setSaving(true); setError('');
    actions.run(async () => {
      try {
        if (mode === 'edit') await actions.editProfile(name.trim(), description.trim());
        else if (mode === 'clone') await actions.cloneProfile(name.trim());
        else await actions.createProfile(name.trim());
        setMode('list');
      } catch (failure) {
        setError(failure instanceof Error ? failure.message : String(failure));
      } finally { setSaving(false); }
    });
  };
  const showModCount = state.mods && !state.query.trim() && !state.loading;
  return h(Antd.Modal, { open: true, centered: true, width: 490, footer: null, className: 'mc-profiles',
    onCancel: saving ? undefined : onClose, closable: !saving, keyboard: !saving,
    title: h('div', { className: 'mc-profiles-heading' },
      h('span', { className: 'mc-profiles-game' }, game?.displayName), h('strong', null, titles[mode])) },
    mode === 'list' ? h(React.Fragment, null,
      current ? h('section', { className: 'mc-profile-plate', 'aria-label': '当前配置' },
        h('span', { className: 'mc-profile-marker' }, '当前配置'),
        h('h3', null, current.displayName),
        current.description && h('p', { className: 'mc-profile-description' }, current.description),
        showModCount && h('div', { className: 'mc-profile-counts' }, h('span', null, `${state.mods!.total} 个模组`), h('span', null, `${state.mods!.items.filter(item => item.enabled).length} 个已启用`)))
        : h('p', { className: 'mc-profiles-empty' }, state.installationId ? '尚无配置' : '请先在设置中注册游戏安装目录。'),
      others.length > 0 && h('div', { className: 'mc-profiles-others' }, h('h4', null, '切换配置'),
        h('ul', null, others.map(item => h('li', { key: item.profileId }, h('button', { type: 'button',
          onClick: () => actions.run(() => actions.selectProfile(item.profileId)) }, h(Icons.ProfileOutlined),
          h('span', null, h('strong', null, item.displayName), item.description && h('small', null, item.description)), h(Icons.RightOutlined)))))),
      h('div', { className: 'mc-profiles-actions' },
        h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.PlusOutlined), disabled: !state.installationId, onClick: () => openForm('create') }, '新建配置'),
        current && h(React.Fragment, null,
          h(Antd.Button, { type: 'text', icon: h(Icons.EditOutlined), onClick: () => openForm('edit') }, '编辑配置'),
          h(Antd.Dropdown, { trigger: ['click'], placement: 'bottomRight', menu: { items: [
            { key: 'clone', label: '复制配置', icon: h(Icons.CopyOutlined), disabled: !writable }, { type: 'divider' },
            { key: 'delete', label: '删除配置', icon: h(Icons.DeleteOutlined), danger: true },
          ], onClick: ({ key }: { key: string }) => key === 'clone' ? openForm('clone') : onDelete() } },
          h(Antd.Button, { type: 'text', 'aria-label': '更多配置操作', title: '更多配置操作', icon: h(Icons.EllipsisOutlined) })))))
    : h('form', { className: 'mc-profiles-form', onSubmit: submit },
      h('label', null, '配置名称', h(Antd.Input, { autoFocus: true, value: name, disabled: saving,
        'aria-label': '配置名称', onChange: (event: ChangeEvent<HTMLInputElement>) => setName(event.target.value) })),
      mode === 'edit' && h('label', null, '配置说明', h(Antd.Input.TextArea, { value: description, autoSize: { minRows: 2, maxRows: 5 },
        'aria-label': '配置说明', disabled: saving, onChange: (event: ChangeEvent<HTMLTextAreaElement>) => setDescription(event.target.value) })),
      error && h('p', { role: 'alert' }, error),
      h('div', { className: 'mc-profiles-form-actions' }, h(Antd.Button, { type: 'text', disabled: saving, onClick: () => setMode('list') }, '取消'),
        h(Antd.Button, { type: 'primary', className: 'glass-primary-action', htmlType: 'submit', loading: saving, disabled: !name.trim() }, mode === 'edit' ? '保存' : mode === 'clone' ? '创建副本' : '创建'))));
}
