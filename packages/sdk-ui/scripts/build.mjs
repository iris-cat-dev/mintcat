#!/usr/bin/env node
import { parseArgs } from 'node:util';
import { readFile, writeFile, mkdir, copyFile, stat } from 'node:fs/promises';
import { resolve, join, dirname, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

const { values } = parseArgs({ options: { source: { type: 'string' }, output: { type: 'string' } }, strict: true });
if (!values.source || !values.output) throw new Error('Usage: mintcat-ui-build --source <module/ui> --output <staging/ui>');
const source = resolve(values.source); const output = resolve(values.output);
const sharedHost = source === resolve(dirname(fileURLToPath(import.meta.url)), '../../../tools/module-dev/host');
if (output === source || !relative(source, output).startsWith('..')) throw new Error('UI output must be outside the source directory');
const descriptor = JSON.parse(await readFile(join(source, 'contributions.json'), 'utf8'));
if (descriptor.schemaVersion !== 1 || descriptor.runtimeVersion !== 1 || descriptor.entry !== 'ui/index.js') throw new Error('Unsupported UI descriptor entry/ABI');
await mkdir(output, { recursive: true });
const result = await build({
  entryPoints: [join(source, 'index.ts')], outdir: output, entryNames: 'index', chunkNames: 'chunks/[name]-[hash]', assetNames: 'assets/[hash]',
  bundle: true, splitting: true, platform: 'browser', format: 'esm', target: 'es2020', sourcemap: false, metafile: true,
  jsx: 'transform', jsxFactory: 'React.createElement', jsxFragment: 'React.Fragment',
  loader: { '.woff': 'file', '.woff2': 'file', '.svg': 'file', '.png': 'file', '.jpg': 'file', '.jpeg': 'file' },
  // React and ReactDOM come from the shared host binding, never a second
  // hook dispatcher inside an official UI module package.
  plugins: [{ name: 'single-ui-runtime', setup(builder) {
    if (!sharedHost) {
      builder.onResolve({ filter: /^(react|react-dom)(\/.*)?$/ }, () => { throw new Error('Use the shared UI runtime binding, not bundled React imports'); });
      builder.onResolve({ filter: /^(antd|@ant-design\/icons)(\/.*)?$/ }, () => { throw new Error('Use the shared Ant Design runtime binding, not bundled UI kit imports'); });
    }
    else builder.onResolve({ filter: /^(react|react-dom|antd|@ant-design\/icons)(\/.*)?$/ }, args => ({ path: fileURLToPath(import.meta.resolve(args.path)) }));
    builder.onResolve({ filter: /^(@tauri-apps\/|@\/)/ }, () => { throw new Error('UI modules must use the SDK, not native plugins or legacy application internals'); });
    builder.onResolve({ filter: /^@mintcat\// }, args => {
      if (args.path !== '@mintcat/sdk-ui' && args.path !== '@mintcat/sdk-ts') throw new Error('UI modules cannot import another module implementation');
      return { path: fileURLToPath(import.meta.resolve(args.path)) };
    });
    builder.onResolve({ filter: /^\./ }, args => {
      const importer = relative(source, args.importer);
      if (importer.startsWith('..')) return;
      const destination = relative(source, resolve(args.resolveDir, args.path));
      if (destination.startsWith('..')) throw new Error('UI relative imports must remain inside their owning UI package');
    });
  } }],
});
const generatedCss = Object.keys(result.metafile.outputs).filter(path => path.endsWith('.css')).map(path => `ui/${relative(output, resolve(path)).replaceAll('\\', '/')}`);
for (const resource of descriptor.css) {
  if (typeof resource !== 'string' || !/^ui\/[A-Za-z0-9._/-]+\.css$/.test(resource) || resource.split('/').includes('..')) throw new Error('Style is not a portable signed UI resource');
  if (generatedCss.includes(resource)) throw new Error(`Declared style collides with an automatically emitted CSS bundle: ${resource}`);
  const suffix = resource.slice(3); const from = join(source, suffix); const to = join(output, suffix);
  if (!(await stat(from)).isFile()) throw new Error(`Declared style is missing: ${resource}`);
  await mkdir(dirname(to), { recursive: true }); await copyFile(from, to);
}
descriptor.css = [...new Set([...descriptor.css, ...generatedCss])];
await writeFile(join(output, 'contributions.json'), `${JSON.stringify(descriptor, null, 2)}\n`);
console.log(JSON.stringify({ moduleId: descriptor.moduleId, uiDirectory: output, entry: descriptor.entry }));
