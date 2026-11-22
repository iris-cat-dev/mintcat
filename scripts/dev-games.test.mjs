import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { provisionOnlineGames } from './dev-games.mjs';

const runtime = {
  schemaVersion: 2,
  trustRoot: 'root.json',
  verificationPolicy: 'policy.json',
  repositoryPath: 'repository',
  scopes: { 'game:drg': { sequence: 3, releaseId: 'dev-drg', channel: 'stable' } },
};

async function cachedRuntime(t, config) {
  const directory = await mkdtemp(join(tmpdir(), 'mintcat-dev-games-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const online = join(directory, 'online');
  await mkdir(online);
  const path = join(online, 'runtime.json');
  const original = JSON.stringify(config);
  await writeFile(path, original);
  await provisionOnlineGames(process.cwd(), directory);
  return { original, updated: await readFile(path, 'utf8') };
}

test('upgrades generated configs missing online sources without changing game release pins', async (t) => {
  const { updated } = await cachedRuntime(t, runtime);
  const { sources, ...unchanged } = JSON.parse(updated);
  assert.deepEqual(unchanged, runtime);
  assert.deepEqual(Object.keys(sources).sort(), ['modcat', 'modio', 'nexusmods']);
});

test('keeps generated configs with explicitly disabled online sources offline', async (t) => {
  const { original, updated } = await cachedRuntime(t, { ...runtime, sources: {} });
  assert.equal(updated, original);
});
