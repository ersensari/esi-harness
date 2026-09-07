// Execute the real self-test recipe/tool loop using a deterministic, local
// provider fixture. This validates tool execution, not a model's intelligence.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
const studio = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const [binary, reportPath, phase = 'model-profiles'] = process.argv.slice(2);
const phaseTitles = {
  'model-profiles': 'ESI Custom Model Profiles',
  'provider-discovery': 'ESI Provider Model Discovery',
  'state-concurrency': 'ESI State Concurrency',
  'tool-authority': 'ESI Required Authority Gates',
  'extension-trust': 'Extension Trust regression',
};
assert(Object.hasOwn(phaseTitles, phase));
const phaseTitle = phaseTitles[phase];
assert(binary?.startsWith('/') && reportPath?.startsWith('/'));
const root = await mkdtemp(join(tmpdir(), 'forgeloop-ai-profile-selftest-'));
const artifacts = join(root, 'artifacts');
await mkdir(artifacts);
await mkdir(join(root, 'config/custom_providers'), { recursive: true });
const commands = phase === 'extension-trust' ? [
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --lib extension_trust -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --lib authority_ -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --lib workspace_plan -- --quiet',
] : phase === 'provider-discovery' ? [
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose-provider-types --test model_profiles -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose-providers --lib discovery -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --test model_profiles -- --quiet',
] : phase === 'tool-authority' ? [
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --test tool_inspection_manager_tests --test tool_inspection_permission_precedence -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --lib authority_ -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --lib workspace_plan -- --quiet',
] : phase === 'state-concurrency' ? [
  'node scripts/test-isolated.mjs cargo test --locked -p esi-workspace-plan -p esi-development -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked -p esi-development-visualizer -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --lib workspace_plan -- --quiet',
] : [
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose-provider-types --test model_profiles -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --test model_profiles -- --quiet',
  'node scripts/test-isolated.mjs cargo test --locked --release -p goose --test acp_custom_requests_test model_profiles_private -- --quiet',
];
let step = 0, failure, child;
const results = [];
const server = createServer(async (req, res) => {
  try {
    res.setHeader('content-type', 'application/json');
    if (req.url === '/v1/models') return res.end(JSON.stringify({ data: [{ id: 'self-test-fixture', meta: { n_ctx: 32768 } }] }));
    if (req.url === '/model/info') { res.writeHead(404); return res.end('{}'); }
    assert.equal(req.url, '/v1/chat/completions');
    let raw = ''; for await (const chunk of req) { raw += chunk; assert(raw.length < 2 * 1024 * 1024); }
    const body = JSON.parse(raw);
    assert(JSON.stringify(body.messages).includes(phaseTitle), 'The updated recipe phase must be rendered');
    if (step > 0) {
      const result = body.messages.findLast(m => m.role === 'tool');
      assert(result, `Expected real tool output for step ${step}`);
      const content = JSON.stringify(result.content);
      if (step <= commands.length) {
        assert(content.includes('test result: ok'), `Tests did not pass: ${content.slice(-1800)}`);
        assert(content.includes('Studio config digest unchanged; child exit 0'), 'Tool execution must protect configuration');
        results.push({ command: commands[step - 1], result: 'PASS' });
      } else assert(!/error|denied/i.test(content), 'Report write must succeed');
    }
    let message, finish_reason;
    if (step < 5) {
      const toolSuffix = step < 3 ? 'shell' : 'write';
      const tool = body.tools?.find(t => t.function.name === toolSuffix);
      assert(tool, `Missing fixture tool ${toolSuffix}: ${body.tools?.map(t => t.function.name)}`);
      const args = step < 3
        ? { command: `cd ${JSON.stringify(studio)} && ${commands[step]}`, timeout_secs: 300 }
        : { path: join(artifacts, step === 3 ? `${phase.replaceAll('-', '_')}.md` : 'detailed_report.md'),
            content: `# ${phaseTitle} self-test\n\nScripted provider, actual Goose tool loop.\n\n` + results.map(r => `PASS: ${r.command}`).join('\n') };
      message = { role: 'assistant', content: null, tool_calls: [{ id: `profile-test-${step}`, type: 'function', function: { name: tool.function.name, arguments: JSON.stringify(args) } }] };
      finish_reason = 'tool_calls';
    } else {
      message = { role: 'assistant', content: `GOOSE SELF-TEST SUMMARY\nPASS: 3 deterministic ${phase} test commands and 2 real report writes. Scripted local provider; no live model inference.` };
      finish_reason = 'stop';
    }
    step++;
    res.end(JSON.stringify({ id: `fixture-${step}`, object: 'chat.completion', created: 1, model: 'self-test-fixture',
      choices: [{ index: 0, message, finish_reason }], usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 } }));
  } catch (error) { failure = String(error); res.writeHead(500); res.end(JSON.stringify({ error: { message: failure } })); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
await writeFile(join(root, 'config/custom_providers/custom_selftest.json'), JSON.stringify({ name: 'custom_selftest', engine: 'openai',
  display_name: 'Self-test fixture', api_key_env: '', base_url: `http://127.0.0.1:${server.address().port}`,
  models: [{ name: 'self-test-fixture', context_limit: 32768 }], requires_auth: false, supports_streaming: false }));
await writeFile(join(root, 'config/config.yaml'), JSON.stringify({ GOOSE_PROVIDER: 'custom_selftest', GOOSE_MODEL: 'self-test-fixture',
  GOOSE_MODE: 'auto', GOOSE_DISABLE_KEYRING: true, GOOSE_TELEMETRY_ENABLED: false, extensions: {} }));
let output = '', success = false;
try {
  child = spawn(binary, ['run', '--recipe', join(studio, 'goose-self-test.yaml'), '--params', `test_phases=${phase}`,
    '--params', 'parallel_tests=false', '--params', `workspace_dir=${artifacts}`, '--params', 'cleanup_after=false', '--max-turns', '8', '--quiet'], {
    cwd: root, env: { ...process.env, GOOSE_PATH_ROOT: root, GOOSE_DISABLE_KEYRING: '1', GOOSE_ADDITIONAL_CONFIG_FILES: '',
      GOOSE_PROVIDER: 'custom_selftest', GOOSE_MODEL: 'self-test-fixture', GOOSE_MODE: 'auto', GOOSE_STATE_MACHINE: '0' },
    detached: true, stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { output = (output + chunk).slice(-16000); });
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }, 500000);
  const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('close', resolve); });
  clearTimeout(timer);
  assert.equal(code, 0, failure || output.slice(-3000));
  assert(!failure, failure);
  assert.equal(results.length, 3); assert.equal(step, 6);
  assert((await readFile(join(artifacts, 'detailed_report.md'), 'utf8')).includes(commands[2]));
  success = true;
  console.log('PASS updated goose-self-test recipe: 3 real test commands, 2 report writes, scripted local provider');
} catch (error) { console.error(String(error)); console.error(output.slice(-3000)); process.exitCode = 1; }
finally {
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
  await writeFile(reportPath, JSON.stringify({ success, phase, provider: 'scripted local fixture', results, failure: failure ?? null }, null, 2));
  await rm(root, { recursive: true, force: true });
}
