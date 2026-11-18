import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { gameInputs } from './build-app.mjs';

const target = 'aarch64-apple-darwin';
const put = (path, value) => writeFileSync(path, JSON.stringify(value));


test('rejects obsolete scopes and mismatched game targets before assembly', () => {
  const root = mkdtempSync(join(tmpdir(), 'mintcat-game-input-'));
  try {
    const pin = `sha256:${'1'.repeat(64)}`;
    const artifact = join(root, 'repository/artifacts/sha256', pin.slice(7));
    mkdirSync(artifact, { recursive: true });
    put(join(artifact, 'module.json'), { id: 'game-drg' });
    put(join(artifact, 'signature.json'), { algorithm: 'ed25519' });
    put(join(root, 'root.json'), {});
    put(join(root, 'policy.json'), { target });
    put(join(root, 'repository/current.json'), { targets: { drg: { signed: { release: { scopeId: 'game:drg' }, packages: { [pin]: { manifest: { id: 'game-drg' } } } } } } });
    const runtime = join(root, 'runtime.json');
    const config = { schemaVersion: 2, trustRoot: 'root.json', verificationPolicy: 'policy.json', repositoryPath: 'repository', scopes: { 'game:drg': { releaseId: 'drg', sequence: 1, channel: 'stable' } } };
    put(runtime, config);
    assert.throws(() => gameInputs(runtime, 'x86_64-pc-windows-msvc'));
    put(runtime, { ...config, scopes: { platform: {} } });
    assert.throws(() => gameInputs(runtime, target));
    put(runtime, { ...config, schemaVersion: 1 });
    assert.throws(() => gameInputs(runtime, target));
  } finally { rmSync(root, { recursive: true }); }
});

