#!/usr/bin/env node
// Uses real supplied product binaries and one real game project, isolated fixture keys only.
import { execFileSync } from 'node:child_process';
import { cpSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash, randomBytes } from 'node:crypto';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: Object.fromEntries(['bin', 'project', 'target', 'root', 'cli', 'daemon'].map(name => [name, { type: 'string' }])) });
for (const name of ['bin', 'project', 'target', 'root', 'cli', 'daemon']) if (!values[name]) throw new Error(`Required --${name}`);
const binary = resolve(values.bin); const source = resolve(values.project); const root = resolve(values.root);
mkdirSync(root);
const json = (name, value) => { const file = join(root, name); writeFileSync(file, JSON.stringify(value, null, 2), { flag: 'wx' }); return file; };
const run = (...args) => { const output = execFileSync(binary, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }); process.stdout.write(output); return output; };
const reject = (expected, ...args) => {
  try { execFileSync(binary, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }); }
  catch (error) { if (error.status !== 0 && String(error.stderr).includes(expected)) return; throw error; }
  throw new Error(`Untrusted input admitted: ${args.join(' ')}`);
};
const digest = file => `sha256:${createHash('sha256').update(readFileSync(file)).digest('hex')}`;
const key = join(root, 'TEST-ONLY-ed25519.seed');
writeFileSync(key, randomBytes(32).toString('base64'), { flag: 'wx', mode: 0o600 });
run('check', source, '--target', values.target);
const packagePath = join(root, 'package');
const record = JSON.parse(run('pack', source, '--target', values.target, '--key', key, '--output', packagePath));
const identity = JSON.parse(run('release', 'key-info', '--key', key));
const expires = Math.floor(Date.now() / 1000) + 3600;
const role = { keyIds: [identity.keyId], threshold: 1 };
const spec = json('root-spec.json', { header: { kind: 'root', schemaVersion: 1, version: 1, expires }, keys: { [identity.keyId]: identity.publicKey }, roles: { root: role, targets: role, snapshot: role, timestamp: role }, publishers: { [record.manifest.publisher]: { [identity.keyId]: identity.publicKey } } });
const pinned = join(root, 'root.json'); run('release', 'sign-root', '--spec', spec, '--key', key, '--output', pinned);
const project = JSON.parse(readFileSync(join(source, 'module-project.json'), 'utf8'));
const policy = json('policy.json', { ...project.policy, target: values.target });
run('release', 'verify', '--root', pinned, '--policy', policy, '--package', packagePath);
const scopeId = record.manifest.contributes.scopeId;
if (!scopeId?.startsWith('game:')) throw new Error('Actual game scope is absent');
const pin = { releaseId: 'local-game-fixture', sequence: 1, channel: 'fixture' };
const providerBindings = record.manifest.provides.map(contract => ({ scopeId, contractId: contract.contractId, contractVersion: contract.version, providerModuleId: record.manifest.id, providerDigest: record.digest }));
const planValue = { scopeId, ...pin, target: values.target, minHostApi: record.manifest.hostApi, roots: [record.digest], providerBindings, singletonRequirements: [], resourceDependencies: {}, cohort: { salt: 'explicit-local-fixture', basisPoints: 10000 } };
const plan = json('game-plan.json', planValue);
const bundle = join(root, 'bundle.json');
run('release', 'compose', '--root', pinned, '--policy', policy, '--plan', plan, '--package', packagePath, '--key', key, '--metadata-version', '1', '--expires', String(expires), '--output', bundle);
run('release', 'verify', '--root', pinned, '--policy', policy, '--bundle', bundle, '--state', join(root, 'verify-state.json'));
run('release', 'publish', '--repository', join(root, 'repository'), '--root', pinned, '--policy', policy, '--package', packagePath, '--bundle', bundle);
const platformPlan = json('forbidden-platform-plan.json', { ...planValue, scopeId: 'platform' });
reject('game', 'release', 'compose', '--root', pinned, '--policy', policy, '--plan', platformPlan, '--package', packagePath, '--key', key, '--metadata-version', '2', '--expires', String(expires), '--output', join(root, 'forbidden-platform.json'));
const uiManifest = json('forbidden-ui-manifest.json', { ...record.manifest, components: [...record.manifest.components, { kind: 'ui', entry: 'ui/index.js', runtime: 'webview', stateModel: 'stateless' }] });
reject('game', 'release', 'pack', '--source', join(packagePath, 'payload'), '--manifest', uiManifest, '--policy', policy, '--key', key, '--key-id', identity.keyId, '--output', join(root, 'forbidden-ui'));
const corrupted = join(root, 'corrupted-game'); cpSync(packagePath, corrupted, { recursive: true });
const resource = record.manifest.files.find(file => !file.executable) ?? record.manifest.files[0];
const corruptedFile = join(corrupted, 'payload', resource.path);
const changedBytes = readFileSync(corruptedFile); changedBytes[0] ^= 1;
writeFileSync(corruptedFile, changedBytes);
reject('SHA-256', 'release', 'verify', '--root', pinned, '--policy', policy, '--package', corrupted);
const binaries = Object.fromEntries([['mintcat', values.cli], ['mintcatd', values.daemon], ['mintcat-module', binary]].map(([name, file]) => [name, { path: resolve(file), sha256: digest(resolve(file)) }]));
const builtinInput = { schemaVersion: 2, target: values.target, variant: 'cli', binaries, sources: {}, output: join(root, 'builtin-stage') };
run('stage', '--manifest', json('builtin-release-input.json', builtinInput));
const gameInput = { ...builtinInput, root: pinned, policy, bundle, packages: [packagePath], scopes: { [scopeId]: pin }, modules: [record.manifest.id], output: join(root, 'game-stage') };
run('stage', '--manifest', json('game-release-input.json', gameInput));
for (const name of ['builtin-stage', 'game-stage']) run('verify-stage', '--directory', join(root, name), '--target', values.target, '--variant', 'cli', '--manifest-sha256', digest(join(root, name, 'release-manifest.json')));
reject('target', 'stage', '--manifest', json('bad-target.json', { ...builtinInput, target: 'x86_64', output: join(root, 'bad-target') }));
reject('scope', 'stage', '--manifest', json('bad-game-pin.json', { ...gameInput, scopes: { [scopeId]: { ...pin, releaseId: 'untrusted' } }, output: join(root, 'bad-game-pin') }));
reject('binary', 'stage', '--manifest', json('bad-binary.json', { ...builtinInput, binaries: { ...binaries, mintcat: { ...binaries.mintcat, sha256: `sha256:${'0'.repeat(64)}` } }, output: join(root, 'bad-binary') }));
writeFileSync(join(root, 'builtin-stage/runtime.json'), '{}');
reject('changed', 'verify-stage', '--directory', join(root, 'builtin-stage'), '--target', values.target, '--variant', 'cli', '--manifest-sha256', digest(join(root, 'builtin-stage/release-manifest.json')));
console.log(`Actual game-only check/pack/sign/compose/verify/publish plus builtin/game stage integrity proof completed at ${root}. Fixture keys are isolated and never production defaults.`);
