// POST-141: real packaged renderer/backend, disposable provider and config only.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
const studio = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(join(studio, 'ui/desktop/package.json'));
const { chromium } = require('playwright');
const [executable, reportPath, loop = 'legacy', mode = 'manual'] = process.argv.slice(2);
const discovery = mode === 'discovery';
const trust = mode === 'trust';
const planning = mode === 'planning';
const controller = mode === 'controller';
let controllerFixture;
const restricted = mode === 'restricted-discovery';
assert(executable?.startsWith('/') && reportPath?.startsWith('/'));
const root = await mkdtemp(join(tmpdir(), 'forgeloop-ai-model-profiles-'));
const gooseRoot = join(root, 'goose'), profile = join(root, 'desktop'), workspace = join(root, 'workspace');
for (const path of [join(gooseRoot, 'config/custom_providers'), profile, workspace, join(root, 'tmp')]) await mkdir(path, { recursive: true });
if (controller) {
  const { controllerFixtureTools } = await import('./controller-acceptance.mjs');
  controllerFixture = controllerFixtureTools(workspace);
}
if (trust) await writeFile(join(root, 'outside.txt'), 'POST147_OUTSIDE_WORKSPACE');
const captures = [], checks = [];
let app, browser, success = false;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const pass = name => { checks.push(name); console.log(`PASS ${name}`); };
async function until(fn, label, timeout = 30000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { if (await fn()) return; await delay(100); }
  throw new Error(`Timeout: ${label}`);
}
const server = createServer(async (req, res) => {
  if (restricted && req.url === '/model/info') { res.writeHead(403); return res.end('{}'); }
  if (discovery && req.url === '/model/info') {
    res.setHeader('content-type', 'application/json');
    return res.end(JSON.stringify({ data: [{ model_name: 'manual',
      model_info: { context_window: 262144, max_output_tokens: 32768, supports_reasoning: true,
        supported_reasoning_efforts: ['none', 'low', 'high', 'xhigh'] },
      litellm_params: { max_tokens: 32768, temperature: 0.65, top_p: 0.95, reasoning_effort: 'xhigh',
        extra_body: { top_k: 20, min_p: 0, repetition_penalty: 1,
          chat_template_kwargs: { preserve_thinking: true } } } }] }));
  }
  if (req.url === '/v1/models') {
    res.setHeader('content-type', 'application/json');
    return res.end(JSON.stringify({ data: [restricted
      ? { id: 'manual', max_input_tokens: 229376, max_output_tokens: 32768 }
      : { id: 'manual', owned_by: 'unsloth-studio', context_length: 32768 }] }));
  }
  if (req.url !== '/v1/chat/completions') { res.writeHead(404); return res.end(); }
  let raw = '';
  for await (const chunk of req) { raw += chunk; if (raw.length > 2 * 1024 * 1024) { req.destroy(); return; } }
  const body = JSON.parse(raw);
  captures.push(body);
  if (JSON.stringify(body.messages).includes('POST141 busy')) await delay(1500);
  const message = { role: 'assistant', content: 'POST141 OK', ...(body.enable_thinking ? { reasoning_content: 'fixture reasoning' } : {}) };
  if (controller && body.messages.at(-1)?.role === 'user') {
    const text = JSON.stringify(body.messages.at(-1));
    const entry = Object.entries(controllerFixture).find(([key]) => text.includes(`M18005 ${key}`));
    if (entry) { const [key, tool] = entry; message.content = null; message.tool_calls = [{ id: `controller-${key}`, type: 'function', function: { name: tool.name, arguments: JSON.stringify(tool.arguments) } }]; }
  }
  if (trust && body.messages.at(-1)?.role === 'user') {
    const shell = JSON.stringify(body.messages.at(-1)).includes('POST147 shell');
    message.content = null;
    message.tool_calls = [{ id: 'trust-call', type: 'function', function: {
      name: shell ? 'shell' : 'todo__todo_write', arguments: JSON.stringify(shell
        ? { command: `cat ${JSON.stringify(join(root, 'outside.txt'))}` }
        : { content: 'POST147 trusted loop execution' }),
    } }];
  }
  if (planning && body.messages.at(-1)?.role === 'user' && JSON.stringify(body.messages.at(-1)).includes('M17005 show plan')) {
    message.content = null;
    message.tool_calls = [{ id: 'plan-canvas-call', type: 'function', function: {
      name: 'esi-development-visualizer__show_development_loop',
      arguments: JSON.stringify({ workspace_path: workspace }),
    } }];
  }
  res.setHeader('content-type', 'application/json');
  res.end(JSON.stringify({ id: 'fixture-completion', object: 'chat.completion', created: 1, model: 'manual',
    choices: [{ index: 0, message, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 20, completion_tokens: 5, total_tokens: 25 } }));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}`;
await writeFile(join(gooseRoot, 'config/custom_providers/custom_profile_fixture.json'), JSON.stringify({
  name: 'custom_profile_fixture', engine: 'openai', display_name: 'Profile fixture', api_key_env: '', base_url: base,
  models: [{ name: 'manual', context_limit: 128000 }], requires_auth: false, supports_streaming: false,
}));
await writeFile(join(gooseRoot, 'config/config.yaml'), JSON.stringify({
  GOOSE_PROVIDER: 'custom_profile_fixture', GOOSE_MODEL: 'manual', GOOSE_DISABLE_KEYRING: true,
  GOOSE_TELEMETRY_ENABLED: false, GOOSE_MODE: trust || planning || controller ? 'auto' : 'chat',
  extensions: planning || controller ? {
    ...(controller ? { controller: { type: 'platform', name: 'controller', enabled: true } } : {}),
    developer: { type: 'platform', name: 'developer', enabled: true },
    workspaceplan: { type: 'platform', name: 'workspaceplan', enabled: true },
    'esi-development-visualizer': { type: 'builtin', name: 'esi-development-visualizer', enabled: true },
  } : trust ? { todo: { type: 'platform', name: 'todo', enabled: true, description: '' },
    developer: { type: 'platform', name: 'developer', enabled: true, description: '' } } : {},
  ...(restricted ? { 'ESI_MODEL_PROFILE:["custom_profile_fixture","manual"]': {
    context_limit: null, max_tokens: null, thinking_protocol: 'reasoning_effort', thinking_effort: 'low',
    extended_sampling: false, preserve_thinking: false,
  } } : {}),
}));
await writeFile(join(profile, 'settings.json'), JSON.stringify({ language: 'en', disableAutoDownload: true, enableNotifications: false, showMenuBarIcon: false }));
const env = Object.fromEntries(['PATH', 'DISPLAY', 'XAUTHORITY', 'LANG'].filter(k => process.env[k]).map(k => [k, process.env[k]]));
Object.assign(env, { GOOSE_PATH_ROOT: gooseRoot, GOOSE_DISABLE_KEYRING: '1', GOOSE_ADDITIONAL_CONFIG_FILES: '',
  GOOSE_TELEMETRY_ENABLED: 'false', GOOSE_STATE_MACHINE: loop === 'state-machine' ? '1' : '0',
  XDG_CONFIG_HOME: join(root, 'xdg-config'), XDG_DATA_HOME: join(root, 'xdg-data'),
  XDG_STATE_HOME: join(root, 'xdg-state'), XDG_CACHE_HOME: join(root, 'xdg-cache'), TMPDIR: join(root, 'tmp'),
  ENABLE_PLAYWRIGHT: 'true', PLAYWRIGHT_DEBUG_PORT: '0', RUST_LOG: 'warn',
});
async function close() {
  if (browser) await Promise.race([browser.newBrowserCDPSession().then(s => s.send('Browser.close')).catch(() => {}), delay(1500)]);
  if (app) { try { process.kill(-app.pid, 'SIGTERM'); } catch (e) { if (e.code !== 'ESRCH') throw e; } }
  if (browser) await Promise.race([browser.close().catch(() => {}), delay(1500)]);
  browser = null; app = null;
}
async function launch() {
  await rm(join(profile, 'DevToolsActivePort'), { force: true });
  app = spawn(executable, ['--ozone-platform=x11', '--dir', workspace, `--user-data-dir=${profile}`], { env, cwd: workspace, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = '';
  for (const stream of [app.stdout, app.stderr]) stream.on('data', chunk => { log = (log + chunk).slice(-4000); });
  await until(async () => {
    if (app.exitCode != null) throw new Error(`Desktop exited: ${log}`);
    return readFile(join(profile, 'DevToolsActivePort'), 'utf8').then(Boolean).catch(() => false);
  }, 'Desktop startup');
  const port = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 15000 });
  let page;
  await until(() => { page = browser.contexts().flatMap(c => c.pages())[0]; return !!page; }, 'Desktop window');
  page.setDefaultTimeout(20000);
  await page.waitForFunction(() => document.querySelector('#root')?.children.length > 0);
  await page.evaluate(async () => {
    const socket = new WebSocket(await window.electron.getAcpUrl());
    await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
    let id = 0; const pending = new Map();
    socket.addEventListener('message', ({ data }) => {
      const msg = JSON.parse(data), job = pending.get(msg.id);
      if (job) { pending.delete(msg.id); clearTimeout(job.timer); msg.error ? job.reject(new Error(JSON.stringify(msg.error))) : job.resolve(msg.result); }
    });
    window.__profileRpc = (method, params) => new Promise((resolve, reject) => {
      const requestId = ++id;
      const timer = setTimeout(() => { pending.delete(requestId); reject(new Error(`ACP timeout: ${method}`)); }, 20000);
      pending.set(requestId, { resolve, reject, timer }); socket.send(JSON.stringify({ jsonrpc: '2.0', id: requestId, method, params }));
    });
    await window.__profileRpc('initialize', { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: 'post141-fixture', version: '1' } });
  });
  return page;
}
const rpc = (page, method, params) => page.evaluate(({ method, params }) => window.__profileRpc(method, params), { method, params });
const target = { provider: 'custom_profile_fixture', model: 'manual' };
const lastCapture = text => captures.findLast(body => JSON.stringify(body.messages.at(-1)).includes(text));
async function send(page, text) {
  await page.getByTestId('chat-input').fill(text);
  await page.getByTestId('chat-input').press('Enter');
  await until(() => !!lastCapture(text), `request ${text}`);
  await until(() => page.getByRole('combobox', { name: 'Thinking', exact: true }).isEnabled().catch(() => false), 'response complete');
  return lastCapture(text);
}
try {
  let page = await launch();
  if (controller) {
    const { acceptController } = await import('./controller-acceptance.mjs');
    await acceptController({ page, rpc, workspace, root, pass, until, captures });
  } else if (planning) {
    const { acceptPlanning } = await import('./planning-acceptance.mjs');
    await acceptPlanning({ page, rpc, workspace, root, pass, until });
  } else if (restricted) {
    await page.getByRole('button', { name: 'Model settings', exact: true }).click();
    await page.getByText(/Effective context: 262,144/).waitFor();
    assert.equal(await page.getByLabel('Context limit (tokens)', { exact: true }).inputValue(), '');
    await page.keyboard.press('Escape');
    await send(page, 'POST148 restricted provider context');
    const sessionId = new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('resumeSessionId');
    assert(sessionId);
    const state = await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId });
    assert.equal(state.contextLimit, 262144); assert.equal(state.serverContextLimit, 262144);
    assert.equal(state.profile.context_limit, null);
    pass('manual sampling profile with context Auto detects visible budgets despite model/info 403');
    await rpc(page, '_goose/esi/model-profile/save', { ...target, profile: null });
    await rpc(page, '_goose/esi/model-profile/apply', { ...target, sessionId });
    const automatic = await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId });
    assert.equal(automatic.contextLimit, 262144);
    assert.equal(automatic.sessionProfile.context_limit, 262144);
    assert.equal(automatic.sessionProfile.max_tokens, 32768);
    pass('fully automatic profile snapshots visible context and output budgets without privileged metadata');
  } else if (trust) {
    assert.equal((await rpc(page, '_goose/esi/extension-trust', { configKey: 'todo' })).trusted, false);
    await until(async () => {
      if (await page.locator('#extension-todo').count()) return true;
      await page.getByText('Extensions', { exact: true }).first().click({ timeout: 2000 });
      return false;
    }, 'extension settings');
    const toggle = page.getByRole('switch', { name: 'Trust todo', exact: true });
    await toggle.click();
    await until(async () => (await rpc(page, '_goose/esi/extension-trust', { configKey: 'todo' })).trusted, 'trust saved');
    pass('new extension starts untrusted and Settings Trust grants execution');
    const session = await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] });
    await rpc(page, 'session/prompt', { sessionId: session.sessionId, prompt: [{ type: 'text', text: 'POST147 update todo' }] });
    assert(captures.some(body => body.messages.some(message => message.role === 'tool' && JSON.stringify(message.content).includes('Updated'))));
    pass('real agent loop executes trusted extension without a fabricated workspace receipt');
    await page.getByRole('switch', { name: 'Trust developer', exact: true }).click();
    await until(async () => (await rpc(page, '_goose/esi/extension-trust', { configKey: 'developer' })).trusted, 'developer trust saved');
    await rpc(page, 'session/prompt', { sessionId: session.sessionId, prompt: [{ type: 'text', text: 'POST147 shell read operator fixture' }] });
    assert(captures.some(body => body.messages.some(message => message.role === 'tool' && JSON.stringify(message.content).includes('POST147_OUTSIDE_WORKSPACE'))));
    pass('trusted unprefixed developer shell works outside the workspace without a plan gate');
    await toggle.click();
    await until(async () => !(await rpc(page, '_goose/esi/extension-trust', { configKey: 'todo' })).trusted, 'trust revoked');
    await assert.rejects(rpc(page, '_goose/unstable/tools/call', { sessionId: session.sessionId,
      name: 'todo__todo_write', arguments: { content: 'must not execute' } }), /ESI authority/);
    pass('revocation rejects execution in an already-connected session');
    await close();
    page = await launch();
    assert.equal((await rpc(page, '_goose/esi/extension-trust', { configKey: 'todo' })).trusted, false);
    pass('revoked Trust persists across Desktop restart');
  } else if (discovery) {
    await page.getByRole('combobox', { name: 'Thinking', exact: true }).selectOption('max');
    await page.getByRole('button', { name: 'Model settings', exact: true }).click();
    await page.getByText(/Effective context: 262,144/).waitFor();
    assert.equal(await page.getByLabel('Maximum output (tokens)', { exact: true }).inputValue(), '32768');
    assert.equal(await page.getByLabel('Temperature', { exact: true }).inputValue(), '0.65');
    await page.keyboard.press('Escape');
    const first = await send(page, 'POST145 automatic');
    assert.equal(first.reasoning_effort, 'xhigh');
    assert.equal(first.max_tokens, 32768); assert.equal(first.top_k, 20);
    assert.equal(first.top_p, 0.95); assert.equal(first.min_p, 0);
    assert.equal(first.repetition_penalty, 1);
    assert.equal(first.chat_template_kwargs.preserve_thinking, true);
    assert(Math.abs(first.temperature - 0.65) < 1e-6);
    const sessionId = new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('resumeSessionId');
    assert(sessionId);
    const snapshot = await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId });
    assert.equal(snapshot.contextLimit, 262144); assert.equal(snapshot.profile, null);
    assert.equal(snapshot.sessionProfile.context_limit, 262144);
    pass('automatic provider context and generation settings reach the real session and wire without a saved profile');
    await page.getByRole('combobox', { name: 'Thinking', exact: true }).selectOption('off');
    await until(async () => (await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId })).thinkingEffort === 'off', 'automatic thinking off');
    const off = await send(page, 'POST145 off');
    assert.equal(off.reasoning_effort, 'none'); assert.equal(off.top_k, 20);
    assert.equal(off.chat_template_kwargs.preserve_thinking, true);
    await assert.rejects(rpc(page, '_goose/esi/model-profile/thinking', { ...target, sessionId, effort: 'medium' }));
    pass('advertised choices enforce exact thinking wire while preserving independent sampling');
    await page.getByRole('button', { name: 'Model settings', exact: true }).click();
    await page.getByLabel('Temperature', { exact: true }).fill('0.4');
    await page.getByLabel('Context limit (tokens)', { exact: true }).fill('');
    await page.getByRole('button', { name: 'Save and apply to this chat', exact: true }).click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const manual = await send(page, 'POST145 override');
    assert(Math.abs(manual.temperature - 0.4) < 1e-6);
    const manualState = await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId });
    assert.equal(manualState.profile.context_limit, null);
    assert.equal(manualState.contextLimit, 262144);
    await page.getByRole('button', { name: 'Model settings', exact: true }).click();
    await page.getByRole('button', { name: 'Reset profile', exact: true }).click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const reset = await send(page, 'POST145 reset');
    assert(Math.abs(reset.temperature - 0.65) < 1e-6);
    assert.equal((await rpc(page, '_goose/esi/model-profile/read', target)).profile, null);
    pass('manual override and reset restore provider defaults');
  } else {
  await page.getByRole('button', { name: 'Model settings', exact: true }).click();
  await page.getByText(/Effective context: 32,768/).waitFor();
  await page.getByLabel('Context limit (tokens)', { exact: true }).fill('');
  await page.getByLabel('Maximum output (tokens)', { exact: true }).fill('512');
  await page.getByLabel('Temperature', { exact: true }).fill('0.7');
  await page.getByLabel('Top-p', { exact: true }).fill('0.8');
  await page.getByLabel('Thinking API supported by this model/server', { exact: true }).selectOption('unsloth');
  await page.getByLabel('Default thinking', { exact: true }).selectOption('low');
  await page.getByLabel('My server supports top-k and min-p', { exact: true }).check();
  await page.getByLabel('Top-k (-1 or 0 disables, depending on server)', { exact: true }).fill('20');
  await page.getByLabel('Min-p', { exact: true }).fill('0');
  await page.getByLabel('Preserve returned thinking in conversation context', { exact: true }).check();
  await page.getByRole('button', { name: 'Save profile', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  await page.getByRole('combobox', { name: 'Thinking', exact: true }).selectOption('off');
  const first = await send(page, 'POST141 first');
  assert.equal(first.enable_thinking, false); assert.equal(first.top_k, 20); assert.equal(first.min_p, 0);
  assert.equal(first.max_tokens, 512); assert(Math.abs(first.temperature - 0.7) < 1e-6);
  assert.equal(first.enable_tools, false); assert.equal(first.preserve_thinking, true);
  assert(!('esi_model_profile' in first));
  pass('visible model editor and first-message chat-only thinking reach the wire');
  const sessionId = new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('resumeSessionId');
  assert(sessionId, 'UI navigated to its real chat');
  const state = await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId });
  assert.equal(state.contextLimit, 32768); assert.equal(state.contextSource, 'server');
  assert.equal(state.profile.thinking_effort, 'low'); assert.equal(state.thinkingEffort, 'off');
  await page.getByText(/^\d+ \/ 33k$/).waitFor();
  pass('runtime context uses server allocation, with independent saved and chat effort');
  await page.getByRole('combobox', { name: 'Thinking', exact: true }).selectOption('high');
  await until(async () => (await rpc(page, '_goose/esi/model-profile/read', { ...target, sessionId })).thinkingEffort === 'high', 'thinking applied');
  const second = await send(page, 'POST141 second');
  assert.equal(second.enable_thinking, true); assert.equal(second.reasoning_effort, 'high');
  const third = await send(page, 'POST141 third');
  assert(third.messages.some(m => m.reasoning_content === 'fixture reasoning'));
  pass('chat thinking and preserved returned reasoning work across turns');
  await page.getByRole('button', { name: 'Model settings', exact: true }).click();
  await page.getByLabel('Preserve returned thinking in conversation context', { exact: true }).uncheck();
  await page.getByRole('button', { name: 'Save and apply to this chat', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  const fourth = await send(page, 'POST141 fourth');
  assert.equal(fourth.preserve_thinking, false);
  assert(!fourth.messages.some(m => m.reasoning_content));
  pass('preservation can be disabled independently of thinking');
  await page.getByTestId('chat-input').fill('POST141 busy');
  await page.getByTestId('chat-input').press('Enter');
  await until(() => !!lastCapture('POST141 busy'), 'active response');
  assert(await page.getByRole('combobox', { name: 'Thinking', exact: true }).isDisabled());
  await assert.rejects(rpc(page, '_goose/esi/model-profile/thinking', { ...target, sessionId, effort: 'off' }));
  await delay(1800);
  pass('UI and backend reject mutation during an active response');
  await assert.rejects(rpc(page, '_goose/esi/model-profile/save', { ...target, profile: { temperature: -1 } }));
  const unchanged = await rpc(page, '_goose/esi/model-profile/read', target);
  assert.equal(unchanged.profile.thinking_effort, 'low');
  await close();
  page = await launch();
  const restarted = await rpc(page, '_goose/esi/model-profile/read', target);
  assert.deepEqual(restarted.profile, unchanged.profile);
  pass('invalid writes are rejected and model profiles survive a Desktop restart');
  }
  success = true;
} catch (error) {
  console.error(String(error));
  if (controller) console.error(JSON.stringify(captures.slice(-2).map(body => body.messages.filter(m => m.role === 'tool').map(m => JSON.stringify(m).slice(0, 1600)))));
  process.exitCode = 1;
  if (browser) { const page = browser.contexts().flatMap(c => c.pages())[0]; if (page) await page.screenshot({ path: `${reportPath}.png` }).catch(() => {}); }
} finally {
  await close();
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  await writeFile(reportPath, JSON.stringify({ success, loop, mode, checks, capturedRequests: captures.length, root }, null, 2));
  if (success) await rm(root, { recursive: true, force: true });
}
