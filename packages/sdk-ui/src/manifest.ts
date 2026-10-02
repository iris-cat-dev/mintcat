import type { UiManifest } from './index';

function record(value: unknown, fields: string[]): asserts value is Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Contribution must be an object');
  for (const key of Object.keys(value)) if (!fields.includes(key)) throw new Error(`Unknown contribution field: ${key}`);
}
function text(value: unknown): asserts value is string {
  if (typeof value !== 'string' || !value.trim() || value.length > 2048) throw new Error('Contribution string is invalid');
}
function list(value: unknown): asserts value is unknown[] {
  if (!Array.isArray(value) || value.length > 200) throw new Error('Contribution list is invalid');
}
export function validateUiManifest(value: unknown, files: readonly string[]): UiManifest {
  record(value, ['schemaVersion','runtimeVersion','moduleId','entry','css','pages','navigation','commands','settings','games']);
  if (value.schemaVersion !== 1 || value.runtimeVersion !== 1) throw new Error('Unsupported UI ABI');
  text(value.moduleId);
  if (!/^[a-z0-9][a-z0-9.-]+$/.test(value.moduleId)) throw new Error('Invalid UI module ID');
  const moduleId = value.moduleId;
  const id = (entry: unknown): void => { text(entry); if (!entry.startsWith(`${moduleId}/`)) throw new Error('Contribution ID must be module namespaced'); };
  const asset = (path: unknown): void => { text(path); if (!path.startsWith('ui/') || path.includes('..') || !files.includes(path)) throw new Error('Resource is absent from signed file list'); };
  asset(value.entry);
  if (!(value.entry as string).endsWith('.js') && !(value.entry as string).endsWith('.mjs')) throw new Error('UI entry must be ESM');
  for (const key of ['css','pages','navigation','commands','settings','games']) list(value[key]);
  for (const css of value.css as unknown[]) { asset(css); if (!(css as string).endsWith('.css')) throw new Error('Style entry must be CSS'); }
  const pageIds = new Set<string>();
  const routes = new Set<string>();
  for (const page of value.pages as unknown[]) {
    record(page, ['id','route','title','export','stateVersion']); id(page.id); text(page.route); text(page.title); text(page.export);
    if (!page.route.startsWith(`/${moduleId}/`) || !/^[a-zA-Z_$][\w$]*$/.test(page.export) || !Number.isSafeInteger(page.stateVersion) || (page.stateVersion as number) < 1) throw new Error('Invalid page declaration');
    if (pageIds.has(page.id as string) || routes.has(page.route)) throw new Error('Duplicate page or route');
    pageIds.add(page.id as string); routes.add(page.route);
  }
  const ids = new Set<string>(pageIds);
  for (const navigation of value.navigation as unknown[]) {
    record(navigation, ['id','pageId','order','icon','placement']); id(navigation.id); text(navigation.pageId);
    if (!pageIds.has(navigation.pageId) || !Number.isFinite(navigation.order)) throw new Error('Invalid navigation target/order');
    if (navigation.icon !== undefined && !['home','search','settings','robot','cloud','install','proxy','local','downloads'].includes(navigation.icon as string)) throw new Error('Invalid navigation icon');
    if (navigation.placement !== undefined && !['sidebar','titlebar','settings'].includes(navigation.placement as string)) throw new Error('Invalid navigation placement');
  }
  for (const command of value.commands as unknown[]) {
    record(command, ['id','title','method','params','enableWhen']); id(command.id); text(command.title); text(command.method);
    if (!command.params || typeof command.params !== 'object' || Array.isArray(command.params)) throw new Error('Command params must be an object');
    if (command.enableWhen !== undefined) { record(command.enableWhen, ['gameId']); text(command.enableWhen.gameId); }
  }
  for (const settings of value.settings as unknown[]) {
    record(settings, ['id','title','gameId','fields']); id(settings.id); text(settings.title); text(settings.gameId); list(settings.fields);
    const route = `/${moduleId}/settings/${encodeURIComponent(settings.id as string)}`;
    if (routes.has(route)) throw new Error('Duplicate page or settings route');
    routes.add(route);
    const keys = new Set<string>();
    for (const field of settings.fields) {
      record(field, ['key','label','type']); text(field.key); text(field.label);
      if (!['string','number','boolean'].includes(field.type as string) || keys.has(field.key)) throw new Error('Invalid or duplicate settings field');
      keys.add(field.key);
    }
  }
  for (const game of value.games as unknown[]) { record(game, ['gameId','displayName','adapterContract']); text(game.gameId); text(game.displayName); text(game.adapterContract); }
  for (const entry of [...value.navigation as Record<string, unknown>[], ...value.commands as Record<string, unknown>[], ...value.settings as Record<string, unknown>[]]) {
    const entryId = entry.id as string;
    if (ids.has(entryId)) throw new Error('Duplicate contribution ID'); ids.add(entryId);
  }
  return value as unknown as UiManifest;
}
