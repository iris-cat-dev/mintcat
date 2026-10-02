#!/usr/bin/env node
import { packageRelease } from './package-release.mjs';
try { packageRelease('fixed-webview2'); } catch (error) { console.error(error.message); process.exitCode = 1; }
