#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { outputLocation, packageRelease } from './package-release.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const defaultBaseline = join(root, 'target/release/bundle/macos/MintCat.app');
const usage = `Build a complete local macOS MintCat.app with an existing signed module baseline.

Usage: pnpm package:app [--baseline <app-or-runtime-directory>] [--output <fresh-directory>]

  --baseline  Complete .app, immutable stage, or bundled Resources directory.
              Default: target/release/bundle/macos/MintCat.app
  --output    Fresh build directory; existing outputs are never replaced.
              Default: target/app-builds/<timestamp>-<pid>
  --help      Show this help.

Builds the frontend, desktop, CLI, daemon and module verifier, authenticates a new
immutable stage, then assembles only the .app with a local ad-hoc signature.
The signed modules, trust root and source configuration are reused unchanged.
No signing seed is required. No database reset, module release change or auto-launch.
For changed business modules, supply their newly signed baseline explicitly.
`;

function capture(binary, args) {
  return execFileSync(binary, args, { cwd: root, encoding: 'utf8' }).trim();
}
function run(binary, args) {
  execFileSync(binary, args, { cwd: root, stdio: 'inherit' });
}
function readJson(path) { return JSON.parse(readFileSync(path, 'utf8')); }
function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
}
function digest(path) {
  return `sha256:${createHash('sha256').update(readFileSync(path)).digest('hex')}`;
}

function buildApp() {
  const { values } = parseArgs({ options: {
    baseline: { type: 'string', default: defaultBaseline },
    output: { type: 'string' },
    help: { type: 'boolean', default: false },
  } });
  if (values.help) { console.log(usage); return; }
  if (process.platform !== 'darwin') throw new Error('package:app requires macOS. Use the native release assembler for other operating systems.');

  const supplied = resolve(values.baseline);
  const resources = extname(supplied).toLowerCase() === '.app' ? join(supplied, 'Contents/Resources') : supplied;
  if (!existsSync(join(resources, 'runtime.json'))) {
    throw new Error(`Signed baseline runtime.json is missing at ${resources}. Use --baseline with a complete .app, stage or Resources directory; a desktop-only bundle is not a baseline.`);
  }
  const baseline = realpathSync(resources);
  const runtime = readJson(join(baseline, 'runtime.json'));
  if (runtime.schemaVersion !== 1) throw new Error('Unsupported signed baseline runtime schema');
  const policyPath = resolve(baseline, runtime.verificationPolicy);
  const trustRoot = resolve(baseline, runtime.trustRoot);
  readJson(trustRoot);
  const policy = readJson(policyPath);
  const { targetTriple: target } = JSON.parse(capture(process.execPath, [join(root, 'scripts/runtime-target.mjs'), '--json']));
  if (policy.target !== target) throw new Error(`Baseline target ${policy.target} differs from the native host ${target}`);
  const repository = resolve(baseline, runtime.repositoryPath);
  const bundlePath = join(repository, 'current.json');
  const bundle = readJson(bundlePath);
  const packages = new Set();
  const modules = new Set();
  for (const scope of Object.values(bundle.targets)) {
    for (const [pin, record] of Object.entries(scope.signed.packages)) {
      if (!/^sha256:[a-f0-9]{64}$/.test(pin)) throw new Error('Baseline contains an invalid package digest');
      const packagePath = join(repository, 'artifacts/sha256', pin.slice(7));
      readJson(join(packagePath, 'module.json'));
      readJson(join(packagePath, 'signature.json'));
      packages.add(packagePath);
      modules.add(record.manifest.id);
    }
  }
  if (!packages.size || !runtime.scopes?.platform) throw new Error('A complete signed platform baseline is required');

  const stamp = `${new Date().toISOString().replaceAll(':', '-')}-${process.pid}`;
  const output = outputLocation(resolve(values.output ?? join(root, 'target/app-builds', stamp)));
  if (existsSync(output)) throw new Error(`Output already exists: ${output}. Choose a fresh directory.`);
  const appRoot = dirname(dirname(baseline));
  const protectedBaseline = extname(appRoot).toLowerCase() === '.app' ? appRoot : baseline;
  if (relative(protectedBaseline, output).split(/[\\/]/)[0] !== '..') throw new Error('Build output must be outside the signed baseline');

  console.log(`Signed baseline: ${baseline}`);
  console.log(`Target: ${target}; output: ${output}`);
  run('pnpm', ['build']);
  run('cargo', ['build', '--locked', '--release', '--target', target,
    '-p', 'mintcat-desktop-vnext', '-p', 'mintcat-cli', '-p', 'mintcat-daemon', '-p', 'mintcat-module',
    '--features', 'mintcat-desktop-vnext/custom-protocol']);
  const metadata = JSON.parse(capture('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1']));
  const binariesDirectory = join(metadata.target_directory, target, 'release');
  const binaries = Object.fromEntries(['mintcat', 'mintcatd', 'mintcat-module', 'mintcat-desktop-vnext'].map(name => {
    const path = join(binariesDirectory, name);
    return [name, { path, sha256: digest(path) }];
  }));
  mkdirSync(dirname(output), { recursive: true });
  mkdirSync(output);
  const releaseInput = join(output, 'release-input.json');
  writeJson(releaseInput, {
    schemaVersion: 1, target, variant: 'standard', root: trustRoot, policy: policyPath,
    bundle: bundlePath, packages: [...packages].sort(), scopes: runtime.scopes,
    modules: [...modules].sort(), binaries, output: join(output, 'stage'), sources: runtime.sources,
  });
  const verifier = binaries['mintcat-module'].path;
  run(verifier, ['stage', '--manifest', releaseInput]);
  const manifestPath = join(output, 'assembly.json');
  writeJson(manifestPath, {
    schemaVersion: 1, target, variant: 'standard', mode: 'development', stage: 'stage',
    stageManifestSha256: digest(join(output, 'stage/release-manifest.json')), output: 'assembled',
  });
  process.env.MINTCAT_MODULE_BIN = verifier;
  packageRelease('standard', ['--manifest', manifestPath], ['app']);
  const application = join(output, 'assembled/bundles/macos/MintCat.app');
  run('codesign', ['--verify', '--deep', '--strict', application]);
  run(join(application, 'Contents/MacOS/mintcat-desktop-vnext'), ['--verify-embedded-ui']);
  console.log(`\nComplete local application: ${application}`);
  console.log('Ad-hoc signed, not notarized. Native GUI launch is a separate acceptance check.');
}

try { buildApp(); } catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
