import { describe, expect, it } from 'vitest';
import { Disposables, QueryCache, SessionStore, validateUiManifest } from '@mintcat/sdk-ui';

const manifest = {
  schemaVersion: 1, runtimeVersion: 1, moduleId: 'mintcat.example', entry: 'ui/index.js', css: [],
  pages: [{ id: 'mintcat.example/page', route: '/mintcat.example/page', title: 'Example', export: 'mountPage', stateVersion: 1 }],
  navigation: [{ id: 'mintcat.example/nav', pageId: 'mintcat.example/page', order: 1 }], commands: [], settings: [], games: [],
};

describe('signed module contribution boundaries', () => {
  it('rejects imports and styles outside the exact signed resource list', () => {
    expect(() => validateUiManifest({ ...manifest, entry: 'https://example.org/payload.js' }, ['ui/index.js'])).toThrow();
    expect(() => validateUiManifest({ ...manifest, css: ['ui/changed.css'] }, ['ui/index.js'])).toThrow();
    expect(() => validateUiManifest({ ...manifest, entry: 'ui/../payload.js' }, ['ui/../payload.js'])).toThrow();
  });
  it('rejects foreign routes and ambiguous navigation targets', () => {
    expect(() => validateUiManifest({ ...manifest, pages: [{ ...manifest.pages[0], route: '/other/page' }] }, ['ui/index.js'])).toThrow();
    expect(() => validateUiManifest({ ...manifest, navigation: [{ ...manifest.navigation[0], pageId: 'mintcat.other/page' }] }, ['ui/index.js'])).toThrow();
    expect(() => validateUiManifest({ ...manifest, pages: [manifest.pages[0], manifest.pages[0]] }, ['ui/index.js'])).toThrow();
  });
  it('rejects a page that would shadow a contributed settings route', () => {
    expect(() => validateUiManifest({
      ...manifest,
      pages: [{ ...manifest.pages[0], route: '/mintcat.example/settings/mintcat.example%2Fgame' }],
      settings: [{ id: 'mintcat.example/game', title: 'Game settings', gameId: 'example-game', fields: [] }],
    }, ['ui/index.js'])).toThrow();
  });
});

describe('UI state and lifetime', () => {
  it('does not let a stale in-flight query overwrite refreshed authority after invalidation', async () => {
    const cache = new QueryCache();
    let finishOld!: (value: string) => void;
    const old = cache.get('module/profile', () => new Promise<string>(resolve => { finishOld = resolve; }));
    cache.invalidate('module/');
    const current = await cache.get('module/profile', async () => 'new revision');
    finishOld('old revision'); await old;
    expect(current).toBe('new revision');
    expect(await cache.get('module/profile', async () => 'unexpected reload')).toBe('new revision');
  });
  it('restores compatible serialized state without sharing mutable objects between generations', () => {
    const sessions = new SessionStore();
    sessions.set('module/page', 2, { query: 'saved', selected: ['profile-a'] });
    const first = sessions.get('module/page', 2, { query: '', selected: [] as string[] });
    first.selected.push('profile-b');
    expect(sessions.get('module/page', 2, { query: '', selected: [] as string[] })).toEqual({ query: 'saved', selected: ['profile-a'] });
    expect(sessions.get('module/page', 3, { query: 'migration fallback' })).toEqual({ query: 'migration fallback' });
  });
  it('unsubscribes every owned resource exactly once, including late registrations', () => {
    const lifetime = new Disposables(); const released: string[] = [];
    lifetime.add({ dispose: () => { released.push('listener'); } });
    lifetime.add({ dispose: () => { released.push('timer'); } });
    lifetime.dispose(); lifetime.dispose();
    lifetime.add({ dispose: () => { released.push('late'); } });
    expect(released).toEqual(['timer', 'listener', 'late']);
  });
});
