// Task POST-122: installed Electron + real Wiki/PostgreSQL, no model inference.
// Run with xvfb-run and test-isolated.mjs. All fixture identities are disposable.
import assert from 'node:assert/strict';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { mkdtemp, mkdir, readFile, writeFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { createServer, createConnection } from 'node:net';
import { assertWikiFixtureOwnership } from './wiki-release-ownership.mjs';

const studio = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(join(studio, 'ui/desktop/package.json'));
const { chromium } = require('playwright');
const yaml = require('yaml');
const executable = process.argv[2];
const reportPath = process.argv[3];
assert(executable?.startsWith('/'), 'Pass an absolute installed Electron executable');
assert(reportPath?.startsWith('/'), 'Pass an absolute report path');
const root = await mkdtemp(join(tmpdir(), 'forgeloop-ai-wiki-acceptance-'));
const runId = randomUUID().replaceAll('-', '').slice(0, 8);
const network = `forgeloop-ai-wiki-net-${runId}`;
const postgres = `forgeloop-ai-wiki-db-${runId}`;
const wiki = `forgeloop-ai-wiki-http-${runId}`;
const pgImage = `forgeloop-ai-wiki-pg-${runId}`;
const password = randomBytes(24).toString('base64url');
const tokens = [];
const resources = [];
const checks = [];
let app;
let proxy;
let stage = 'setup';
let success = false;
const docker = (args, input) => execFileSync('docker', args, {
  input, encoding: 'utf8', timeout: 60_000, maxBuffer: 1024 * 1024,
  stdio: ['pipe', 'pipe', 'pipe'],
}).trim();
const labels = (resource) => [
  '--label', 'forgeloop.managed=true', '--label', 'forgeloop.project=forgeloop-ai',
  '--label', `forgeloop.resource=${resource}`, '--label', 'forgeloop.task_id=POST-122',
  '--label', `forgeloop.run_id=${runId}`,
];
const pass = (name) => { checks.push(name); console.log(`PASS ${name}`); };
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(fn, name) {
  const end = Date.now() + 30_000;
  while (Date.now() < end) {
    try { if (await fn()) return; } catch { /* bounded startup polling */ }
    await delay(250);
  }
  throw new Error(`Timeout: ${name}`);
}
function scrub(message) {
  let result = String(message).replaceAll(password, '[redacted]');
  for (const token of tokens) result = result.replaceAll(token, '[redacted]');
  return result.replace(/wiki_session_[\w-]+/g, '[redacted]').slice(0, 1800);
}

const gooseRoot = join(root, 'goose');
const workspace = join(root, 'workspace');
const profile = join(root, 'desktop');
await mkdir(join(gooseRoot, 'config'), { recursive: true });
await mkdir(workspace);
await mkdir(profile);
const env = Object.fromEntries(['PATH', 'DISPLAY', 'XAUTHORITY', 'LANG'].filter((key) => process.env[key]).map((key) => [key, process.env[key]]));
Object.assign(env, {
  GOOSE_PATH_ROOT: gooseRoot, GOOSE_DISABLE_KEYRING: '1', GOOSE_TELEMETRY_ENABLED: 'false',
  XDG_CONFIG_HOME: join(root, 'xdg-config'), XDG_DATA_HOME: join(root, 'xdg-data'),
  XDG_CACHE_HOME: join(root, 'xdg-cache'), XDG_STATE_HOME: join(root, 'xdg-state'),
  TMPDIR: join(root, 'tmp'), OPENAI_API_KEY: 'acceptance-placeholder-not-a-real-key',
  OPENAI_HOST: 'http://127.0.0.1:1', RUST_LOG: 'warn',
});
await mkdir(env.TMPDIR);
await writeFile(join(profile, 'settings.json'), JSON.stringify({ language: 'en', disableAutoDownload: true, enableNotifications: false, showMenuBarIcon: false }));

async function launch() {
  // The release disables Node inspector fuses; use the existing renderer-only
  // test hook instead of modifying the installed executable's protections.
  await rm(join(profile, 'DevToolsActivePort'), { force: true });
  const child = spawn(executable, ['--ozone-platform=x11', `--user-data-dir=${profile}`], {
    cwd: workspace, env: { ...env, ENABLE_PLAYWRIGHT: 'true', PLAYWRIGHT_DEBUG_PORT: '0' },
    detached: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let startupLog = '';
  for (const stream of [child.stdout, child.stderr]) stream.on('data', (chunk) => { startupLog = (startupLog + chunk.toString()).slice(-5000); });
  let browser;
  let page;
  app = {
    process: () => child,
    firstWindow: async () => page,
    close: async () => {
      // Browser.close can wait indefinitely when a packaged Electron app tears
      // down its debug transport. Bound it, then stop only our process group.
      if (browser) await Promise.race([
        browser.newBrowserCDPSession().then((session) => session.send('Browser.close')).catch(() => {}),
        delay(2000),
      ]);
      try { process.kill(-child.pid, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
      if (browser) await Promise.race([browser.close().catch(() => {}), delay(2000)]);
      await delay(500);
    },
  };
  await until(async () => {
    if (child.exitCode != null) throw new Error(`Desktop exited: ${scrub(startupLog)}`);
    return (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).length > 0;
  }, 'Desktop debugging readiness');
  const debugPort = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  assert.match(debugPort, /^\d+$/);
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${debugPort}`, { timeout: 15_000 });
  await until(() => { page = browser.contexts().flatMap((context) => context.pages())[0]; return Boolean(page); }, 'Desktop window');
  await page.waitForFunction(() => document.querySelector('#root')?.children.length > 0);
  page.setDefaultTimeout(20_000);
  await page.evaluate(async () => {
    const address = await window.electron.getAcpUrl();
    const socket = new WebSocket(address);
    const pending = new Map();
    let next = 0;
    socket.addEventListener('message', ({ data }) => {
      const msg = JSON.parse(data);
      if (msg.id != null && pending.has(msg.id)) {
        const request = pending.get(msg.id);
        pending.delete(msg.id);
        clearTimeout(request.timer);
        if (msg.error) request.reject(new Error(msg.error.message));
        else request.resolve(msg.result);
      }
    });
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('ACP connect timed out')), 10_000);
      socket.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
      socket.addEventListener('error', () => reject(new Error('ACP connection failed')), { once: true });
    });
    window.__wikiAcceptanceRpc = (method, params) => new Promise((resolve, reject) => {
      const id = ++next;
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`ACP timeout: ${method}`)); }, 30_000);
      pending.set(id, { resolve, reject, timer });
      socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
    });
    await window.__wikiAcceptanceRpc('initialize', { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: 'post122-acceptance', version: '1' } });
  });
  return page;
}
const rpc = (page, method, params) => page.evaluate(({ method, params }) => window.__wikiAcceptanceRpc(method, params), { method, params });
const tool = async (page, sessionId, name, args = {}) => {
  const result = await rpc(page, '_goose/unstable/tools/call', { sessionId, name, arguments: args });
  assert(!result.isError, `Tool failed: ${name}`);
  return result;
};
const readPlan = async () => JSON.parse(await readFile(join(workspace, '.esi/workspace-plan.json'), 'utf8'));
const sql = (query) => docker(['exec', '-i', postgres, 'psql', '-U', 'postgres', '-d', 'wiki', '-At', '-v', 'ON_ERROR_STOP=1'], query);
const draft = {
  title: 'POST-122 disposable acceptance', description: 'A synthetic inventory widget',
  architecture_notes: 'Local deterministic fixtures with no production data',
  requirements: [{ id: 'R1', description: 'Show inventory', acceptance_criteria: ['Count is visible'], priority: 'must' }],
  tasks: [{ id: 'T1', title: 'Render inventory', description: 'Synthetic fixture only' }],
  innovation: { brief: 'Compare simple layouts', research_findings: ['Local synthetic evidence'], candidates: ['Table'], selected_rationale: 'Table is easy to inspect' },
};

try {
  docker(['build', '--pull=false', ...labels('acceptance-image'), '-t', pgImage, '-f', join(studio, 'scripts/fixtures/wiki-release/postgres.Dockerfile'), join(studio, 'scripts/fixtures/wiki-release')]);
  resources.push(['image', pgImage]);
  docker(['network', 'create', '--internal', ...labels('acceptance-network'), network]);
  resources.push(['network', network]);
  docker(['run', '-d', '--init', '--pull=never', '--name', postgres, ...labels('acceptance-database'), '--network', network,
    '--tmpfs', '/var/lib/postgresql/data:rw', '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', '-e', 'POSTGRES_DB=wiki', pgImage]);
  resources.push(['container', postgres]);
  await until(() => { docker(['exec', postgres, 'pg_isready', '-U', 'postgres']); return true; }, 'database startup');
  // Pass the synthetic password through env, not argv or emitted command output.
  process.env.WIKI_BOOTSTRAP_ADMIN_PASSWORD = password;
  docker(['run', '-d', '--init', '--pull=never', '--name', wiki, ...labels('acceptance-wiki'), '--network', network,
    '-e', `WIKI_DATABASE_URL=postgres://postgres@${postgres}/wiki`,
    '-e', 'WIKI_BOOTSTRAP_ADMIN_PASSWORD', '-e', 'WIKI_BIND_ADDRESS=0.0.0.0:8090', 'forgeloop-ai-esi-wiki:local']);
  delete process.env.WIKI_BOOTSTRAP_ADMIN_PASSWORD;
  resources.push(['container', wiki]);
  const wikiIp = JSON.parse(docker(['container', 'inspect', wiki]))[0].NetworkSettings.Networks[network].IPAddress;
  assert.match(wikiIp, /^\d+\.\d+\.\d+\.\d+$/);
  // Internal Docker networks do not publish ports. The host-only forwarding
  // socket preserves loopback HTTP while both fixtures remain egress-isolated.
  proxy = createServer((incoming) => {
    const outgoing = createConnection({ host: wikiIp, port: 8090 });
    incoming.on('error', () => outgoing.destroy());
    outgoing.on('error', () => incoming.destroy());
    incoming.pipe(outgoing).pipe(incoming);
  });
  await new Promise((resolve) => proxy.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${proxy.address().port}`;
  await until(async () => (await fetch(`${base}/health/ready`)).ok, 'Wiki startup');
  const login = await fetch(`${base}/v1/sessions`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ handle: 'admin', password }) });
  assert.equal(login.status, 201);
  const initial = (await login.json()).token;
  assert(initial);
  tokens.push(initial);
  const extension = { name: 'esi-wiki', type: 'streamable_http', uri: `${base}/mcp`, bundled: true,
    description: 'Disposable Wiki acceptance', env_keys: ['ESI_WIKI_AUTHORIZATION'],
    headers: { Authorization: '${ESI_WIKI_AUTHORIZATION}' }, timeout: 30 };
  await writeFile(join(gooseRoot, 'config/config.yaml'), JSON.stringify({
    GOOSE_PROVIDER: 'openai', GOOSE_MODEL: 'gpt-4o', GOOSE_DISABLE_KEYRING: true, GOOSE_TELEMETRY_ENABLED: false,
    extensions: { 'esi-wiki': { ...extension, enabled: true }, workspaceplan: { name: 'workspaceplan', type: 'platform', enabled: true } },
  }));
  await writeFile(join(gooseRoot, 'config/secrets.yaml'), JSON.stringify({ ESI_WIKI_AUTHORIZATION: `Bearer ${initial}` }), { mode: 0o600 });
  stage = 'installed Desktop startup';
  let page = await launch();
  const originalPid = app.process().pid;
  const session = await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] });
  await tool(page, session.sessionId, 'esi-wiki__wiki_knowledge_list', { scope: 'workspace', workspace_id: 'empty-fixture' });
  pass('installed Desktop uses isolated profile and connected Wiki');
  stage = 'real session expiry';
  const tokenHash = createHash('sha256').update(initial).digest('hex');
  assert.equal(sql(`UPDATE esi_wiki.sessions SET created_at=NOW()-INTERVAL '2 hours', expires_at=NOW()-INTERVAL '1 second' WHERE token_hash=decode('${tokenHash}', 'hex') RETURNING 1;`).split('\n')[0], '1');
  const expired = await fetch(`${base}/mcp`, { method: 'POST', headers: { Authorization: `Bearer ${initial}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }) });
  assert.equal(expired.status, 401);
  await tool(page, session.sessionId, 'workspaceplan__save_draft', draft);
  await tool(page, session.sessionId, 'workspaceplan__approve');
  let plan = await readPlan();
  assert.equal(plan.status, 'approved');
  assert.equal(plan.memory_sync.status, 'pending');
  pass('genuine expired Wiki session leaves approved plan pending');
  stage = 'settings form renewal';
  // Startup navigation may still restore the initial chat. Use the visible
  // navigation control and wait for the mounted extension card, not a hash edit.
  await until(async () => {
    if (await page.locator('#extension-esi-wiki').count()) return true;
    await page.getByText('Extensions', { exact: true }).first().click({ timeout: 2000 });
    return false;
  }, 'Wiki extension screen');
  await page.locator('#extension-esi-wiki').getByRole('button').click();
  await page.getByLabel('Wiki handle', { exact: true }).fill('admin');
  await page.getByLabel('Wiki password', { exact: true }).fill(password);
  await page.getByRole('button', { name: 'Renew Wiki session', exact: true }).click();
  await page.getByRole('status').filter({ hasText: 'Wiki authorization updated' }).waitFor();
  assert.equal(await page.getByLabel('Wiki password', { exact: true }).inputValue(), '');
  assert.equal(app.process().pid, originalPid);
  const secret = yaml.parse(await readFile(join(gooseRoot, 'config/secrets.yaml'), 'utf8')).ESI_WIKI_AUTHORIZATION;
  assert.notEqual(secret, `Bearer ${initial}`);
  tokens.push(secret.replace(/^Bearer /, ''));
  pass('bundled Wiki form renews authorization without Desktop restart');
  stage = 'pending capture retry';
  await tool(page, session.sessionId, 'workspaceplan__retry_memory_sync');
  plan = await readPlan();
  assert.equal(plan.memory_sync.status, 'synced');
  const workspaceId = plan.workspace_id;
  const countRows = () => sql('SELECT count(*) FROM esi_wiki.knowledge_entries;');
  assert.equal(countRows(), '3');
  await tool(page, session.sessionId, 'workspaceplan__approve');
  assert.equal(countRows(), '3');
  pass('retry persists three stable records; unchanged approval is idempotent');
  stage = 'existing Wiki connection reconnect';
  let cachedFailed = false;
  try { await tool(page, session.sessionId, 'esi-wiki__wiki_knowledge_list', { scope: 'workspace', workspace_id: workspaceId }); }
  catch { cachedFailed = true; }
  await rpc(page, '_goose/unstable/session/extensions/remove', { sessionId: session.sessionId, name: 'esi-wiki' });
  await rpc(page, '_goose/unstable/session/extensions/add', { sessionId: session.sessionId, extension: {
    type: 'mcp', server: { type: 'http', name: 'esi-wiki', url: extension.uri,
      headers: [{ name: 'Authorization', value: '${ESI_WIKI_AUTHORIZATION}' }] },
    envKeys: ['ESI_WIKI_AUTHORIZATION'], timeout: 30,
  } });
  await tool(page, session.sessionId, 'esi-wiki__wiki_knowledge_get', { scope: 'workspace', workspace_id: workspaceId, key: 'product-scope' });
  pass(cachedFailed ? 'existing MCP connection requires reconnect; reconnect succeeds without restart' : 'existing MCP connection already refreshed; explicit reconnect also succeeds');
  stage = 'revision and fresh session';
  await tool(page, session.sessionId, 'workspaceplan__save_draft', { ...draft, description: 'Revised synthetic inventory widget' });
  await tool(page, session.sessionId, 'workspaceplan__approve');
  assert.equal(countRows(), '3');
  const second = await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] });
  const record = await tool(page, second.sessionId, 'esi-wiki__wiki_knowledge_get', { scope: 'workspace', workspace_id: workspaceId, key: 'product-scope' });
  assert(JSON.stringify(record).includes('Revised synthetic inventory widget'));
  pass('revision upserts existing rows and a fresh session retrieves revised scope');
  await app.close(); app = null;
  stage = 'fresh Desktop process';
  page = await launch();
  assert.notEqual(app.process().pid, originalPid);
  const third = await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] });
  await tool(page, third.sessionId, 'esi-wiki__wiki_knowledge_get', { scope: 'workspace', workspace_id: workspaceId, key: 'architecture-decision' });
  pass('fresh Desktop process retrieves durable Wiki records');
  await app.close(); app = null;
  stage = 'transcript privacy';
  async function scan(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await scan(path);
      else if (entry.isFile()) {
        const bytes = await readFile(path);
        assert(!bytes.includes(Buffer.from(password)), 'Password persisted in fixture profile');
        if (path !== join(gooseRoot, 'config/secrets.yaml')) {
          for (const token of tokens) assert(!bytes.includes(Buffer.from(token)), 'Bearer persisted outside secret storage');
        }
      }
    }
  }
  await scan(gooseRoot);
  await scan(profile);
  pass('Goose and Desktop profile scans exclude password and out-of-store bearer');
  success = true;
} catch (error) {
  console.error(`FAIL ${stage}: ${scrub(error.message)}`);
  if (app) {
    const page = await app.firstWindow().catch(() => null);
    if (page) console.error(`UI: ${scrub(await page.locator('body').innerText().catch(() => 'unavailable'))}`);
  }
} finally {
  if (app) await app.close().catch(() => {});
  if (proxy) proxy.close();
  let cleanupPassed = true;
  for (const [kind, name] of resources.reverse()) {
    try {
      const inspect = JSON.parse(docker([kind, 'inspect', name]))[0];
      const owned = kind === 'network' ? inspect.Labels : inspect.Config.Labels;
      assertWikiFixtureOwnership(kind, name, owned, runId);
      docker([kind, 'rm', ...(kind === 'container' ? ['-f'] : []), name]);
    } catch (error) { cleanupPassed = false; console.error(`Cleanup failed: ${kind} ${name}: ${scrub(error.message)}`); }
  }
  if (cleanupPassed) pass('owned fixture containers/network/image removed after label verification');
  await writeFile(reportPath, JSON.stringify({ passed: success && cleanupPassed, executable, stage, checks, root, runId }, null, 2));
  if (success && cleanupPassed) await rm(root, { recursive: true });
  else console.error(`Fixture diagnostics retained at ${root}`);
  process.exitCode = success && cleanupPassed ? 0 : 1;
}
