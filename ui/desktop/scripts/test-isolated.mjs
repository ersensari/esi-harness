import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { runIsolated } from '../../../scripts/test-isolated.mjs';

try {
  const require = createRequire(import.meta.url);
  const args = process.argv.slice(2);
  const playwright = args[0] === '--playwright';
  if (playwright) args.shift();
  const manifestPath = require.resolve(playwright ? '@playwright/test/package.json' : 'vitest/package.json');
  const manifest = require(manifestPath);
  const entry = resolve(dirname(manifestPath), manifest.bin[playwright ? 'playwright' : 'vitest']);
  process.exitCode = await runIsolated(process.execPath, [entry, ...args]);
  console.log(`Studio config digest unchanged; child exit ${process.exitCode}`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
