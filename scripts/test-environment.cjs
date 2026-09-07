const { isAbsolute, join } = require('node:path');

// Fail before importing test files. This is a runner contract, not a sandbox.
function assertIsolatedTestEnvironment(env = process.env) {
  const root = env.ESI_STUDIO_TEST_ROOT;
  if (!root || !isAbsolute(root) || env.GOOSE_PATH_ROOT !== root ||
      env.GOOSE_DISABLE_KEYRING !== '1' || env.GOOSE_ADDITIONAL_CONFIG_FILES !== '' ||
      env.XDG_CONFIG_HOME !== join(root, 'xdg/config') ||
      env.APPDATA !== join(root, 'appdata/roaming') || env.PLUGINS) {
    throw new Error('Studio tests require the isolated runner. Use a pnpm test script or node scripts/test-isolated.mjs <command> [args...] from Studio root.');
  }
  return root;
}

module.exports = { assertIsolatedTestEnvironment };
