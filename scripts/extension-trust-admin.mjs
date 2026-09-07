// Explicit operator maintenance. This is not an agent tool or an automatic migration.
// Back up the user's config before using --trust-all-existing during installation.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../ui/desktop/package.json', import.meta.url));
const yaml = require('yaml');

const [binary, action, configFile] = process.argv.slice(2);
assert(binary?.startsWith('/'), 'Pass an absolute goose executable');
assert(['--list', '--trust-all-existing'].includes(action), 'Use --list or --trust-all-existing');
if (action === '--trust-all-existing') assert(configFile?.startsWith('/'), 'Supply the absolute host config path to include hidden extensions');
const child = spawn(binary, ['acp'], { stdio: ['pipe', 'pipe', 'pipe'] });
const pending = new Map();
let next = 0;
// Never emit backend logs or extension connection details, which may contain credentials.
child.stderr.resume();
const lines = createInterface({ input: child.stdout });
lines.on('line', line => {
  let message;
  try { message = JSON.parse(line); } catch { return; }
  const job = pending.get(message.id);
  if (!job) return;
  pending.delete(message.id); clearTimeout(job.timer);
  if (message.error) job.reject(new Error(`ACP ${job.method} failed (${message.error.code}); no configuration details emitted`));
  else job.resolve(message.result);
});
function rpc(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++next;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`ACP timeout: ${method}`)); }, 30000);
    pending.set(id, { resolve, reject, timer, method });
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
  });
}
try {
  await rpc('initialize', { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: 'extension-trust-admin', version: '1' } });
  let { extensions } = await rpc('_goose/unstable/config/extensions/list', {});
  if (configFile) {
    const stored = yaml.parse(await readFile(configFile, 'utf8'));
    extensions = Object.entries(stored.extensions ?? {}).map(([key, entry]) => ({
      configKey: (entry.name ?? key).replace(/\s/g, '').replace(/[^a-zA-Z0-9_-]/g, '_').toLowerCase(),
      enabled: entry.enabled,
    }));
  }
  const results = [];
  for (const entry of extensions) {
    const configKey = entry.configKey;
    assert(configKey, 'Expected persisted configuration key');
    let result;
    try {
      result = await rpc('_goose/esi/extension-trust', {
        configKey, ...(action === '--trust-all-existing' ? { trusted: true } : {}),
      });
    } catch (error) {
      if (action !== '--list') throw error;
      results.push({ configKey, enabled: entry.enabled, status: 'unavailable-or-not-persisted' });
      continue;
    }
    results.push({ configKey, enabled: entry.enabled, trusted: result.trusted });
  }
  console.log(JSON.stringify({ action, extensions: results }, null, 2));
} catch (error) {
  console.error(String(error)); process.exitCode = 1;
} finally {
  lines.close(); child.stdin.end();
  for (const job of pending.values()) clearTimeout(job.timer);
  child.kill('SIGTERM');
}
