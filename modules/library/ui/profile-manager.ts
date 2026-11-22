import type { ChangeEvent, FormEvent, ReactElement } from 'react';
import { getSharedRuntime, tr } from '@mintcat/sdk-ui';
import type { LibraryActions, LibraryState } from './library';
import './profile-manager.css';

type Mode = 'list' | 'edit' | 'create' | 'clone';
const titles = { list: tr('管理配置', 'Manage profiles'), edit: tr('编辑配置', 'Edit profile'), create: tr('新建配置', 'New profile'), clone: tr('复制配置', 'Duplicate profile') };

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
    setName(next === 'edit' ? current!.displayName : next === 'clone' ? `${current!.displayName}${tr(' 副本', ' copy')}` : '');
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
  const mods = state.query.trim() || state.loading ? undefined : state.mods;
  const enabledModCount = mods?.items.reduce((count, item) => count + (item.enabled ? 1 : 0), 0);
  return h(Antd.Modal, { open: true, centered: true, width: 490, footer: null, className: 'mc-profiles',
    onCancel: saving ? undefined : onClose, closable: !saving, keyboard: !saving,
    title: h('div', { className: 'mc-profiles-heading' },
      h('span', { className: 'mc-profiles-game' }, game?.displayName), h('strong', null, titles[mode])) },
    mode === 'list' ? h(React.Fragment, null,
      current ? h('section', { className: 'mc-profile-plate', 'aria-label': tr('当前配置', 'Current profile') },
        h('span', { className: 'mc-profile-marker' }, tr('当前配置', 'Current profile')),
        h('h3', null, current.displayName),
        current.description && h('p', { className: 'mc-profile-description' }, current.description),
        mods && h('div', { className: 'mc-profile-counts' },
          h('span', null, tr(`${mods.total} 个模组`, `${mods.total} ${mods.total === 1 ? 'mod' : 'mods'}`)),
          h('span', null, tr(`${enabledModCount} 个已启用`, `${enabledModCount} enabled`))))
        : h('p', { className: 'mc-profiles-empty' }, state.installationId ? tr('尚无配置', 'No profiles yet') : tr('请先在设置中注册游戏安装目录。', 'Register a game installation in Settings first.')),
      others.length > 0 && h('div', { className: 'mc-profiles-others' }, h('h4', null, tr('切换配置', 'Switch profile')),
        h('ul', null, others.map(item => h('li', { key: item.profileId }, h('button', { type: 'button',
          onClick: () => actions.run(() => actions.selectProfile(item.profileId)) }, h(Icons.ProfileOutlined),
          h('span', null, h('strong', null, item.displayName), item.description && h('small', null, item.description)), h(Icons.RightOutlined)))))),
      h('div', { className: 'mc-profiles-actions' },
        h(Antd.Button, { type: 'primary', className: 'glass-primary-action', icon: h(Icons.PlusOutlined), disabled: !state.installationId, onClick: () => openForm('create') }, titles.create),
        current && h(React.Fragment, null,
          h(Antd.Button, { type: 'text', icon: h(Icons.EditOutlined), onClick: () => openForm('edit') }, titles.edit),
          h(Antd.Dropdown, { trigger: ['click'], placement: 'bottomRight', menu: { items: [
            { key: 'clone', label: titles.clone, icon: h(Icons.CopyOutlined), disabled: !writable }, { type: 'divider' },
            { key: 'delete', label: tr('删除配置', 'Delete profile'), icon: h(Icons.DeleteOutlined), danger: true },
          ], onClick: ({ key }: { key: string }) => key === 'clone' ? openForm('clone') : onDelete() } },
          h(Antd.Button, { type: 'text', 'aria-label': tr('更多配置操作', 'More profile actions'), title: tr('更多配置操作', 'More profile actions'), icon: h(Icons.EllipsisOutlined) })))))
    : h('form', { className: 'mc-profiles-form', onSubmit: submit },
      h('label', null, tr('配置名称', 'Profile name'), h(Antd.Input, { autoFocus: true, value: name, disabled: saving,
        'aria-label': tr('配置名称', 'Profile name'), onChange: (event: ChangeEvent<HTMLInputElement>) => setName(event.target.value) })),
      mode === 'edit' && h('label', null, tr('配置说明', 'Profile description'), h(Antd.Input.TextArea, { value: description, autoSize: { minRows: 2, maxRows: 5 },
        'aria-label': tr('配置说明', 'Profile description'), disabled: saving, onChange: (event: ChangeEvent<HTMLTextAreaElement>) => setDescription(event.target.value) })),
      error && h('p', { role: 'alert' }, error),
      h('div', { className: 'mc-profiles-form-actions' }, h(Antd.Button, { type: 'text', disabled: saving, onClick: () => setMode('list') }, tr('取消', 'Cancel')),
        h(Antd.Button, { type: 'primary', className: 'glass-primary-action', htmlType: 'submit', loading: saving, disabled: !name.trim() }, mode === 'edit' ? tr('保存', 'Save') : mode === 'clone' ? tr('创建副本', 'Create copy') : tr('创建', 'Create')))));
}
