#!/usr/bin/env node
import { execFileSync, spawn } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
if (args.includes('--help') || args.includes('-h')) {
  console.log('Usage: pnpm dev [Tauri dev options]\nBuilds only the builtin daemon and desktop, sharing the workspace Cargo cache.\nNo signed game configuration is required. MINTCAT_RUNTIME_CONFIG selects provisioned games.');
} else {
  execFileSync('cargo', ['build', '--locked', '-p', 'mintcat-daemon'], { cwd: root, stdio: 'inherit' });
  const child = spawn('pnpm', ['exec', 'tauri', 'dev', '--config', 'src-tauri/tauri.conf.json', ...args], { cwd: resolve(root, 'apps/desktop'), stdio: 'inherit' });
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
  child.on('error', error => { console.error(error.message); process.exitCode = 1; });
  child.on('exit', (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); });
}
