// Read-only extension inventory through a running Desktop, without model inference.
// Creates one labelled diagnostic session in a disposable working directory.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const require = createRequire(new URL('../ui/desktop/package.json', import.meta.url));
const { chromium } = require('playwright');
const [port, reportPath, configFile] = process.argv.slice(2);
assert(/^\d+$/.test(port) && reportPath?.startsWith('/'));
const workspace = await mkdtemp(join(tmpdir(), 'forgeloop-ai-extension-inventory-'));
const stored = configFile ? require('yaml').parse(await readFile(configFile, 'utf8')) : null;
const inventory = stored ? Object.entries(stored.extensions ?? {}).map(([key, entry]) => ({
  configKey: (entry.name ?? key).replace(/\s/g, '').replace(/[^a-zA-Z0-9_-]/g, '_').toLowerCase(), enabled: entry.enabled,
})) : null;
const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
try {
  const page = browser.contexts().flatMap(context => context.pages())[0];
  const report = await page.evaluate(async ({ workspace, inventory }) => {
    const socket = new WebSocket(await window.electron.getAcpUrl());
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, { once: true });
      socket.addEventListener('error', reject, { once: true });
    });
    const pending = new Map(); let id = 0;
    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data), job = pending.get(message.id);
      if (!job) return;
      pending.delete(message.id); clearTimeout(job.timer);
      message.error ? job.reject(new Error(`ACP ${job.method} failed (${message.error.code})`)) : job.resolve(message.result);
    });
    const rpc = (method, params) => new Promise((resolve, reject) => {
      const requestId = ++id;
      const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`Timeout: ${method}`)); }, 90000);
      pending.set(requestId, { resolve, reject, timer, method });
      socket.send(JSON.stringify({ jsonrpc: '2.0', id: requestId, method, params }));
    });
    try {
      await rpc('initialize', { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: 'extension-inventory', version: '1' } });
      const configured = await rpc('_goose/unstable/config/extensions/list', {});
      const session = await rpc('session/new', { cwd: workspace, mcpServers: [] });
      const rows = [];
      for (const entry of inventory ?? configured.extensions) {
        const key = entry.configKey;
        const trust = await rpc('_goose/esi/extension-trust', { configKey: key });
        const tools = entry.enabled ? await rpc('_goose/unstable/tools/list', { sessionId: session.sessionId, extensionName: key }) : { tools: [] };
        rows.push({ key, enabled: entry.enabled, trusted: trust.trusted, tools: tools.tools.length });
      }
      return { sessionId: session.sessionId, workspace, extensions: rows };
    } finally { socket.close(); }
  }, { workspace, inventory });
  await writeFile(reportPath, JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
} finally { await browser.close(); }
