// Bounded local Unsloth functional check. No server/model/real config mutations.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const [binary, reportPath] = process.argv.slice(2);
assert(binary?.startsWith('/') && reportPath?.startsWith('/'));
const base = 'http://127.0.0.1:8888';
async function loaded() {
  const response = await fetch(`${base}/v1/models`, { signal: AbortSignal.timeout(5000) });
  assert(response.ok, 'Local Unsloth model metadata must be accessible');
  const data = await response.json();
  const entries = data.data.filter(m => m.loaded === true);
  assert.equal(entries.length, 1, 'Require one explicitly loaded model; never auto-switch');
  return entries[0];
}
const before = await loaded();
const root = await mkdtemp(join(tmpdir(), 'forgeloop-ai-live-profile-'));
const configDir = join(root, 'config');
await mkdir(join(configDir, 'custom_providers'), { recursive: true });
const captures = [], checks = [];
let success = false;
const proxy = createServer(async (req, res) => {
  try {
    assert(['/v1/models', '/v1/chat/completions'].includes(req.url));
    let body = '';
    for await (const chunk of req) { body += chunk; assert(body.length < 1024 * 1024); }
    if (body) {
      const payload = JSON.parse(body);
      assert.equal(payload.model, before.id, 'Never request a different model');
      assert.equal(payload.enable_tools, false, 'Server tools must stay disabled');
      assert(payload.max_tokens > 0 && payload.max_tokens <= 512, 'Bound generation budget');
      captures.push(payload);
    }
    const response = await fetch(`${base}${req.url}`, { method: req.method,
      headers: { 'content-type': 'application/json' }, body: body || undefined, signal: AbortSignal.timeout(90000) });
    res.writeHead(response.status, { 'content-type': response.headers.get('content-type') || 'application/json' });
    res.end(Buffer.from(await response.arrayBuffer()));
  } catch (error) { res.writeHead(502); res.end(JSON.stringify({ error: { message: String(error) } })); }
});
await new Promise(resolve => proxy.listen(0, '127.0.0.1', resolve));
await writeFile(join(configDir, 'custom_providers/custom_live_fixture.json'), JSON.stringify({
  name: 'custom_live_fixture', engine: 'openai', display_name: 'Local smoke fixture', api_key_env: '',
  base_url: `http://127.0.0.1:${proxy.address().port}`, models: [{ name: before.id, context_limit: 128000 }],
  requires_auth: false, supports_streaming: false,
}));
const profileKey = `ESI_MODEL_PROFILE:${JSON.stringify(['custom_live_fixture', before.id])}`;
try {
  for (const effort of ['off', 'low']) {
    const profile = { temperature: 0.7, top_p: 0.8, top_k: 20, min_p: 0, extended_sampling: true,
      max_tokens: 512, thinking_protocol: 'unsloth', thinking_effort: effort, preserve_thinking: true };
    await writeFile(join(configDir, 'config.yaml'), JSON.stringify({ GOOSE_PROVIDER: 'custom_live_fixture', GOOSE_MODEL: before.id,
      GOOSE_DISABLE_KEYRING: true, GOOSE_TELEMETRY_ENABLED: false, GOOSE_MODE: 'chat', extensions: {}, [profileKey]: profile }));
    const child = spawn(binary, ['run', '--no-profile', '--max-turns', '1', '--quiet', '--text', `POST141 ${effort}: Reply exactly OK. Do not use tools.`], {
      cwd: root, env: { ...process.env, GOOSE_PATH_ROOT: root, GOOSE_DISABLE_KEYRING: '1', GOOSE_ADDITIONAL_CONFIG_FILES: '',
        GOOSE_PROVIDER: 'custom_live_fixture', GOOSE_MODEL: before.id, GOOSE_MODE: 'chat', GOOSE_TELEMETRY_ENABLED: 'false' },
      stdio: ['ignore', 'pipe', 'pipe'], detached: true,
    });
    let output = '';
    for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { output = (output + chunk).slice(-8000); });
    const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }, 100000);
    const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('close', resolve); });
    clearTimeout(timer);
    assert.equal(code, 0, `CLI ${effort} failed: ${output.slice(-1200)}`);
    assert(output.includes('OK'), `Missing final answer for ${effort}`);
    const sent = captures.findLast(p => JSON.stringify(p.messages).includes(`POST141 ${effort}`));
    assert(sent, 'The actual CLI sent a captured request');
    assert.equal(sent.enable_thinking, effort !== 'off');
    assert.equal(sent.reasoning_effort, effort === 'off' ? undefined : 'low');
    assert.equal(sent.preserve_thinking, true);
    checks.push({ effort, model: sent.model, enable_thinking: sent.enable_thinking,
      reasoning_effort: sent.reasoning_effort ?? null, max_tokens: sent.max_tokens, top_k: sent.top_k, final_answer: 'OK' });
    console.log(`PASS live Unsloth ${effort}, bounded 512-token output`);
  }
  const after = await loaded();
  assert.equal(after.id, before.id); assert.equal(after.context_length, before.context_length);
  success = true;
} catch (error) { console.error(String(error)); process.exitCode = 1; }
finally {
  proxy.closeAllConnections(); await new Promise(resolve => proxy.close(resolve));
  await writeFile(reportPath, JSON.stringify({ success, context: before.context_length, checks }, null, 2));
  await rm(root, { recursive: true, force: true });
}
