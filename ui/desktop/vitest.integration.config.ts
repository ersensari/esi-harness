import { defineConfig } from 'vitest/config';
import path from 'path';

require('../../scripts/test-environment.cjs').assertIsolatedTestEnvironment();

export default defineConfig({
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  test: {
    globals: true,
    environment: 'node',
    include: ['tests/integration/**/*.test.ts'],
    testTimeout: 60000,
    hookTimeout: 60000,
    pool: 'forks',
    singleFork: true,
    maxConcurrency: 4,
    silent: 'passed-only',
  },
});
