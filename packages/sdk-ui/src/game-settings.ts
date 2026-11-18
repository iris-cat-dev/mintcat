import type { Disposable, Json, UiContext } from './index';
export interface GameUiDescriptor {
  gameId: string; displayName: string;
  settingsSchemaVersion: number;
  settingsSchema: { type: 'object'; properties: Record<string, { type: 'string' | 'number' | 'boolean'; title?: string; enum?: Json[] }>; required?: string[] };
  variants: Array<{ variantId: string; displayName: string }>;
  sourceBindings: Array<{ sourceId: string; remoteGameKey: string }>;
}
export interface GameModuleList { generationId: string; modules: Array<{ moduleId: string; digest?: string; enabled: boolean; game?: { gameId: string; displayName: string; descriptor?: GameUiDescriptor } }> }
export async function mountGameSettings(root: HTMLElement, context: UiContext, gameId: string): Promise<Disposable> {
  const list = await context.request<GameModuleList>('modules.list', {});
  const binding = list.modules.find(module => module.enabled && module.game?.gameId === gameId)?.game;
  if (!binding?.descriptor) throw new Error('Game settings descriptor unavailable');
  const descriptor = binding.descriptor;
  const scope = { kind: 'game', gameId: binding.gameId };
  const result = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', { scope, section: 'game' });
  const stateVersion = descriptor.settingsSchemaVersion;
  if (!Number.isSafeInteger(stateVersion) || stateVersion < 1) throw new Error('Game settings schema version is invalid');
  const saved = context.state<{ values: Record<string, Json>; revision: number; dirty: boolean }>(`settings/${binding.gameId}`, stateVersion, { values: result.values, revision: result.revision, dirty: false });
  const values = saved.dirty ? saved.values : result.values;
  let revision = saved.dirty ? saved.revision : result.revision;
  let dirty = saved.dirty;
  let editGeneration = 0;
  let pending = false;
  const controls = new Map<string, HTMLInputElement | HTMLSelectElement>();
  const revisionBadge = context.kit.element('span');
  revisionBadge.className = 'game-settings-badge';
  const dirtyBadge = context.kit.element('span');
  const syncBadges = (): void => {
    revisionBadge.textContent = `修订 #${revision}`;
    dirtyBadge.className = `game-settings-badge ${dirty ? 'game-settings-badge--dirty' : 'game-settings-badge--clean'}`;
    dirtyBadge.textContent = dirty ? '未保存修改' : '已同步';
  };
  const saveDraft = (): void => {
    syncBadges();
    context.saveState(`settings/${binding.gameId}`, stateVersion, { values, revision, dirty });
  };
  context.dirty(dirty);
  syncBadges();
  const card = context.kit.element('div');
  card.className = 'game-settings-card';
  const header = context.kit.element('div');
  header.className = 'game-settings-header';
  const heading = context.kit.element('div');
  heading.className = 'game-settings-heading';
  const title = context.kit.element('h2', `${binding.displayName} 游戏设置`);
  title.className = 'game-settings-title';
  const subtitle = context.kit.element('p', `配置当前游戏的启动参数与运行选项 · 作用域 ${binding.gameId}`);
  subtitle.className = 'game-settings-subtitle';
  heading.append(title, subtitle);
  const badges = context.kit.element('div');
  badges.className = 'game-settings-badges';
  badges.append(revisionBadge, dirtyBadge);
  header.append(heading, badges);

  const form = context.kit.element('form');
  form.className = 'game-settings-form';
  const fieldsContainer = context.kit.element('div');
  fieldsContainer.className = 'game-settings-fields';
  root.classList.add('game-settings');
  const fields = Object.entries(descriptor.settingsSchema.properties);
  for (const [key, field] of fields) {
    const label = context.kit.element('label');
    label.className = 'game-settings-field';
    const meta = context.kit.element('span');
    meta.className = 'game-settings-field-meta';
    const fieldTitle = context.kit.element('span', field.title ?? key);
    fieldTitle.className = 'game-settings-field-title';
    const fieldKey = context.kit.element('span', key);
    fieldKey.className = 'game-settings-field-key';
    meta.append(fieldTitle, fieldKey);
    label.append(meta);
    if (field.enum || field.type === 'boolean') {
      const select = context.kit.element('select');
      select.className = 'game-settings-control';
      controls.set(key, select);
      const unset = context.kit.element('option', `请选择 ${field.title ?? key}`);
      unset.value = ''; unset.disabled = true; select.append(unset);
      select.required = descriptor.settingsSchema.required?.includes(key) ?? false;
      for (const value of field.enum ?? [true, false]) {
        const display = field.type === 'boolean' && !field.enum
          ? (value === true ? '启用 (true)' : '禁用 (false)')
          : String(value);
        const option = context.kit.element('option', display);
        option.value = JSON.stringify(value);
        select.append(option);
      }
      select.value = values[key] === undefined ? '' : JSON.stringify(values[key]);
      select.addEventListener('change', () => { values[key] = JSON.parse(select.value) as Json; editGeneration++; dirty = true; context.dirty(true); saveDraft(); });
      label.append(select);
    } else {
      const input = context.kit.element('input');
      input.className = 'game-settings-control';
      controls.set(key, input);
      input.type = field.type === 'number' ? 'number' : 'text';
      input.placeholder = field.type === 'number' ? '输入数值…' : '输入配置内容…';
      input.required = descriptor.settingsSchema.required?.includes(key) ?? false;
      input.value = String(values[key] ?? '');
      input.addEventListener('input', () => { if (field.type === 'number' && input.value === '') delete values[key]; else values[key] = field.type === 'number' ? Number(input.value) : input.value; editGeneration++; dirty = true; context.dirty(true); saveDraft(); });
      label.append(input);
    }
    fieldsContainer.append(label);
  }
  if (!fields.length) {
    const empty = context.kit.element('p', '当前游戏插件未声明额外设置项。');
    empty.className = 'game-settings-empty';
    fieldsContainer.append(empty);
  }
  const replaceValues = (authoritative: Record<string, Json>): void => {
    for (const key of Object.keys(values)) delete values[key];
    Object.assign(values, authoritative);
    for (const [key, control] of controls) control.value = control instanceof HTMLSelectElement ? values[key] === undefined ? '' : JSON.stringify(values[key]) : String(values[key] ?? '');
  };
  const save = context.kit.button('保存设置', () => { form.requestSubmit(); });
  save.className = 'game-settings-save';
  const discard = context.kit.button('放弃草稿并重新加载', context.guard(async () => {
    if (dirty && !window.confirm('放弃未保存的游戏设置修改，并重新读取当前生效值？')) return;
    if (pending) return;
    pending = true;
    const discardedGeneration = editGeneration;
    save.disabled = discard.disabled = true;
    try {
      const current = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', { scope, section: 'game' });
      if (editGeneration !== discardedGeneration) { context.notify('存在更新的未保存修改，未覆盖当前草稿'); return; }
      replaceValues(current.values); revision = current.revision; dirty = false; editGeneration++;
      context.dirty(false); saveDraft(); context.notify('已重新加载游戏设置并丢弃草稿');
    } catch (error) { context.notify(error instanceof Error ? error.message : String(error)); }
    finally { pending = false; if (!context.signal.aborted) save.disabled = discard.disabled = false; }
  }));
  discard.className = 'game-settings-discard';
  const actionsBar = context.kit.element('div');
  actionsBar.className = 'game-settings-actions';
  actionsBar.append(save, discard);
  form.append(fieldsContainer, actionsBar);
  card.append(header, form);
  root.append(card);
  form.addEventListener('submit', event => {
    event.preventDefault();
    context.guard(async () => {
      if (pending) return;
      pending = true;
      save.disabled = discard.disabled = true;
      const submittedValues = structuredClone(values); const submittedGeneration = editGeneration;
      try {
        const updated = await context.request<{ values: Record<string, Json>; revision: number }>('settings.update', { scope, section: 'game', values: submittedValues, expectedRevision: revision }, crypto.randomUUID());
        revision = updated.revision;
        if (context.signal.aborted) {
          const latest = context.state<{ values: Record<string, Json>; revision: number; dirty: boolean }>(`settings/${binding.gameId}`, stateVersion, { values: submittedValues, revision, dirty: false });
          const retainedDirty = latest.dirty && JSON.stringify(latest.values) !== JSON.stringify(submittedValues);
          if (latest.revision <= revision) context.saveState(`settings/${binding.gameId}`, stateVersion, { values: retainedDirty ? latest.values : updated.values, revision, dirty: retainedDirty });
        } else {
          dirty = editGeneration !== submittedGeneration;
          if (!dirty) replaceValues(updated.values);
          context.dirty(dirty); saveDraft();
          context.notify(dirty ? '已提交保存，较新的输入仍保留在草稿中' : '游戏设置已保存');
        }
      } catch (error) { context.notify(error instanceof Error ? error.message : String(error)); }
      finally { pending = false; if (!context.signal.aborted) save.disabled = discard.disabled = false; }
    })();
  });
  return { dispose: () => { saveDraft(); context.dirty(false); root.replaceChildren(); } };
}
