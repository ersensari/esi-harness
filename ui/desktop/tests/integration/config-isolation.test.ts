import { expect, test } from 'vitest';
import { mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { discoverTestCases } from './test_providers_lib';

test('integration runner and child writes use disposable configuration', () => {
  const root = require('../../../../scripts/test-environment.cjs').assertIsolatedTestEnvironment();
  const config = join(root, 'config');
  mkdirSync(config, { recursive: true });
  writeFileSync(join(config, 'isolation-fixture.json'), '{}');
  const child = spawnSync(process.execPath, ['-e', `
    require('fs').writeFileSync(require('path').join(process.env.GOOSE_PATH_ROOT,
      'config/isolation-fixture.json'), JSON.stringify({ isolated: true }));
  `], { env: process.env, encoding: 'utf8' });
  expect(child.status).toBe(0);
  expect(JSON.parse(readFileSync(join(config, 'isolation-fixture.json'), 'utf8'))).toEqual({ isolated: true });
});

test('provider discovery does not load dotenv or use credentials without opt-in', () => {
  const before = { ...process.env };
  try {
    delete process.env.ESI_TEST_LIVE_PROVIDERS;
    process.env.OPENAI_API_KEY = 'fixture-not-a-secret';
    const baseline = { ...process.env };
    const cases = discoverTestCases();
    expect(cases.length).toBeGreaterThan(0);
    expect(cases.every(tc => !tc.available)).toBe(true);
    expect(process.env).toEqual(baseline);
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in before)) delete process.env[key];
    Object.assign(process.env, before);
  }
});
