// Actual packaged Desktop/provider/tool loop. Only disposable fixture paths.
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const run = promisify(execFile);

export function controllerFixtureTools(workspace) {
  const op = (name, request_id) => ({ name: `controller__${name}`, arguments: { task_id: 'T1', request_id } });
  return {
    show: { name: 'esi-development-visualizer__show_development_loop', arguments: { workspace_path: workspace } },
    start: { ...op('start', 'start'), arguments: { task_id: 'T1', request_id: 'start', validators: [{ id: 'test', category: 'targeted_tests', program: '/bin/sh', arguments: ['-c', 'test "$(cat result.txt)" = fixed'], required: true }] } },
    fail: op('validate', 'v1'), resume: op('resume', 'resume1'),
    write: { name: 'write', arguments: { path: 'result.txt', content: 'fixed\n' } },
    validate: op('validate', 'v2'), review: op('review', 'review1'),
    stale: op('complete', 'complete-stale'), reconcile: op('resume', 'resume2'),
    revalidate: op('validate', 'v3'), rereview: op('review', 'review2'), finish: op('complete', 'complete'),
  };
}

export async function acceptController({ page, rpc, workspace, pass, until, captures }) {
  for (const args of [['init', '-b', 'main'], ['config', 'user.name', 'Controller Fixture'], ['config', 'user.email', 'fixture@example.invalid']]) await run('/usr/bin/git', ['-C', workspace, ...args]);
  await writeFile(join(workspace, 'result.txt'), 'broken\n');
  await run('/usr/bin/git', ['-C', workspace, 'add', 'result.txt']);
  await run('/usr/bin/git', ['-C', workspace, 'commit', '-m', 'fixture']);
  const call = async (sessionId, name, args = {}) => {
    const result = await rpc(page, '_goose/unstable/tools/call', { sessionId, name, arguments: args });
    assert(!result.isError, JSON.stringify(result).slice(0, 1200)); return result;
  };
  const first = (await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] })).sessionId;
  await call(first, 'workspaceplan__save_draft', {
    title: 'Controller delivery fixture', description: 'Repair result', architecture_notes: 'Owned local worktree',
    requirements: [{ id: 'R1', description: 'Fix result', acceptance_criteria: ['result equals fixed'], priority: 'must' }],
    tasks: [{ id: 'T1', title: 'Repair result', description: 'Fix result.txt' }],
    task_contracts: { T1: { depends_on: [], affected_components: ['result.txt'], acceptance_criteria: [{ id: 'AC1', description: 'result equals fixed', requirement_id: 'R1' }], validation_expectations: [{ id: 'test', description: 'Test result', criterion_ids: ['AC1'] }] } },
  });
  await rpc(page, '_goose/esi/extension-trust', { configKey: 'esi-development-visualizer', trusted: true });
  const send = async key => { await until(async () => (await page.getByRole('button', { name: 'Stop', exact: true }).count()) === 0, 'previous controller turn complete'); await page.getByTestId('chat-input').fill(`M18005 ${key}`); await page.getByTestId('chat-input').press('Enter'); };
  await send('show');
  const reviewPlan = page.getByRole('button', { name: 'Review this plan revision', exact: true }).last();
  await reviewPlan.waitFor(); await reviewPlan.click();
  await page.getByRole('checkbox', { name: 'Approve this plan' }).check();
  await page.getByRole('button', { name: 'Submit', exact: true }).last().click();
  const planStatus = async () => JSON.parse((await call(first, 'workspaceplan__status')).content.find(c => c.type === 'text').text);
  await until(async () => (await planStatus()).implementation_allowed, 'native source approval');
  pass('native plan approval binds the actual packaged controller fixture');
  const sessionId = new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('resumeSessionId');
  assert(sessionId);
  const status = async () => JSON.parse((await call(sessionId, 'controller__status', { task_id: 'T1' })).content.find(c => c.type === 'text').text);
  const stage = async expected => until(async () => { try { return (await status()).stage === expected; } catch { return false; } }, `controller stage ${expected}`, 45000);
  const approve = async label => { await page.getByRole('checkbox', { name: label }).last().check(); await page.getByRole('button', { name: 'Submit', exact: true }).last().click(); };
  await send('start'); await approve('Approve this task execution'); await stage('implement');
  let state = await status(); const owned = state.worktree.identity.worktree_path;
  assert.notEqual(owned, workspace);
  await assert.rejects(call(first, 'write', { path: 'wrong-session.txt', content: 'must not write' }));
  pass('real start approval creates one owned worktree; another chat cannot write');
  await send('fail'); await stage('diagnose');
  state = await status(); assert.equal(state.validation_runs.at(-1).passed, false);
  await send('resume'); await stage('repair');
  await send('write'); await until(async () => (await readFile(join(owned, 'result.txt'), 'utf8')) === 'fixed\n', 'managed repair write');
  assert.equal(await readFile(join(workspace, 'result.txt'), 'utf8'), 'broken\n');
  await send('validate'); await stage('review');
  pass('actual failed validator repairs in its worktree and passes without modifying main');
  await send('review'); await approve('Approve this exact delivery evidence'); await stage('completion_gate');
  await writeFile(join(owned, 'result.txt'), 'changed after review\n');
  await send('stale');
  await until(() => captures.some(body => body.messages.some(m => m.role === 'tool' && JSON.stringify(m).includes('snapshot') && JSON.stringify(m).includes('Controller:'))), 'stale completion rejection');
  assert.equal((await status()).stage, 'completion_gate');
  await send('reconcile'); await stage('repair');
  await send('write'); await until(async () => (await readFile(join(owned, 'result.txt'), 'utf8')) === 'fixed\n', 'second repair write');
  await send('revalidate'); await stage('review');
  await send('rereview'); await approve('Approve this exact delivery evidence'); await stage('completion_gate');
  await send('finish'); await approve('Approve this exact delivery evidence'); await stage('completed');
  state = await status(); assert.equal(state.validation_runs.length, 3);
  assert.equal(state.validation_runs.at(-1).passed, true);
  pass('changed delivery snapshot rejects completion, requires revalidation and new human review');
  const response = await call(sessionId, 'controller__status', { task_id: 'T1' });
  assert.equal(response.structuredContent.delivery.current, true);
  assert.equal(response.structuredContent.delivery.criteria[0].covered_by_current_required_tests, true);
  const hash = response.structuredContent.delivery.snapshot_id;
  await page.getByRole('button', { name: 'Refresh Canvas snapshot', exact: true }).last().click();
  await until(async () => {
    for (const frame of page.frames()) if ((await frame.locator('#delivery-evidence').count()) && (await frame.locator('#delivery-evidence').textContent()).includes(hash)) return true;
    return false;
  }, 'packaged controller Canvas exact delivery hash');
  assert.equal(await readFile(join(workspace, 'result.txt'), 'utf8'), 'broken\n');
  assert.equal((await run('/usr/bin/git', ['-C', workspace, 'log', '--oneline'])).stdout.trim().split('\n').length, 1);
  pass('bundled Canvas shows actual criterion/test coverage, repair history and snapshot; no merge/publication');
}
