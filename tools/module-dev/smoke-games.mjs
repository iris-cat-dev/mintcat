#!/usr/bin/env node
// TEST-ONLY: creates signed marker archives outside the repository, never publisher assets.
// Usage (host target only; supply newly built product binaries):
// node tools/module-dev/smoke-games.mjs --bin target/release/mintcat-module \
//   --cli target/release/mintcat --daemon target/release/mintcatd \
//   --target aarch64-apple-darwin --root /tmp/mintcat-TEST-ONLY-game-smoke
// Outputs: <root>/<game>/package, game-stage, worker-proof.json, worker-runtime-test/module-logs.
// check/pack rebuild the REAL game worker source and reuse the workspace Cargo cache.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: {
  ...Object.fromEntries(['bin', 'cli', 'daemon', 'target', 'root'].map(name => [name, { type: 'string' }])),
  help: { type: 'boolean' },
} });
if (values.help) {
  console.log('TEST-ONLY actual DRG + Rogue Core developer/worker proof\n'
    + 'node tools/module-dev/smoke-games.mjs --bin <mintcat-module> --cli <mintcat> --daemon <mintcatd> --target <host-triple> --root <new-directory-outside-repository>\n'
    + 'Uses offline Cargo caches; check/pack build real workers, not mocks. No real game installation or patch assets are supplied.\n'
    + 'Evidence: <root>/{drg,rogue-core}/worker-proof.json, package/, game-stage/, worker-runtime-test/module-logs/');
  process.exit(0);
}
for (const name of ['bin', 'cli', 'daemon', 'target', 'root']) {
  if (!values[name]) throw new Error(`Required --${name}; use --help`);
}
const here = dirname(fileURLToPath(import.meta.url));
const repository = resolve(here, '../..');
const root = resolve(values.root);
const inside = relative(repository, root);
if (!inside || (!inside.startsWith(`..${sep}`) && inside !== '..' && !inside.startsWith(sep))) {
  throw new Error('TEST-ONLY output must be outside the source repository');
}
const cargo = (...args) => execFileSync('cargo', args, { cwd: repository, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
const metadata = JSON.parse(cargo('metadata', '--locked', '--no-deps', '--format-version', '1'));
const host = execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host: (.+)$/m)?.[1];
if (values.target !== host) throw new Error(`Actual worker execution requires the host target ${host}`);
mkdirSync(root, { mode: 0o700 }); // Never reuse an existing directory, key, or user database.
const marker = 'TEST-ONLY MintCat game worker smoke; marker ZIPs are NOT installable game assets.\n';
writeFileSync(join(root, 'TEST-ONLY'), marker, { flag: 'wx' });
const workspace = join(root, 'projects');
mkdirSync(workspace);
const workspaceText = readFileSync(join(repository, 'Cargo.toml'), 'utf8');
function section(text, name) {
  const escaped = name.replaceAll('.', '\\.');
  const match = text.match(new RegExp(`^\\[${escaped}\\]\\r?\\n([\\s\\S]*?)(?=^\\[|$(?![\\s\\S]))`, 'm'));
  if (!match) throw new Error(`Missing Cargo section [${name}]`);
  return match[1];
}
// Preserve the repository's versions/features, making only path dependencies absolute.
const dependencies = section(workspaceText, 'workspace.dependencies').replace(/\bpath\s*=\s*"([^"]+)"/g,
  (_, path) => `path = ${JSON.stringify(resolve(repository, path))}`);
const games = ['drg', 'rogue-core'].map(game => {
  const source = join(repository, 'modules', `game-${game}`);
  const manifest = join(source, 'Cargo.toml');
  const pkg = metadata.packages.find(pkg => resolve(pkg.manifest_path) === manifest);
  const project = JSON.parse(readFileSync(join(source, 'module-project.json'), 'utf8'));
  const worker = pkg?.targets.find(target => target.kind.includes('bin') && target.name === project.workerBinary);
  if (!worker) throw new Error(`Cargo metadata does not contain the declared real ${game} worker`);
  const fixture = join(workspace, game);
  mkdirSync(fixture);
  writeFileSync(join(fixture, 'TEST-ONLY'), marker, { flag: 'wx' });
  writeFileSync(join(fixture, 'module-project.json'), JSON.stringify(project, null, 2), { flag: 'wx' });
  const gameDependencies = section(readFileSync(manifest, 'utf8'), 'dependencies');
  writeFileSync(join(fixture, 'Cargo.toml'), `[package]\nname = "test-only-${game}-worker-project"\nversion.workspace = true\nedition.workspace = true\nlicense.workspace = true\nrust-version.workspace = true\npublish = false\nautobins = false\n\n[[bin]]\nname = ${JSON.stringify(worker.name)}\npath = ${JSON.stringify(worker.src_path)}\n\n[dependencies]\n${gameDependencies}\n${pkg.name}.workspace = true\n`, { flag: 'wx' });
  // A valid stored ZIP with exactly one unmistakable marker. No PAK, DLL, or UE asset.
  const archive = project.manifest.contributes.bundledResources.find(resource => resource.archivePath);
  if (!archive) throw new Error(`The real ${game} manifest lacks its required archive binding`);
  const archivePath = join(fixture, 'payload', archive.archivePath);
  mkdirSync(dirname(archivePath), { recursive: true });
  writeFileSync(archivePath, markerZip(marker), { flag: 'wx' });
  return { game, source, fixture };
});
const probeName = 'test-only-game-worker-smoke-host';
const hostProject = join(workspace, 'host');
mkdirSync(hostProject);
writeFileSync(join(hostProject, 'Cargo.toml'), `[package]\nname = "${probeName}"\nversion.workspace = true\nedition.workspace = true\nlicense.workspace = true\nrust-version.workspace = true\npublish = false\n\n[[bin]]\nname = "${probeName}"\npath = ${JSON.stringify(join(here, 'host/game-worker-smoke.rs'))}\n\n[dependencies]\nanyhow.workspace = true\nmintcat-game-api.workspace = true\nmintcat-integrator-core.workspace = true\nmintcat-runtime-artifacts.workspace = true\nmintcat-runtime-kernel.workspace = true\nmintcat-runtime-release.workspace = true\nmintcat-runtime-supervisor.workspace = true\nmintcat-sdk.workspace = true\nmintcat-unreal-pak.workspace = true\nserde_json.workspace = true\ntokio.workspace = true\n`, { flag: 'wx' });
writeFileSync(join(workspace, 'Cargo.toml'), `[workspace]\nresolver = "2"\nmembers = ["drg", "rogue-core", "host"]\n\n[workspace.package]\n${section(workspaceText, 'workspace.package')}\n[workspace.dependencies]\n${dependencies}`, { flag: 'wx' });
copyFileSync(join(repository, 'Cargo.lock'), join(workspace, 'Cargo.lock'));
process.env.CARGO_TARGET_DIR = metadata.target_directory;
process.env.CARGO_NET_OFFLINE = 'true';
// Resolve the fixture dependency graph against the copied lock, preserving its pins.
// --no-deps skips lock resolution; discard the full graph instead of buffering it.
execFileSync('cargo', ['metadata', '--offline', '--filter-platform', values.target, '--format-version', '1',
  '--manifest-path', join(workspace, 'Cargo.toml')], { cwd: workspace, stdio: ['ignore', 'ignore', 'inherit'] });
for (const { game, source, fixture } of games) {
  execFileSync(process.execPath, [join(here, 'smoke.mjs'), '--bin', resolve(values.bin), '--project', fixture,
    '--target', values.target, '--root', join(root, game), '--cli', resolve(values.cli), '--daemon', resolve(values.daemon)],
  { cwd: repository, stdio: 'inherit' });
  const absentArchive = JSON.parse(readFileSync(join(source, 'module-project.json'), 'utf8'))
    .manifest.contributes.bundledResources.find(resource => resource.archivePath).archivePath;
  const rejections = {};
  for (const command of ['check', 'pack']) {
    const args = [command, source, '--target', values.target];
    if (command === 'pack') args.push('--key', join(root, game, 'TEST-ONLY-ed25519.seed'), '--output', join(root, game, 'forbidden-official-package'));
    try {
      execFileSync(resolve(values.bin), args, { cwd: repository, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
    } catch (error) {
      const diagnostic = String(error.stderr);
      if (!(error.status > 0) || !diagnostic.includes(`bundled resource archive ${absentArchive} missing from signed game payload`)) throw error;
      rejections[command] = { status: 'PASS rejected unavailable official resource input', diagnostic };
      continue;
    }
    throw new Error(`Official ${game} ${command} unexpectedly accepted unavailable real resource input`);
  }
  writeFileSync(join(root, game, 'official-missing-resources.json'), JSON.stringify(rejections, null, 2), { flag: 'wx' });
  console.log(`PASS ${game}: official check/pack reject unavailable real archive payload`);
  console.log(`PASS ${game}: developer check/pack/sign/verify/stage integrity and rejection proofs`);
}
cargo('build', '--locked', '--release', '--target', values.target, '--bin', probeName, '--manifest-path', join(workspace, 'Cargo.toml'));
const probe = join(metadata.target_directory, values.target, 'release', `${probeName}${values.target.includes('windows') ? '.exe' : ''}`);
for (const { game } of games) execFileSync(probe, [join(root, game)], { cwd: repository, stdio: 'inherit' });
console.log(`PASS two actual signed game workers; evidence retained at ${root}`);
console.log('UNAVAILABLE real-game install inputs: licensed base game content, authentic DRG/Rogue Core patch archives, and optional UE4SS assets. TEST-ONLY markers are rejected; no deployment or Windows gameplay evidence is claimed.');

function markerZip(text) {
  const name = Buffer.from('TEST-ONLY-NOT-INSTALLABLE.txt');
  const data = Buffer.from(text);
  let crc = 0xffffffff;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  crc = (crc ^ 0xffffffff) >>> 0;
  const local = Buffer.alloc(30); local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4);
  local.writeUInt16LE(33, 12); local.writeUInt32LE(crc, 14);
  local.writeUInt32LE(data.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(name.length, 26);
  const central = Buffer.alloc(46); central.writeUInt32LE(0x02014b50); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6);
  central.writeUInt16LE(33, 14); central.writeUInt32LE(crc, 16);
  central.writeUInt32LE(data.length, 20); central.writeUInt32LE(data.length, 24); central.writeUInt16LE(name.length, 28);
  const end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50); end.writeUInt16LE(1, 8); end.writeUInt16LE(1, 10);
  end.writeUInt32LE(central.length + name.length, 12); end.writeUInt32LE(local.length + name.length + data.length, 16);
  return Buffer.concat([local, name, data, central, name, end]);
}
