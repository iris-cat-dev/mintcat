#!/usr/bin/env node
// Actual local workflow proof. Requires an already built mintcat-module and supplied 0600 base64 seed.
import { execFileSync, spawn } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync, readdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { createHash } from 'node:crypto';
const options = Object.fromEntries(process.argv.slice(2).reduce((rows, value, index, argv) => index % 2 === 0 ? [...rows, [value, argv[index + 1]]] : rows, []));
for (const name of ['--bin', '--key', '--target', '--root', '--cli', '--daemon']) if (!options[name]) throw new Error(`Required ${name}`);
const binary = resolve(options['--bin']); const key = resolve(options['--key']); const root = resolve(options['--root']);
mkdirSync(root); const source = join(root, 'example'); const devRoot = join(root, 'dev');
const json = (name, value) => { const file = join(root, name); writeFileSync(file, JSON.stringify(value, null, 2)); return file; };
const run = (...args) => { const output = execFileSync(binary, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }); process.stdout.write(output); return output; };
const reject = (expected, ...args) => { try { execFileSync(binary, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }); } catch (error) { if (error.status !== 0 && String(error.stderr).includes(expected)) return; throw error; } throw new Error(`Untrusted input admitted: ${args.join(' ')}`); };
run('new', source, '--id', 'local.example', '--target', options['--target']);
run('dev', source, '--dev-root', devRoot, '--key', key, '--once');
run('check', source);
// A real UI host and generation swap, not a mock supervisor or manually edited active pointer.
const dev = spawn(binary, ['dev', source, '--dev-root', devRoot, '--key', key], { detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'inherit'] });
let devClosed;
const devExit = new Promise(resolveExit => {
  const finish = outcome => { devClosed = outcome; resolveExit(outcome); };
  dev.once('error', error => finish({ error }));
  dev.once('close', (code, signal) => finish({ code, signal }));
});
let output = ''; dev.stdout.on('data', chunk => { output += chunk; process.stdout.write(chunk); });
let origin;
async function waitFor(predicate) { for (let i = 0; i < 240; i++) { if (predicate()) return; if (devClosed || dev.exitCode !== null || dev.signalCode !== null) throw new Error(`dev exited ${dev.exitCode ?? dev.signalCode}: ${devClosed?.error ?? ''}`); await sleep(250); } throw new Error('dev host deadline exceeded'); }
try {
  await waitFor(() => output.includes('"activeRequests"'));
  origin = output.match(/Isolated signed worker\/UI host: (http:\/\/127\.0\.0\.1:\d+)/)?.[1];
  if (!origin) throw new Error('real UI host URL absent');
  const descriptor = await fetch(`${origin}/payload/ui/contributions.json`).then(response => response.json());
  if (descriptor.moduleId !== 'local.example' || descriptor.pages[0].route !== '/local.example/example') throw new Error('signed UI descriptor mismatch');
  const previousManifest = await fetch(`${origin}/manifest`).then(response => response.json());
  const response = await fetch(`${origin}/invoke`, { method: 'POST', headers: { 'Content-Type': 'application/json', Origin: origin }, body: JSON.stringify({ method: 'example', params: { consumer: 'local-smoke' } }) });
  const result = await response.json();
  if (!response.ok || result.input.consumer !== 'local-smoke' || result.example !== true) throw new Error('real framed worker invoke mismatch');
  const forbidden = await fetch(`${origin}/invoke`, { method: 'POST', headers: { Origin: 'https://untrusted.invalid' }, body: '{}' });
  if (forbidden.status !== 403) throw new Error('untrusted UI origin admitted');
  const traversal = await fetch(`${origin}/payload/ui/%2f../module.json`);
  if (traversal.status !== 403) throw new Error('untrusted encoded payload path admitted');
  const previous = output;
  const ui = join(source, 'payload/ui/index.js'); writeFileSync(ui, readFileSync(ui, 'utf8').replace('Runnable local module example,', 'Reloaded runnable local module example,'));
  await waitFor(() => output.slice(previous.length).includes('"activeRequests"'));
  const changed = Buffer.from(await fetch(`${origin}/payload/ui/index.js`).then(response => response.arrayBuffer()));
  const nextManifest = await fetch(`${origin}/manifest`).then(response => response.json());
  const nextFile = nextManifest.files.find(file => file.path === 'ui/index.js');
  const previousFile = previousManifest.files.find(file => file.path === 'ui/index.js');
  if (nextFile.sha256 === previousFile.sha256 || nextFile.sha256 !== `sha256:${createHash('sha256').update(changed).digest('hex')}`) throw new Error('generation swap did not serve new authenticated UI bytes');
} finally {
  let timer;
  const deadline = new Promise((_, rejectDrain) => {
    timer = setTimeout(() => rejectDrain(new Error('dev framed drain deadline exceeded')), 60_000);
  });
  try {
    if (!devClosed && dev.exitCode === null && dev.signalCode === null) dev.kill('SIGINT');
    const outcome = await Promise.race([devExit, deadline]);
    if (outcome.error) throw outcome.error;
    if (outcome.code !== 0) throw new Error(`dev drain exit ${outcome.code ?? outcome.signal}`);
    if (!output.includes('framed drain complete')) throw new Error('dev exited without completing framed drain');
    if (origin) {
      let listenerOpen = false;
      try { await fetch(`${origin}/generation`, { signal: AbortSignal.timeout(2000) }); listenerOpen = true; }
      catch (error) { if (error.cause?.code !== 'ECONNREFUSED') throw error; }
      if (listenerOpen) throw new Error('dev listener survived framed drain');
    }
  } catch (error) {
    if (!devClosed && dev.pid) {
      // A failed proof must clean up its own process group, never silently pass after a forced exit.
      if (process.platform === 'win32') dev.kill('SIGKILL');
      else { try { process.kill(-dev.pid, 'SIGKILL'); } catch (killError) { if (killError.code !== 'ESRCH') throw killError; } }
      await devExit;
    }
    throw error;
  } finally { clearTimeout(timer); }
}
const packagePath = join(root, 'package'); const record = JSON.parse(run('pack', source, '--key', key, '--output', packagePath));
const identity = JSON.parse(run('release', 'key-info', '--key', key));
const expires = Math.floor(Date.now() / 1000) + 3600;
const role = { keyIds: [identity.keyId], threshold: 1 };
const rootSpec = json('root-spec.json', { header: { kind: 'root', schemaVersion: 1, version: 1, expires }, keys: { [identity.keyId]: identity.publicKey }, roles: { root: role, targets: role, snapshot: role, timestamp: role }, publishers: { 'local-example': { [identity.keyId]: identity.publicKey } } });
const pinned = join(root, 'root.json'); run('release', 'sign-root', '--spec', rootSpec, '--key', key, '--output', pinned);
const project = JSON.parse(readFileSync(join(source, 'module-project.json'), 'utf8'));
const policy = json('policy.json', project.policy);
for (const generation of readdirSync(join(devRoot, 'packages'))) reject('origin', 'release', 'verify', '--root', pinned, '--policy', policy, '--package', join(devRoot, 'packages', generation));
reject('package', 'release', 'verify', '--root', pinned, '--policy', policy, '--package', join(source, 'payload'));
const providerBindings = record.manifest.provides.map(contract => ({ scopeId: 'platform', contractId: contract.contractId, contractVersion: contract.version, providerModuleId: record.manifest.id, providerDigest: record.digest }));
const plan = json('platform-plan.json', { scopeId: 'platform', releaseId: 'local-example-baseline', sequence: 1, channel: 'fixture', target: options['--target'], minHostApi: project.manifest.hostApi, roots: [record.digest], providerBindings, singletonRequirements: [], resourceDependencies: {}, cohort: { salt: 'explicit-local-fixture', basisPoints: 10000 } });
const bundle = join(root, 'bundle.json');
run('release', 'compose', '--root', pinned, '--policy', policy, '--plan', plan, '--package', packagePath, '--key', key, '--metadata-version', '1', '--expires', String(expires), '--output', bundle);
run('release', 'verify', '--root', pinned, '--policy', policy, '--bundle', bundle, '--state', join(root, 'verify-state.json'));
run('release', 'publish', '--repository', join(root, 'repository'), '--root', pinned, '--policy', policy, '--package', packagePath, '--bundle', bundle);
// Stage exercises actual product siblings; no generated stand-ins are accepted.
{
  const binaries = Object.fromEntries([['mintcat', options['--cli']], ['mintcatd', options['--daemon']], ['mintcat-module', binary]].map(([name, file]) => [name, { path: resolve(file), sha256: `sha256:${createHash('sha256').update(readFileSync(file)).digest('hex')}` }]));
  const manifest = json('release-input.json', { schemaVersion: 1, target: options['--target'], variant: 'cli', root: pinned, policy, bundle, packages: [packagePath], scopes: { platform: { releaseId: 'local-example-baseline', sequence: 1, channel: 'fixture' } }, modules: ['local.example'], binaries, output: join(root, 'staged-cli') });
  run('stage', '--manifest', manifest);
  const input = JSON.parse(readFileSync(manifest, 'utf8'));
  reject('target', 'stage', '--manifest', json('bad-target.json', { ...input, target: 'x86_64', output: join(root, 'bad-target') }));
  reject('scope', 'stage', '--manifest', json('bad-baseline.json', { ...input, scopes: { platform: { ...input.scopes.platform, releaseId: 'untrusted-baseline' } }, output: join(root, 'bad-baseline') }));
  reject('binary', 'stage', '--manifest', json('bad-binary.json', { ...input, binaries: { ...input.binaries, mintcat: { ...input.binaries.mintcat, sha256: `sha256:${'0'.repeat(64)}` } }, output: join(root, 'bad-binary') }));
}
console.log(`Actual local new/dev/check/signed-pack/compose/verify/publish proof completed at ${root}. Fixture root is explicitly supplied; never use it as a production default. Browser visual rendering and Windows installation/start remain target-surface proofs.`);
