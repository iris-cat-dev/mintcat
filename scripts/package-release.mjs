#!/usr/bin/env node
import { readFileSync, existsSync, mkdirSync, copyFileSync, writeFileSync, statSync, lstatSync, readdirSync, readlinkSync, realpathSync, chmodSync, cpSync } from 'node:fs';
import { dirname, resolve, join, relative, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const supported = new Map([
  ['aarch64-apple-darwin', { extension: '', os: 'macos' }],
  ['x86_64-apple-darwin', { extension: '', os: 'macos' }],
  ['x86_64-pc-windows-msvc', { extension: '.exe', os: 'windows' }],
  ['x86_64-unknown-linux-gnu', { extension: '', os: 'linux' }],
  ['aarch64-unknown-linux-gnu', { extension: '', os: 'linux' }],
]);
function run(binary, args, cwd = root, env = process.env) { execFileSync(binary, args, { cwd, stdio: 'inherit', env }); }
function digest(path) { return `sha256:${createHash('sha256').update(readFileSync(path)).digest('hex')}`; }
function realFile(path) { const metadata = lstatSync(path); if (!metadata.isFile() || metadata.isSymbolicLink()) throw new Error(`Expected regular file: ${path}`); }
function object(value, name) { if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${name} must be an object`); }
function requireEnvironment(names) {
  const absent = names.filter(name => !process.env[name]?.trim());
  if (absent.length) throw new Error(`Production signing prerequisite missing: ${absent.join(', ')}. Select an explicit development assembly for unsigned OS development smoke; the signed module baseline is always required.`);
}
export function outputLocation(path) {
  let ancestor = path;
  const missing = [];
  while (!existsSync(ancestor)) { missing.unshift(basename(ancestor)); ancestor = dirname(ancestor); }
  return join(realpathSync(ancestor), ...missing);
}
function verifyStage(stage, manifest) {
  const args = ['verify-stage', '--directory', stage, '--target', manifest.target, '--variant', manifest.variant, '--manifest-sha256', manifest.stageManifestSha256];
  const verifier = process.env.MINTCAT_MODULE_BIN;
  if (!verifier?.trim()) throw new Error('MINTCAT_MODULE_BIN must explicitly select an already built native mintcat-module verifier; implicit development Cargo builds are forbidden');
  realFile(resolve(verifier));
  run(resolve(verifier), args);
}
function verifyCodeSignatures(stage, manifest, platform, names) {
  if (manifest.mode !== 'production') return;
  if (platform.os === 'macos') {
    requireEnvironment(['APPLE_TEAM_ID']);
    for (const name of names) {
      const binary = join(stage, name);
      run('codesign', ['--verify', '--strict', binary]);
      const details = spawnSync('codesign', ['--display', '--verbose=4', binary], { encoding: 'utf8' });
      if (details.error || details.status !== 0) throw new Error(`Cannot inspect native code signature: ${name}`);
      if (!details.stderr.split(/\r?\n/).includes(`TeamIdentifier=${process.env.APPLE_TEAM_ID}`)) throw new Error(`Binary is not signed by the declared Apple team: ${name}`);
    }
  } else if (platform.os === 'windows') {
    const quote = value => value.replaceAll("'", "''");
    for (const name of names) run('powershell', ['-NoProfile', '-NonInteractive', '-Command', `$s=Get-AuthenticodeSignature -LiteralPath '${quote(join(stage, `${name}.exe`))}'; if ($s.Status -ne 'Valid') { throw 'Native release binary must have a valid Authenticode signature' }`]);
  }
}
function notarizeArchive(archive) {
  const result = spawnSync('xcrun', ['notarytool', 'submit', archive, '--apple-id', process.env.APPLE_ID, '--password', process.env.APPLE_PASSWORD, '--team-id', process.env.APPLE_TEAM_ID, '--wait', '--output-format', 'json'], { encoding: 'utf8' });
  if (result.error || result.status !== 0) throw new Error(`Native CLI archive notarization failed: ${result.error?.message ?? result.stderr}`);
  const submission = JSON.parse(result.stdout);
  if (submission.status !== 'Accepted' || typeof submission.id !== 'string') throw new Error('Apple did not accept the native CLI archive notarization');
  writeFileSync(`${archive}.notarization.json`, JSON.stringify({ id: submission.id, status: submission.status, sha256: digest(archive) }, null, 2));
}
function artifactPins(base, directory, pins = {}) {
  for (const entry of readdirSync(directory, { withFileTypes: true }).sort((left, right) => left.name.localeCompare(right.name))) {
    const path = join(directory, entry.name);
    const name = relative(base, path).replaceAll('\\', '/');
    if (entry.isDirectory()) artifactPins(base, path, pins);
    else if (entry.isFile()) pins[name] = { sha256: digest(path), mode: lstatSync(path).mode & 0o777 };
    // Native .app/framework symlinks are produced by Tauri, not accepted stage inputs.
    else if (entry.isSymbolicLink()) pins[name] = { symlink: readlinkSync(path) };
    else throw new Error(`Unexpected bundle artifact: ${path}`);
  }
  return pins;
}
function copyReleaseTree(source, destination, verbatimSymlinks = false) {
  cpSync(source, destination, { recursive: true, preserveTimestamps: true, verbatimSymlinks, force: false, errorOnExist: true });
  // Node's copy can apply the host umask to newly created files (Tauri's DMG
  // helper is 0777). Restore pinned permissions, then independently verify them.
  function restoreModes(sourcePath, destinationPath) {
    const metadata = lstatSync(sourcePath);
    if (metadata.isSymbolicLink()) return;
    if (metadata.isDirectory()) {
      for (const name of readdirSync(sourcePath)) restoreModes(join(sourcePath, name), join(destinationPath, name));
    }
    chmodSync(destinationPath, metadata.mode & 0o777);
  }
  restoreModes(source, destination);
}
export function packageRelease(variant, args = process.argv.slice(2), bundleTargets = []) {
  if (args.length !== 2 || args[0] !== '--manifest' || !args[1]) throw new Error('Usage: --manifest <assembly.json>; first create a signed immutable stage with mintcat-module stage --manifest <release-input.json>');
  const manifestPath = resolve(args[1]); realFile(manifestPath);
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8')); object(manifest, 'Assembly plan');
  const fields = new Set(['schemaVersion', 'target', 'variant', 'mode', 'stage', 'stageManifestSha256', 'output']);
  if (Object.keys(manifest).some(key => !fields.has(key))) throw new Error('Unknown assembly plan field; mutable binaries, repositories, frontend ZIPs and backend DLL inputs are not accepted');
  if (manifest.schemaVersion !== 1 || !['production', 'development'].includes(manifest.mode)) throw new Error('Assembly plan requires schemaVersion=1 and explicit production/development mode');
  const platform = supported.get(manifest.target);
  if (!platform || manifest.variant !== variant) throw new Error('Assembly plan must pin the exact supported target and requested flavor');
  if (['portable', 'fixed-webview2'].includes(variant) && platform.os !== 'windows') throw new Error('Portable and fixed WebView2 are Windows MSVC variants only');
  if (!/^sha256:[a-f0-9]{64}$/.test(manifest.stageManifestSha256)) throw new Error('Assembly plan must pin stageManifestSha256');
  for (const key of ['stage', 'output']) if (typeof manifest[key] !== 'string' || !manifest[key].trim()) throw new Error(`${key} must be an explicit nonempty path`);
  const stage = resolve(dirname(manifestPath), manifest.stage);
  const output = outputLocation(resolve(dirname(manifestPath), manifest.output));
  const immutableStage = realpathSync(stage);
  if (output === immutableStage || relative(immutableStage, output).split(/[\\/]/)[0] !== '..') throw new Error('Assembly output must not be inside the immutable stage');
  if (existsSync(output) || existsSync(`${output}.zip`) || existsSync(`${output}.zip.notarization.json`)) throw new Error('Assembly output/archive must be fresh; existing outputs are never replaced');
  if ((platform.os === 'windows' && process.platform !== 'win32') || (platform.os === 'macos' && process.platform !== 'darwin') || (platform.os === 'linux' && process.platform !== 'linux')) throw new Error('Runnable OS packaging/signing requires the actual target OS; cross-host stage verification is not installation proof');
  // Cryptographic and complete scope/target/variant verification happens before bundling or creating output.
  verifyStage(stage, manifest);
  const staged = JSON.parse(readFileSync(join(stage, 'release-manifest.json'), 'utf8'));
  const names = ['mintcat', 'mintcatd', 'mintcat-module', ...(variant === 'cli' ? [] : ['mintcat-desktop-vnext'])];
  verifyCodeSignatures(stage, manifest, platform, names);
  // A native GUI compiled with devUrl/empty assets is not an offline product,
  // even when its module baseline and OS image are otherwise valid.
  if (variant !== 'cli') run(join(stage, `mintcat-desktop-vnext${platform.extension}`), ['--verify-embedded-ui'], stage);
  if (manifest.mode === 'production' && platform.os === 'macos' && variant === 'cli') requireEnvironment(['APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID']);
  if (manifest.mode === 'production' && ['standard', 'fixed-webview2'].includes(variant)) {
    if (platform.os === 'macos') requireEnvironment(['APPLE_SIGNING_IDENTITY', 'APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID']);
    if (platform.os === 'windows') requireEnvironment(['MINTCAT_WINDOWS_SIGN_COMMAND']);
  }
  mkdirSync(output, { recursive: true });
  if (variant === 'portable' || variant === 'cli') {
    copyReleaseTree(stage, output);
    verifyStage(output, manifest);
    const archive = `${output}.zip`;
    if (process.platform === 'win32') {
      const ps = value => value.replaceAll("'", "''");
      run('powershell', ['-NoProfile', '-NonInteractive', '-Command', `Compress-Archive -LiteralPath '${ps(output)}' -DestinationPath '${ps(archive)}'`]);
    } else run('zip', ['-q', '-r', archive, output.split(/[\\/]/).at(-1)], dirname(output));
    if (manifest.mode === 'production' && platform.os === 'macos') notarizeArchive(archive);
    console.log(`Verified offline ${variant} archive: ${archive}`);
    console.log(`Data directory: ${JSON.stringify(staged.dataDirectory)}; --data-dir / MINTCAT_DATA_DIR overrides the portable directory. CLI and GUI must use the same declared directory; legacy databases are never imported implicitly.`);
    return;
  }
  const runtime = join(output, 'runtime'); copyReleaseTree(stage, runtime);
  const native = join(output, 'native'); mkdirSync(native);
  for (const name of ['mintcat', 'mintcatd', 'mintcat-module']) copyFileSync(join(runtime, `${name}${platform.extension}`), join(native, `${name}-${manifest.target}${platform.extension}`));
  // Bundle the already-pinned compiled desktop; never rebuild/re-hash it into a mutated stage.
  const cargoTargetDir = join(output, 'compiler');
  const releaseDir = join(cargoTargetDir, manifest.target, 'release'); mkdirSync(releaseDir, { recursive: true });
  const desktopBinary = join(releaseDir, `mintcat-desktop-vnext${platform.extension}`);
  copyFileSync(join(runtime, `mintcat-desktop-vnext${platform.extension}`), desktopBinary);
  if (digest(desktopBinary) !== staged.binaries['mintcat-desktop-vnext']) throw new Error('Compiled desktop digest differs from the verified stage');
  const resources = Object.fromEntries(['baseline', 'trust', 'scope-locks.json', 'runtime.json', 'release-manifest.json', ...(variant === 'fixed-webview2' ? ['webview2'] : [])].map(name => [join(runtime, name), name]));
  const config = {
    // Keep the name compiled into Tauri's Linux resource resolver unchanged.
    productName: 'MintCat',
    identifier: manifest.mode === 'development' ? 'com.mint.cat.vnext.local' : 'com.mint.cat',
    bundle: { active: true, externalBin: ['mintcat', 'mintcatd', 'mintcat-module'].map(name => join(native, name)), resources },
  };
  if (platform.os === 'macos' && manifest.mode === 'development') {
    // Tauri's implicit local signature can cover only the executable. Pin an
    // explicit ad-hoc bundle identity so Contents/Resources is sealed too.
    config.bundle.macOS = { signingIdentity: '-' };
  }
  if (platform.os === 'windows') {
    config.bundle.windows = {};
    if (manifest.mode === 'production') config.bundle.windows.signCommand = process.env.MINTCAT_WINDOWS_SIGN_COMMAND;
    if (variant === 'fixed-webview2') config.bundle.windows.webviewInstallMode = { type: 'fixedRuntime', path: join(runtime, 'webview2') };
    else config.bundle.windows.webviewInstallMode = { type: 'embedBootstrapper', silent: true };
  }
  const configPath = join(output, 'native-bundle.json'); writeFileSync(configPath, JSON.stringify(config, null, 2));
  const bundleArgs = ['exec', 'tauri', 'bundle', '--target', manifest.target, '--config', configPath];
  if (bundleTargets.length) bundleArgs.push('--bundles', ...bundleTargets);
  run('pnpm', bundleArgs, join(root, 'apps/desktop'), { ...process.env, CARGO_TARGET_DIR: cargoTargetDir });
  // Tauri reports the genuine platform names; preserve them rather than pretending every target is a ZIP/EXE.
  const bundleDir = join(releaseDir, 'bundle');
  if (!existsSync(bundleDir) || !statSync(bundleDir).isDirectory()) throw new Error('Tauri produced no native bundle directory');
  const pins = artifactPins(bundleDir, bundleDir);
  if (!Object.keys(pins).length) throw new Error('Tauri produced no native bundle artifacts');
  copyReleaseTree(bundleDir, join(output, 'bundles'), true);
  if (JSON.stringify(artifactPins(join(output, 'bundles'), join(output, 'bundles'))) !== JSON.stringify(pins)) throw new Error('Native bundle bytes, permissions or framework symlinks changed while copying');
  verifyStage(runtime, manifest);
  writeFileSync(join(output, 'assembly-manifest.json'), JSON.stringify({ ...manifest, stage: 'runtime', runtimeManifestSha256: manifest.stageManifestSha256, artifacts: pins, dataDirectory: staged.dataDirectory }, null, 2));
  console.log(`Verified ${manifest.target} ${variant} native bundles: ${join(output, 'bundles')}`);
  console.log('Windows runtime/installation smoke requires an actual target Windows host. Signed-stage and bundling success are not a validated clean-machine product matrix.');
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try { packageRelease('standard'); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
