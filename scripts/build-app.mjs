#!/usr/bin/env node
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, realpathSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { outputLocation, packageRelease } from './package-release.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const usage = `Build a complete local macOS MintCat.app with compiled builtin services.

Usage: pnpm package:app [--game-config <runtime.json>] [--output <fresh-directory>]

  --game-config Explicit schemaVersion=2 runtime configuration. Reuses only its
                verified game packages, trust root, policy and source configuration.
                Omit for a fresh app with no games; no signed baseline is required.
  --output      Fresh build directory; existing outputs are never replaced.
                Default: target/app-builds/<timestamp>-<pid>
  --help        Show this help without building or accessing signing material.

Builds the static desktop UI, desktop, CLI, builtin daemon and package verifier
using the shared workspace Cargo cache. Games are never rebuilt by app assembly.
Authenticates an immutable stage and assembles an ad-hoc signed local .app.
No publisher signing key, data reset, version change, publishing or auto-launch.
Production packaging is separate and retains native signing/notarization checks.
`;
function capture(binary, args) {
  return execFileSync(binary, args, { cwd: root, encoding: 'utf8' }).trim();
}
function run(binary, args) { execFileSync(binary, args, { cwd: root, stdio: 'inherit' }); }
function readJson(path) { return JSON.parse(readFileSync(path, 'utf8')); }
function writeJson(path, value) { writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' }); }
function digest(path) {
  const file = openSync(path, 'r');
  try {
    const hash = createHash('sha256');
    const buffer = Buffer.allocUnsafe(8192);
    let count;
    while ((count = readSync(file, buffer, 0, buffer.length, null)) !== 0) hash.update(buffer.subarray(0, count));
    return `sha256:${hash.digest('hex')}`;
  } finally {
    closeSync(file);
  }
}

export function gameInputs(configPath, target) {
  if (!configPath) return { sources: {}, scopes: {}, modules: [], packages: [] };
  const config = realpathSync(resolve(configPath));
  const base = dirname(config);
  const runtime = readJson(config);
  if (runtime.schemaVersion !== 2) throw new Error('game-config requires runtime schemaVersion=2; migrate historical platform configurations before app assembly');
  const scopes = runtime.scopes ?? {};
  if (!scopes || typeof scopes !== 'object' || Array.isArray(scopes) || Object.keys(scopes).some(scope => !scope.startsWith('game:'))) throw new Error('game-config scopes must contain only explicit game scopes');
  const inputs = { sources: runtime.sources ?? {}, scopes, modules: [], packages: [], ...(runtime.cloud ? { cloud: runtime.cloud } : {}) };
  if (!Object.keys(scopes).length) return inputs;
  for (const field of ['trustRoot', 'verificationPolicy', 'repositoryPath']) if (typeof runtime[field] !== 'string' || !runtime[field].trim()) throw new Error(`Configured games require ${field}`);
  const trustRoot = resolve(base, runtime.trustRoot);
  const policyPath = resolve(base, runtime.verificationPolicy);
  readJson(trustRoot);
  const policy = readJson(policyPath);
  if (policy.target !== target) throw new Error(`Configured game target ${policy.target} differs from app target ${target}`);
  const repository = resolve(base, runtime.repositoryPath);
  const bundlePath = join(repository, 'current.json');
  const bundle = readJson(bundlePath);
  const packages = new Set();
  const modules = new Set();
  for (const record of Object.values(bundle.targets)) {
    if (!record.signed.release.scopeId.startsWith('game:')) throw new Error('App game repository must not contain a platform release');
    for (const [pin, pkg] of Object.entries(record.signed.packages)) {
      if (!/^sha256:[a-f0-9]{64}$/.test(pin)) throw new Error('Configured game contains an invalid package digest');
      const packagePath = join(repository, 'artifacts/sha256', pin.slice(7));
      readJson(join(packagePath, 'module.json'));
      readJson(join(packagePath, 'signature.json'));
      packages.add(packagePath); modules.add(pkg.manifest.id);
    }
  }
  return { ...inputs, root: trustRoot, policy: policyPath, bundle: bundlePath, packages: [...packages].sort(), modules: [...modules].sort() };
}

function buildApp() {
  const { values } = parseArgs({ options: { 'game-config': { type: 'string' }, output: { type: 'string' }, help: { type: 'boolean', default: false } } });
  if (values.help) { console.log(usage); return; }
  if (process.platform !== 'darwin') throw new Error('package:app requires macOS. Use the native release assembler for other operating systems.');
  const { targetTriple: target } = JSON.parse(capture(process.execPath, [join(root, 'scripts/runtime-target.mjs'), '--json']));
  const games = gameInputs(values['game-config'], target);
  const stamp = `${new Date().toISOString().replaceAll(':', '-')}-${process.pid}`;
  const output = outputLocation(resolve(values.output ?? join(root, 'target/app-builds', stamp)));
  if (existsSync(output)) throw new Error(`Output already exists: ${output}. Choose a fresh directory.`);
  if (values['game-config']) {
    const configRoot = dirname(realpathSync(resolve(values['game-config'])));
    if (relative(configRoot, output).split(/[\\/]/)[0] !== '..') throw new Error('Build output must be outside the configured game repository resources');
  }
  console.log(`Target: ${target}; configured games: ${Object.keys(games.scopes).length}; output: ${output}`);
  run('pnpm', ['build:ui']);
  run('cargo', ['build', '--locked', '--release', '--target', target,
    '-p', 'mintcat-desktop-vnext', '-p', 'mintcat-cli', '-p', 'mintcat-daemon', '-p', 'mintcat-module',
    '--features', 'mintcat-desktop-vnext/custom-protocol']);
  const metadata = JSON.parse(capture('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1']));
  const binariesDirectory = join(metadata.target_directory, target, 'release');
  const binaries = Object.fromEntries(['mintcat', 'mintcatd', 'mintcat-module', 'mintcat-desktop-vnext'].map(name => {
    const path = join(binariesDirectory, name);
    return [name, { path, sha256: digest(path) }];
  }));
  mkdirSync(dirname(output), { recursive: true }); mkdirSync(output);
  const releaseInput = join(output, 'release-input.json');
  writeJson(releaseInput, { schemaVersion: 2, target, variant: 'standard', ...games, binaries, output: join(output, 'stage') });
  const verifier = binaries['mintcat-module'].path;
  run(verifier, ['stage', '--manifest', releaseInput]);
  const manifestPath = join(output, 'assembly.json');
  writeJson(manifestPath, { schemaVersion: 1, target, variant: 'standard', mode: 'development', stage: 'stage', stageManifestSha256: digest(join(output, 'stage/release-manifest.json')), output: 'assembled' });
  process.env.MINTCAT_MODULE_BIN = verifier;
  packageRelease('standard', ['--manifest', manifestPath], ['app']);
  const application = join(output, 'assembled/bundles/macos/MintCat.app');
  run('codesign', ['--verify', '--deep', '--strict', application]);
  run(join(application, 'Contents/MacOS/mintcat-desktop-vnext'), ['--verify-embedded-ui']);
  console.log(`\nComplete local application: ${application}`);
  console.log('Ad-hoc signed, not notarized. Native GUI launch is a separate acceptance check.');
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try { buildApp(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
