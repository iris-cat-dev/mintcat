#!/usr/bin/env node
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { provisionOnlineGames } from './dev-games.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
if (args.includes('--help') || args.includes('-h')) {
  console.log('Usage: pnpm dev [Tauri dev options]\nBuilds the daemon and desktop, and provisions signed local DRG and Rogue Core packages from the official patch archives.\nMINTCAT_RUNTIME_CONFIG and dev-games.local/runtime.json override automatic provisioning.');
} else {
  const desktop = resolve(root, 'apps/desktop');
  const cli = createRequire(resolve(desktop, 'package.json')).resolve('@tauri-apps/cli/tauri.js');
  const env = { ...process.env };
  const gamesDirectory = resolve(root, 'dev-games.local');
  const localGames = join(gamesDirectory, 'runtime.json');
  execFileSync('cargo', ['build', '-p', 'mintcat-daemon'], { cwd: root, stdio: 'inherit' });
  if (!env.MINTCAT_RUNTIME_CONFIG) {
    if (existsSync(localGames)) {
      const config = JSON.parse(readFileSync(localGames, 'utf8'));
      if (config?.schemaVersion !== 2 || !config.scopes?.['game:drg'] || !config.scopes?.['game:rogue-core']) {
        throw new Error(`${localGames} must pin game:drg and game:rogue-core in a schemaVersion=2 configuration.`);
      }
      env.MINTCAT_RUNTIME_CONFIG = localGames;
    } else {
      env.MINTCAT_RUNTIME_CONFIG = await provisionOnlineGames(root, gamesDirectory);
    }
  }
  const child = spawn(process.execPath, [cli, 'dev', '--config', 'src-tauri/tauri.conf.json', ...args], { cwd: desktop, env, stdio: 'inherit' });
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
  child.on('error', error => { console.error(error.message); process.exitCode = 1; });
  child.on('exit', (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); });
}
