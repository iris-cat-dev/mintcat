import { describe, expect, it } from 'vitest';
import { Disposables, QueryCache, SessionStore } from '@mintcat/sdk-ui';

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
  it('restores compatible serialized state without sharing mutable objects between page mounts', () => {
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
