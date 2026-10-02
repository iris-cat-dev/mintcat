import type { Disposable, Json, SettingField, UiContext } from './index';
export interface GameUiDescriptor {
  gameId: string; displayName: string;
  settingsSchemaVersion: number;
  settingsSchema: { type: 'object'; properties: Record<string, { type: 'string' | 'number' | 'boolean'; title?: string; enum?: Json[] }>; required?: string[] };
  variants: Array<{ variantId: string; displayName: string }>;
  sourceBindings: Array<{ sourceId: string; remoteGameKey: string }>;
}
export interface GameModuleList { generationId: string; modules: Array<{ moduleId: string; enabled: boolean; game?: { gameId: string; displayName: string; descriptor?: GameUiDescriptor } }> }
export async function mountGameSettings(root: HTMLElement, context: UiContext, gameId?: string, presentation: readonly SettingField[] = []): Promise<Disposable> {
  const list = await context.request<GameModuleList>('modules.list', {});
  const binding = list.modules.find(module => module.enabled && (gameId ? module.game?.gameId === gameId : module.moduleId === context.moduleId))?.game;
  if (!binding?.descriptor) throw new Error('Signed game settings descriptor unavailable');
  const descriptor = binding.descriptor;
  const scope = { kind: 'game', gameId: binding.gameId };
  const result = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', { scope, section: 'game' });
  const stateVersion = descriptor.settingsSchemaVersion;
  if (!Number.isSafeInteger(stateVersion) || stateVersion < 1) throw new Error('Signed settings schema version is invalid');
  const saved = context.state<{ values: Record<string, Json>; revision: number; dirty: boolean }>(`settings/${binding.gameId}`, stateVersion, { values: result.values, revision: result.revision, dirty: false });
  const values = saved.dirty ? saved.values : result.values;
  let revision = saved.dirty ? saved.revision : result.revision;
  let dirty = saved.dirty;
  let editGeneration = 0;
  let pending = false;
  const controls = new Map<string, HTMLInputElement | HTMLSelectElement>();
  const saveDraft = (): void => context.saveState(`settings/${binding.gameId}`, stateVersion, { values, revision, dirty });
  context.dirty(dirty);
  const form = context.kit.element('form');
  root.classList.add('game-settings');
  root.append(context.kit.element('h2', `${binding.displayName} settings`), context.kit.element('p', `Game scope: ${binding.gameId}`), form);
  const fields = presentation.length ? presentation.map(field => {
    const property = descriptor.settingsSchema.properties[field.key];
    if (!property || property.type !== field.type) throw new Error(`Contributed setting does not match signed game schema: ${field.key}`);
    return [field.key, { ...property, title: field.label }] as const;
  }) : Object.entries(descriptor.settingsSchema.properties);
  for (const [key, field] of fields) {
    const label = context.kit.element('label', field.title ?? key);
    if (field.enum || field.type === 'boolean') {
      const select = context.kit.element('select'); controls.set(key, select);
      const unset = context.kit.element('option', `Choose ${field.title ?? key}`); unset.value = ''; unset.disabled = true; select.append(unset);
      select.required = descriptor.settingsSchema.required?.includes(key) ?? false;
      for (const value of field.enum ?? [true, false]) { const option = context.kit.element('option', String(value)); option.value = JSON.stringify(value); select.append(option); }
      select.value = values[key] === undefined ? '' : JSON.stringify(values[key]);
      select.addEventListener('change', () => { values[key] = JSON.parse(select.value) as Json; editGeneration++; dirty = true; context.dirty(true); saveDraft(); });
      label.append(select);
    } else {
      const input = context.kit.element('input'); controls.set(key, input);
      input.type = field.type === 'number' ? 'number' : 'text';
      input.required = descriptor.settingsSchema.required?.includes(key) ?? false;
      input.value = String(values[key] ?? '');
      input.addEventListener('input', () => { if (field.type === 'number' && input.value === '') delete values[key]; else values[key] = field.type === 'number' ? Number(input.value) : input.value; editGeneration++; dirty = true; context.dirty(true); saveDraft(); });
      label.append(input);
    }
    form.append(label);
  }
  const replaceValues = (authoritative: Record<string, Json>): void => {
    for (const key of Object.keys(values)) delete values[key];
    Object.assign(values, authoritative);
    for (const [key, control] of controls) control.value = control instanceof HTMLSelectElement ? values[key] === undefined ? '' : JSON.stringify(values[key]) : String(values[key] ?? '');
  };
  const save = context.kit.button('Save game settings', () => { form.requestSubmit(); });
  const discard = context.kit.button('Discard draft and reload', context.guard(async () => {
    if (dirty && !window.confirm('Discard unsaved game settings and reload the authoritative values?')) return;
    if (pending) return;
    pending = true;
    const discardedGeneration = editGeneration;
    save.disabled = discard.disabled = true;
    try {
      const current = await context.request<{ values: Record<string, Json>; revision: number }>('settings.get', { scope, section: 'game' });
      if (editGeneration !== discardedGeneration) { context.notify('Newer edits remain unsaved; reload was not applied'); return; }
      replaceValues(current.values); revision = current.revision; dirty = false; editGeneration++;
      context.dirty(false); saveDraft(); context.notify('Game settings reloaded; unsaved draft discarded');
    } catch (error) { context.notify(error instanceof Error ? error.message : String(error)); }
    finally { pending = false; if (!context.signal.aborted) save.disabled = discard.disabled = false; }
  }));
  form.append(save, discard);
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
          context.notify(dirty ? 'Submitted settings saved; newer edits remain unsaved' : 'Game settings saved');
        }
      } catch (error) { context.notify(error instanceof Error ? error.message : String(error)); }
      finally { pending = false; if (!context.signal.aborted) save.disabled = discard.disabled = false; }
    })();
  });
  return { dispose: () => { saveDraft(); context.dirty(false); root.replaceChildren(); } };
}
