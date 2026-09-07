import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { configDigest, runIsolated } from './test-isolated.mjs';
import guard from './test-environment.cjs';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'esi-isolation-fixture-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const config = join(root, 'config');
  await mkdir(config);
  await writeFile(join(config, 'config.yaml'), 'plugins: {}\n');
  return { root, config };
}

test('child config writes cannot touch inherited Studio config; temporary root is cleaned', async (t) => {
  const { root, config } = await fixture(t);
  const marker = join(root, 'marker.json');
  const before = await configDigest([config]);
  const code = await runIsolated(process.execPath, ['--input-type=module', '-e', `
    import { mkdirSync, writeFileSync } from 'node:fs';
    import { join } from 'node:path';
    const root = process.env.GOOSE_PATH_ROOT;
    mkdirSync(join(root, 'config'), { recursive: true });
    writeFileSync(join(root, 'config/config.yaml'), 'plugins: {fixture: true}');
    writeFileSync(process.argv[1], JSON.stringify({root, keyring: process.env.GOOSE_DISABLE_KEYRING,
      additional: process.env.GOOSE_ADDITIONAL_CONFIG_FILES, plugins: process.env.PLUGINS}));
  `, marker], { env: {...process.env, GOOSE_PATH_ROOT: root, PLUGINS: 'live',
    GOOSE_ADDITIONAL_CONFIG_FILES: join(config, 'config.yaml')}, protectedPaths: [config] });
  assert.equal(code, 0);
  const result = JSON.parse(await readFile(marker, 'utf8'));
  assert.notEqual(result.root, root);
  assert.equal(result.keyring, '1');
  assert.equal(result.additional, '');
  assert.equal(result.plugins, undefined);
  assert.equal(await configDigest([config]), before);
  await assert.rejects(access(result.root));
});

test('nonzero status and literal arguments survive without shell evaluation', async (t) => {
  const { config } = await fixture(t);
  const code = await runIsolated(process.execPath, ['-e',
    'if (process.argv[1] !== "$(false);`false`") process.exit(9); process.exit(7)',
    '$(false);`false`'], {protectedPaths: [config]});
  assert.equal(code, 7);
});

test('spawn failure is reported', async (t) => {
  const { root, config } = await fixture(t);
  await assert.rejects(runIsolated(join(root, 'missing-command'), [], {
    protectedPaths: [config],
  }), {code: 'ENOENT'});
});

test('digest catches additions, deletions and content changes', async (t) => {
  const { config } = await fixture(t);
  const before = await configDigest([config]);
  await writeFile(join(config, 'config.yaml'), 'plugins: {unexpected: true}\n');
  assert.notEqual(await configDigest([config]), before);
  await rm(join(config, 'config.yaml'));
  const deleted = await configDigest([config]);
  assert.notEqual(deleted, before);
  await writeFile(join(config, 'extra.yaml'), 'new');
  assert.notEqual(await configDigest([config]), deleted);
});

test('protected mutation fails without restoring over concurrent user changes', async (t) => {
  const { config } = await fixture(t);
  let retained;
  await assert.rejects(runIsolated(process.execPath, ['-e',
    "require('fs').writeFileSync(process.argv[1], 'concurrent user change')",
    join(config, 'config.yaml')], {protectedPaths: [config]}), (error) => {
      retained = error.message.split('retained at ')[1];
      return Boolean(retained);
    });
  t.after(() => rm(retained, { recursive: true, force: true }));
  assert.equal(await readFile(join(config, 'config.yaml'), 'utf8'), 'concurrent user change');
});

test('runner establishes the fail-closed test environment before imports', async (t) => {
  const { config } = await fixture(t);
  assert.throws(() => guard.assertIsolatedTestEnvironment({}), /isolated runner/);
  const guardPath = new URL('./test-environment.cjs', import.meta.url).pathname;
  assert.equal(await runIsolated(process.execPath, ['-e',
    `const {assertIsolatedTestEnvironment: check} = require(process.argv[1]);
     check();
     for (const key of ['GOOSE_PATH_ROOT', 'GOOSE_DISABLE_KEYRING', 'XDG_CONFIG_HOME', 'APPDATA']) {
       const env = {...process.env, [key]: 'invalid'};
       require('assert').throws(() => check(env));
     }`, guardPath], { protectedPaths: [config] }), 0);
});

test('every executable Desktop test script routes through isolation', async () => {
  const manifest = JSON.parse(await readFile(new URL('../ui/desktop/package.json', import.meta.url), 'utf8'));
  const exempt = new Set(['test-e2e:report', 'test:esi-distribution']); // no test process/config writer
  for (const [name, command] of Object.entries(manifest.scripts)) {
    if (name.startsWith('test') && !exempt.has(name)) {
      assert(command.includes('node scripts/test-isolated.mjs'), `${name} bypasses isolation`);
    }
  }
});

test('direct Vitest and Playwright entry points reject missing isolation before loading tests', async (t) => {
  const { root } = await fixture(t);
  const desktop = new URL('../ui/desktop/', import.meta.url).pathname;
  const require = createRequire(join(desktop, 'package.json'));
  for (const [pkg, bin, args] of [
    ['vitest', 'vitest', ['run', 'src/acp/__tests__/modelProfiles.test.ts']],
    ['vitest', 'vitest', ['run', '--config', 'vitest.integration.config.ts']],
    ['@playwright/test', 'playwright', ['test', '--list']],
  ]) {
    const manifestPath = require.resolve(`${pkg}/package.json`);
    const entry = resolve(dirname(manifestPath), require(manifestPath).bin[bin]);
    const result = spawnSync(process.execPath, [entry, ...args], {
      cwd: desktop, env: { ...process.env, ESI_STUDIO_TEST_ROOT: '', GOOSE_PATH_ROOT: root },
      encoding: 'utf8', timeout: 20000,
    });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stdout + result.stderr, /Studio tests require the isolated runner/);
  }
});
