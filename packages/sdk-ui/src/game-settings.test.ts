// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createOnlineModJobs, kit, mountGameSettings, SessionStore } from './index';
import type { Disposable, GameModuleList, Json, UiContext } from './index';

const inventory: GameModuleList = { generationId: 'games', modules: ['drg', 'rogue-core'].map(id => ({
  moduleId: `mintcat.game.${id}`, enabled: true, game: {
    gameId: `game:${id}`, displayName: id, descriptor: {
      gameId: `game:${id}`, displayName: id, settingsSchemaVersion: 1,
      settingsSchema: { type: 'object', properties: { rootPath: { type: 'string', title: '安装目录' } } },
      variants: [], sourceBindings: [],
    },
  },
})) };
const mounted: Disposable[] = [];
afterEach(() => { for (const page of mounted.splice(0)) page.dispose(); document.body.replaceChildren(); });

function host() {
  const states = new SessionStore();
  const values: Record<string, { values: Record<string, Json>; revision: number }> = {
    'game:drg': { values: { rootPath: '/drg' }, revision: 4 },
    'game:rogue-core': { values: { rootPath: '/rogue' }, revision: 9 },
  };
  const pending: Promise<unknown>[] = [];
  const request = vi.fn(async (method: string, params: Record<string, unknown>) => {
    if (method === 'modules.list') return inventory;
    const scope = params.scope;
    if (!scope || typeof scope !== 'object' || !('gameId' in scope) || typeof scope.gameId !== 'string') throw new Error('Missing explicit game scope');
    const gameId = scope.gameId;
    if (method === 'settings.get') return structuredClone(values[gameId]);
    if (method === 'settings.update') {
      expect(params.expectedRevision).toBe(values[gameId].revision);
      values[gameId] = { values: structuredClone(params.values as Record<string, Json>), revision: values[gameId].revision + 1 };
      return structuredClone(values[gameId]);
    }
    throw new Error(`Unexpected method ${method}`);
  });
  const context = (controller: AbortController): UiContext => ({
    // Deliberately unrelated to either game: this is a local state namespace, not identity.
    moduleId: 'desktop.settings', signal: controller.signal, kit,
    onlineModJobs: createOnlineModJobs(states),
    request: async <T>(method: string, params: Record<string, unknown>): Promise<T> => await request(method, params) as T,
    call: async () => { throw new Error('Unexpected typed command'); },
    query: (_key, load) => load(), invalidate() {},
    state: (key, version, fallback) => states.get(key, version, fallback),
    saveState: (key, version, value) => states.set(key, version, value),
    dirty: vi.fn(), notify: vi.fn(), own: value => value,
    onInventoryChanged: () => ({ dispose() {} }), interval: () => ({ dispose() {} }),
    guard: action => (...args) => { pending.push(Promise.resolve().then(() => action(...args))); },
  });
  return { states, values, request, context, async settle() { await Promise.all(pending.splice(0)); } };
}
function edit(root: HTMLElement, value: string): void {
  const input = root.querySelector('input')!; input.value = value; input.dispatchEvent(new Event('input', { bubbles: true }));
}
function submit(root: HTMLElement): void { root.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); }

describe('shared game settings renderer', () => {
  it('renders both descriptors and saves each explicit game with its own revision', async () => {
    const environment = host(); const controller = new AbortController();
    const drg = document.createElement('section'), rogue = document.createElement('section'); document.body.append(drg, rogue);
    mounted.push(await mountGameSettings(drg, environment.context(controller), 'game:drg'));
    mounted.push(await mountGameSettings(rogue, environment.context(controller), 'game:rogue-core'));
    expect(drg.querySelector('input')!.value).toBe('/drg'); expect(rogue.querySelector('input')!.value).toBe('/rogue');
    edit(drg, '/drg-edited'); edit(rogue, '/rogue-edited'); submit(drg); submit(rogue); await environment.settle();
    expect(environment.values).toEqual({
      'game:drg': { values: { rootPath: '/drg-edited' }, revision: 5 },
      'game:rogue-core': { values: { rootPath: '/rogue-edited' }, revision: 10 },
    });
  });
  it('restores only the selected game draft after page disposal', async () => {
    const environment = host(); const first = document.createElement('section');
    const page = await mountGameSettings(first, environment.context(new AbortController()), 'game:drg');
    edit(first, '/draft'); page.dispose();
    const second = document.createElement('section'), other = document.createElement('section');
    mounted.push(await mountGameSettings(second, environment.context(new AbortController()), 'game:drg'));
    mounted.push(await mountGameSettings(other, environment.context(new AbortController()), 'game:rogue-core'));
    expect(second.querySelector('input')!.value).toBe('/draft'); expect(other.querySelector('input')!.value).toBe('/rogue');
    expect(environment.values['game:drg'].revision).toBe(4);
  });
  it('retains a mutation accepted before disposal and does not resurrect an obsolete draft', async () => {
    const environment = host(); const controller = new AbortController(), root = document.createElement('section');
    const page = await mountGameSettings(root, environment.context(controller), 'game:drg');
    edit(root, '/accepted'); submit(root);
    // Let the request begin before the page is removed, as in the native request boundary.
    await Promise.resolve(); controller.abort(); page.dispose(); await environment.settle();
    expect(environment.states.get('settings/game:drg', 1, { values: {}, revision: 0, dirty: true })).toEqual({ values: { rootPath: '/accepted' }, revision: 5, dirty: false });
  });
});
