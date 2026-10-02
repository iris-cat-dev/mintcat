#!/usr/bin/env node
import { packageRelease } from './package-release.mjs';
try { packageRelease('cli'); } catch (error) { console.error(error.message); process.exitCode = 1; }
