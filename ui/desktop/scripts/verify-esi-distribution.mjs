import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const desktopRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = resolve(desktopRoot, '..', '..');
const read = (...parts) => readFileSync(resolve(desktopRoot, ...parts), 'utf8');

const packageJson = JSON.parse(read('package.json'));
assert.equal(packageJson.name, 'esi-studio-app');
assert.equal(packageJson.productName, 'ESI-Studio');

const extensions = JSON.parse(
  read('src', 'components', 'settings', 'extensions', 'bundled-extensions.json')
);
for (const id of ['esi-wiki', 'esi-innovation', 'forgeloop']) {
  const extension = extensions.find((candidate) => candidate.id === id);
  assert.ok(extension, `missing ${id} extension placeholder`);
  assert.equal(extension.enabled, false, `${id} must be disabled by default`);
  assert.equal(extension.bundled, true, `${id} must be managed by the distribution`);
}

const forgeConfig = read('forge.config.ts');
for (const expected of [
  "name: 'ESI-Studio'",
  "executableName: 'esi-studio'",
  "appBundleId: 'ai.esi.studio'",
  "owner: process.env.GITHUB_OWNER || 'ersensari'",
  "name: process.env.GITHUB_REPO || 'esi-harness'",
]) {
  assert.ok(forgeConfig.includes(expected), `missing forge identity: ${expected}`);
}

const updateConfig = read('src', 'app-update.yml');
assert.ok(updateConfig.includes('owner: ersensari'));
assert.ok(updateConfig.includes('repo: esi-harness'));
assert.ok(updateConfig.includes('updaterCacheDirName: esi-studio-updater'));

for (const template of ['forge.deb.desktop', 'forge.rpm.desktop']) {
  const desktopTemplate = read(template);
  assert.ok(desktopTemplate.includes('Name=ESI-Studio'));
  assert.ok(desktopTemplate.includes('/usr/lib/esi-studio/esi-studio'));
}

for (const icon of ['esi-icon.svg', 'esi-icon.png', 'esi-icon-512.png', 'esi-icon.ico']) {
  assert.ok(existsSync(resolve(desktopRoot, 'src', 'images', icon)), `missing ${icon}`);
}

const initConfig = readFileSync(resolve(repoRoot, 'init-config.yaml'), 'utf8');
assert.match(initConfig, /^GOOSE_MODE: smart_approve\s*$/m);

const pathSource = readFileSync(resolve(repoRoot, 'crates', 'goose', 'src', 'config', 'paths.rs'), 'utf8');
assert.ok(pathSource.includes('app_name: "esi-studio"'));
assert.ok(pathSource.includes('top_level_domain: "ESI"'));

const cliSource = readFileSync(resolve(repoRoot, 'crates', 'goose-cli', 'src', 'cli.rs'), 'utf8');
for (const expected of [
  'name = "esi-studio"',
  'Configure ESI-Studio settings',
  'Display ESI-Studio information',
  'Terminal-integrated ESI-Studio session',
  'default_value = "esi-studio"',
]) {
  assert.ok(cliSource.includes(expected), `missing CLI identity: ${expected}`);
}
for (const forbidden of [
  'Configure goose settings',
  'Display goose information',
  'Terminal-integrated goose session',
]) {
  assert.ok(!cliSource.includes(forbidden), `stale CLI identity: ${forbidden}`);
}

const configureSource = readFileSync(
  resolve(repoRoot, 'crates', 'goose-cli', 'src', 'commands', 'configure.rs'),
  'utf8'
);
assert.ok(configureSource.includes('Welcome to ESI-Studio!'));
assert.ok(configureSource.includes('esi-studio configure'));
assert.ok(!configureSource.includes('style("goose configure")'));

const i18nSource = read('src', 'i18n', 'index.ts');
assert.ok(i18nSource.includes("message.replace(GOOSE_BRAND_PATTERN, 'ESI-Studio')"));
assert.ok(i18nSource.includes('(?!:\\/\\/)'), 'goose:// compatibility links must be preserved');

const nativeBranding = [
  {
    source: read('src', 'main.ts'),
    expected: ['Focus ESI-Studio Window', 'About ESI-Studio', "title: 'ESI-Studio'"],
    forbidden: ['Focus Goose Window', 'About Goose', "title: 'Goose'", 'Goose Failed to Start'],
  },
  {
    source: read('src', 'utils', 'autoUpdater.ts'),
    expected: ['ESI-Studio.app', 'quit ESI-Studio', 'launch ESI-Studio'],
    forbidden: ['Goose.app', 'quit Goose', 'launch Goose'],
  },
  {
    source: read('src', 'gooseServeLeaseRegistry.ts'),
    expected: ["ESI-Studio backend stopped", 'restart ESI-Studio'],
    forbidden: ["Goose backend stopped", 'restart Goose Desktop'],
  },
  {
    source: read('src', 'toasts.tsx'),
    expected: ['Ask ESI-Studio'],
    forbidden: ['Ask goose'],
  },
];

for (const { source, expected, forbidden } of nativeBranding) {
  for (const text of expected) assert.ok(source.includes(text), `missing native brand: ${text}`);
  for (const text of forbidden) assert.ok(!source.includes(text), `stale native brand: ${text}`);
}

console.log('ESI distribution contract: PASS');