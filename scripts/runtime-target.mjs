#!/usr/bin/env node
import process from 'node:process';

import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const triples = new Map([
  ['darwin-arm64', 'aarch64-apple-darwin'],
  ['darwin-x64', 'x86_64-apple-darwin'],
  ['linux-x64', 'x86_64-unknown-linux-gnu'],
  ['linux-arm64', 'aarch64-unknown-linux-gnu'],
  ['win32-x64', 'x86_64-pc-windows-msvc'],
]);

const platformKey = `${process.platform}-${process.arch}`;
const targetTriple = triples.get(platformKey);
if (!targetTriple) {
  console.error(`Unsupported development host: ${platformKey}`);
  process.exit(1);
}

const result = {
  schemaVersion: 1,
  platformKey,
  targetTriple,
  cargoTargetDirectory: process.env.CARGO_TARGET_DIR ? resolve(dirname(fileURLToPath(import.meta.url)), '..', process.env.CARGO_TARGET_DIR) : 'target',
  desktopOutputDirectory: 'apps/desktop/dist',
  desktopDevelopmentPort: 1430,
};

if (process.argv.includes('--json')) {
  console.log(JSON.stringify(result));
} else {
  console.log(targetTriple);
}
