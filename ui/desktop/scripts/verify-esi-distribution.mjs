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
const developmentVisualizer = extensions.find(
  (candidate) => candidate.id === 'esi-development-visualizer'
);
assert.equal(developmentVisualizer.type, 'builtin');
assert.equal(developmentVisualizer.enabled, true);
assert.deepEqual(developmentVisualizer.env_keys, []);
const forgeLoop = extensions.find((candidate) => candidate.id === 'forgeloop');
assert.equal(forgeLoop.display_name, 'ForgeLoop (Operator Only)');
assert.match(forgeLoop.description, /operator-only/i);
const wiki = extensions.find((candidate) => candidate.id === 'esi-wiki');
assert.equal(wiki.type, 'streamable_http');
assert.equal(wiki.uri, '', 'ESI-Wiki endpoint must be supplied by the user');
assert.deepEqual(wiki.env_keys, []);
assert.match(wiki.description, /authenticated/i);
assert.match(wiki.description, /configure/i);
assert.ok(!('headers' in wiki), 'ESI-Wiki must not bundle authorization headers');
assert.ok(!('client_id' in wiki), 'ESI-Wiki must not bundle an OAuth client identity');
assert.ok(!('client_secret_key' in wiki), 'ESI-Wiki must not bundle a client secret reference');

const forgeConfig = read('forge.config.ts');
for (const expected of [
  "name: 'ESI-Studio'",
  "executableName: 'esi-studio'",
  "appBundleId: 'ai.esi.studio'",
  "'../../init-config.yaml'",
  "'../../provider-profiles'",
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
assert.match(initConfig, /^ESI_PROVIDER_PROFILE: team\s*$/m);
assert.match(initConfig, /^GOOSE_PROVIDER: chatgpt_codex\s*$/m);
assert.match(initConfig, /^GOOSE_MODEL: gpt-5\.5\s*$/m);

const developmentSkill = readFileSync(
  resolve(repoRoot, 'crates', 'goose', 'src', 'skills', 'builtins', 'esi_local_development.md'),
  'utf8'
);
for (const expected of [
  'name: esi-local-development',
  'normal Goose tools',
  'only authority',
  'Never call ForgeLoop',
  'provider-neutral',
]) {
  assert.ok(developmentSkill.includes(expected), `missing development skill contract: ${expected}`);
}

const visualizerSource = readFileSync(
  resolve(repoRoot, 'crates', 'esi-development-visualizer', 'src', 'lib.rs'),
  'utf8'
);
assert.ok(visualizerSource.includes('ui://esi-development/run'));
assert.ok(visualizerSource.includes('DevelopmentState::load'));
assert.ok(!visualizerSource.includes('FORGELOOP_SERVER'));
assert.ok(!visualizerSource.includes('LITELLM_'));
assert.ok(existsSync(resolve(repoRoot, 'crates', 'esi-development-visualizer', 'src', 'app.html')));
for (const forbidden of [
  /https?:\/\//,
  /LITELLM_/,
  /FORGELOOP_SERVER/,
  /OPENAI_API_KEY/,
  /ANTHROPIC_API_KEY/,
]) {
  assert.ok(!forbidden.test(developmentSkill), `development skill leaks private material: ${forbidden}`);
}

const providerManifest = JSON.parse(
  readFileSync(resolve(repoRoot, 'provider-profiles', 'manifest.json'), 'utf8')
);
assert.equal(providerManifest.schema_version, 1);
const teamProfile = providerManifest.profiles.find((profile) => profile.id === 'team');
const operatorProfile = providerManifest.profiles.find((profile) => profile.id === 'operator');
assert.ok(teamProfile, 'missing team provider profile');
assert.ok(operatorProfile, 'missing operator provider profile');
assert.equal(teamProfile.default_provider, 'chatgpt_codex');
assert.deepEqual(
  teamProfile.providers.map((provider) => [provider.id, provider.role]),
  [
    ['chatgpt_codex', 'primary'],
    ['claude-acp', 'alternative'],
  ]
);
assert.equal(teamProfile.allow_other_goose_providers, false);
assert.equal(teamProfile.allow_private_litellm, false);
assert.equal(teamProfile.allow_private_forgeloop, false);
for (const providerId of [
  'chatgpt_codex',
  'claude-acp',
  'codex-acp',
  'openai',
  'anthropic',
  'litellm',
  'ollama',
  'lmstudio',
]) {
  assert.ok(
    operatorProfile.providers.some((provider) => provider.id === providerId),
    `operator profile missing ${providerId}`
  );
}
assert.equal(operatorProfile.allow_other_goose_providers, true);
assert.equal(operatorProfile.allow_private_litellm, true);
assert.equal(operatorProfile.allow_private_forgeloop, true);

const profileSources = ['manifest.json', 'team.yaml', 'operator.yaml']
  .map((file) => readFileSync(resolve(repoRoot, 'provider-profiles', file), 'utf8'))
  .join('\n');
for (const forbidden of [
  /LITELLM_(?:HOST|API_KEY)\s*[:=]/,
  /FORGELOOP_(?:SERVER|SERVER_BEARER_TOKEN)\s*[:=]/,
  /https?:\/\/[^\s"']*forgeloop/i,
  /(?:^|[\\/])\.(?:codex|claude)(?:[\\/]|$)/m,
  /(?:access|refresh|api)[_-]?token\s*[:=]/i,
]) {
  assert.ok(!forbidden.test(profileSources), `provider profile leaks forbidden data: ${forbidden}`);
}

const pathSource = readFileSync(
  resolve(repoRoot, 'crates', 'goose', 'src', 'config', 'paths.rs'),
  'utf8'
);
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
    expected: ['ESI-Studio backend stopped', 'restart ESI-Studio'],
    forbidden: ['Goose backend stopped', 'restart Goose Desktop'],
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
