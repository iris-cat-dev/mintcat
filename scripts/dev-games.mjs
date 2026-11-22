import { execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// origin/dev:src/apis/mintcat/release.ts. Legacy manifests carry MD5, not a
// publisher signature; only admit these reviewed SHA-256 pins to local signing.
const origins = [
  'https://yuri-oss-hz.oss-cn-hangzhou.aliyuncs.com',
  'https://yuri-oss-sg.oss-ap-southeast-1.aliyuncs.com',
];
const games = [
  { name: 'drg', project: 'game-drg', file: 'DRG.zip', version: '0.9.0', size: 247252,
    sha256: '3229d50349a848f55ba29cd414b0c537b28135c581c4c4127f140f21a287173f' },
  { name: 'rc', project: 'game-rogue-core', file: 'RC.zip', version: '0.11.0', size: 83180,
    sha256: '83b8409f1a79eeb36a690117c45f68fe53dc26658b043c5f2e81834044d558e3' },
];

const { version } = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));

// Public endpoints only. The daemon collects credentials separately in the OS vault.
const sources = {
  modio: {
    apiBase: 'https://api.mod.io/v1/',
    downloadOrigins: ['https://api.mod.io', 'https://g-2475.modapi.io', 'https://binary.modcdn.io'],
  },
  modcat: {
    apiBase: 'https://modcat.top:8089/',
    downloadRequestOrigin: 'https://modcat.top/',
    downloadOrigins: [],
  },
  nexusmods: {
    apiBase: 'https://api.nexusmods.com/',
    websiteBase: 'https://www.nexusmods.com/',
    navigationOrigins: ['https://www.nexusmods.com', 'https://nexusmods.com'],
    downloadOrigins: ['https://cf-files.nexusmods.com', 'https://cf-files.nexus-cdn.com'],
    applicationName: 'MintCat',
    applicationVersion: version,
  },
};

// Older generated configs omitted this field. Preserve custom sources, including an explicit {}.
function migrateCachedRuntime(config) {
  const runtime = JSON.parse(readFileSync(config, 'utf8'));
  if (Object.hasOwn(runtime, 'sources')) return;
  const temporary = `${config}.${randomBytes(8).toString('hex')}.tmp`;
  writeFileSync(temporary, JSON.stringify({ ...runtime, sources }, null, 2), { flag: 'wx' });
  try {
    renameSync(temporary, config);
  } finally {
    rmSync(temporary, { force: true });
  }
}

async function readOnline(url, limit) {
  const response = await fetch(url, { redirect: 'error', signal: AbortSignal.timeout(20000) });
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`${url}: HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return Buffer.concat(chunks, length);
      length += value.byteLength;
      if (length > limit) throw new Error(`${url}: response exceeds ${limit} bytes`);
      chunks.push(value);
    }
  } finally {
    await reader.cancel();
  }
}

async function downloadPatches() {
  const failures = [];
  for (const origin of origins) {
    try {
      const manifest = JSON.parse(await readOnline(`${origin}/update.json`, 256 * 1024));
      return await Promise.all(games.map(async game => {
        const entry = manifest.find(item => item.name === game.name && item.type === 'blueprint'
          && item.platform === 'windows' && item.channel === 'stable' && item.latestVersion === game.version);
        const path = `/releases/${game.name}/windows/stable/${game.version}/${game.file}`;
        if (!entry || entry.fileSize !== game.size || new URL(entry.downloadUrl, origin).href !== `${origin}${path}`) {
          throw new Error(`${game.name}: expected pinned ${game.version} in the stable Windows manifest`);
        }
        const bytes = await readOnline(`${origin}${path}`, game.size);
        const checksum = algorithm => createHash(algorithm).update(bytes).digest('hex');
        if (bytes.length !== game.size || checksum('md5') !== entry.md5 || checksum('sha256') !== game.sha256) {
          throw new Error(`${game.file}: checksum or size mismatch`);
        }
        console.log(`Verified ${game.file} ${game.version} from ${origin}`);
        return bytes;
      }));
    } catch (error) {
      failures.push(`${origin}: ${error.message}`);
    }
  }
  throw new Error(`Could not obtain the pinned DRG and Rogue Core patches:\n${failures.join('\n')}`);
}

export async function provisionOnlineGames(root, directory) {
  const destination = join(directory, 'online');
  const config = join(destination, 'runtime.json');
  if (existsSync(config)) {
    migrateCachedRuntime(config);
    return config;
  }
  const patches = await downloadPatches();
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const seed = join(directory, 'publisher.seed');
  try {
    writeFileSync(seed, randomBytes(32).toString('base64'), { mode: 0o600, flag: 'wx' });
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
  }
  const run = (command, args) => execFileSync(command, args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
  const target = run('rustc', ['-vV']).match(/^host: (\S+)/m)?.[1];
  if (!target) throw new Error('rustc did not report the host target');
  run('cargo', ['build', '--locked', '-p', 'mintcat-module']);
  const { target_directory: targetDirectory } = JSON.parse(run('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1']));
  const binary = join(targetDirectory, 'debug', `mintcat-module${process.platform === 'win32' ? '.exe' : ''}`);
  const release = (...args) => JSON.parse(run(binary, ['release', ...args]));
  const stage = mkdtempSync(join(directory, 'online-stage-'));
  const json = (name, value) => {
    const path = join(stage, name);
    writeFileSync(path, JSON.stringify(value, null, 2), { flag: 'wx' });
    return path;
  };
  try {
    const packages = games.map((game, index) => {
      const project = join(root, 'modules', game.project);
      const patch = join(stage, game.file);
      writeFileSync(patch, patches[index], { flag: 'wx' });
      const path = join(stage, game.project);
      const record = JSON.parse(run(binary, ['pack', project, '--target', target, '--key', seed, '--output', path, '--patch-archive', patch]));
      const { policy } = JSON.parse(readFileSync(join(project, 'module-project.json'), 'utf8'));
      return { game, record, path, policy };
    });
    const { keyId, publicKey } = release('key-info', '--key', seed);
    const expires = Math.floor(Date.now() / 1000) + 365 * 24 * 3600;
    const role = { keyIds: [keyId], threshold: 1 };
    const spec = json('root-spec.json', {
      header: { kind: 'root', schemaVersion: 1, version: 1, expires },
      keys: { [keyId]: publicKey },
      roles: { root: role, targets: role, snapshot: role, timestamp: role },
      publishers: Object.fromEntries(packages.map(({ record }) => [record.manifest.publisher, { [keyId]: publicKey }])),
    });
    const pinned = join(stage, 'root.json');
    release('sign-root', '--spec', spec, '--key', seed, '--output', pinned);
    const policies = packages.map(entry => entry.policy);
    const policy = json('policy.json', {
      ...policies[0], target,
      permissions: [...new Map(policies.flatMap(p => p.permissions).map(p => [JSON.stringify(p), p])).values()],
    });
    const scopes = {};
    let bundle;
    for (const [index, { game, record, path }] of packages.entries()) {
      const scopeId = record.manifest.contributes.scopeId;
      const pin = { releaseId: `dev-${game.name}-${game.version}`, sequence: 1, channel: 'stable' };
      scopes[scopeId] = pin;
      const plan = json(`${game.name}-plan.json`, {
        scopeId, ...pin, target, minHostApi: record.manifest.hostApi,
        roots: [record.digest],
        providerBindings: record.manifest.provides.map(contract => ({ scopeId, contractId: contract.contractId,
          contractVersion: contract.version, providerModuleId: record.manifest.id, providerDigest: record.digest })),
        singletonRequirements: [], resourceDependencies: {},
        cohort: { salt: 'mintcat-local-dev', basisPoints: 10000 },
      });
      const next = join(stage, `${game.name}-bundle.json`);
      release('compose', '--root', pinned, '--policy', policy, '--plan', plan, '--package', path,
        '--key', seed, '--metadata-version', String(index + 1), '--expires', String(expires),
        ...(bundle ? ['--previous', bundle] : []), '--output', next);
      bundle = next;
    }
    release('publish', '--root', pinned, '--policy', policy, '--repository', join(stage, 'repository'),
      '--bundle', bundle, ...packages.flatMap(({ path }) => ['--package', path]));
    json('runtime.json', { schemaVersion: 2, trustRoot: 'root.json', verificationPolicy: 'policy.json', repositoryPath: 'repository', scopes, sources });
    try {
      renameSync(stage, destination);
    } catch (error) {
      if (!existsSync(config)) throw error;
    }
    return config;
  } finally {
    rmSync(stage, { recursive: true, force: true, maxRetries: 3 });
  }
}
