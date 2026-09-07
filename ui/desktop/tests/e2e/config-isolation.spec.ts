import { test, expect } from '@playwright/test';
import { mkdirSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';

// This exercises the Playwright process/worker boundary, without live providers
// or a browser. Packaged Electron isolation is separately covered by POST-141.
test('Playwright workers and subprocesses inherit the isolated config root', () => {
  const root = require('../../../../scripts/test-environment.cjs').assertIsolatedTestEnvironment();
  mkdirSync(join(root, 'config'), { recursive: true });
  const child = spawnSync(process.execPath, ['-e', `
    require('fs').writeFileSync(require('path').join(process.env.GOOSE_PATH_ROOT,
      'config/playwright-fixture.json'), JSON.stringify({ keyring: process.env.GOOSE_DISABLE_KEYRING }));
  `], { env: process.env, encoding: 'utf8' });
  expect(child.status).toBe(0);
  expect(JSON.parse(readFileSync(join(root, 'config/playwright-fixture.json'), 'utf8'))).toEqual({ keyring: '1' });
});
