// Real packaged renderer/backend; all workspaces and plans are disposable.
import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { join } from 'node:path';

export async function acceptPlanning({ page, rpc, workspace, root, pass, until }) {
  const call = async (sessionId, name, args = {}) => {
    const result = await rpc(page, '_goose/unstable/tools/call', { sessionId, name, arguments: args });
    assert(!result.isError, JSON.stringify(result).slice(0, 1000));
    return result;
  };
  const status = async sessionId => JSON.parse((await call(sessionId, 'workspaceplan__status')).content.find(c => c.type === 'text').text);
  const readPlan = async () => JSON.parse(await readFile(join(workspace, '.esi/workspace-plan.json'), 'utf8'));
  await rpc(page, '_goose/esi/extension-trust', { configKey: 'esi-development-visualizer', trusted: true });
  const first = (await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] })).sessionId;
  const second = (await rpc(page, 'session/new', { cwd: workspace, mcpServers: [] })).sessionId;
  await call(first, 'workspaceplan__create_template', { template: 'small_change', title: 'Small UI fix', objective: 'Submit once and show the result' });
  let plan = await status(second);
  assert.equal(plan.tasks.length, 3); assert.equal(plan.implementation_allowed, false);
  assert.equal(plan.task_execution_order.length, 3);
  const bytes = await readFile(join(workspace, '.esi/workspace-plan.json'), 'utf8');
  await call(second, 'workspaceplan__create_template', { template: 'greenfield', title: 'Must not replace', objective: 'Ignored retry' });
  assert.equal(await readFile(join(workspace, '.esi/workspace-plan.json'), 'utf8'), bytes);
  pass('small-change template is shared across chats without replacing authored scope');
  const green = join(root, 'greenfield'); await mkdir(green);
  const fresh = (await rpc(page, 'session/new', { cwd: green, mcpServers: [] })).sessionId;
  await call(fresh, 'workspaceplan__create_template', { template: 'greenfield', title: 'New app', objective: 'Build a minimal local app' });
  assert.equal((await status(fresh)).tasks.length, 5);
  pass('greenfield has five ordered tasks without automatic approval');
  for (const sessionId of [first, second]) await assert.rejects(call(sessionId, 'shell', { command: 'touch must-not-exist' }));
  await assert.rejects(readFile(join(workspace, 'must-not-exist')), { code: 'ENOENT' });
  pass('untrusted controller-managed shell cannot write before approval in either chat');
  // The real Hub creates its own chat in the explicit --dir fixture workspace.
  // Do not mutate location/history behind React Router or use the host home dir.
  await page.getByTestId('chat-input').fill('M17005 show plan');
  await page.getByTestId('chat-input').press('Enter');
  const reviewButton = page.getByRole('button', { name: 'Review this plan revision', exact: true }).last();
  await reviewButton.waitFor();
  let frame;
  await until(async () => {
    for (const candidate of page.frames()) if (await candidate.locator('#plan-details').count()) { frame = candidate; return true; }
    return false;
  }, 'packaged Canvas frame');
  await until(async () => (await frame.locator('#plan-details').textContent()).includes(plan.revision_diff.current_hash), 'Canvas exact hash');
  assert((await frame.locator('#plan-details').textContent()).includes('Task execution order'));
  assert.equal(await frame.getByRole('button', { name: /approve/i }).count(), 0);
  pass('packaged read-only Canvas renders current hash, dependency order and criteria');
  await page.evaluate(() => {
    window.__planReviewWire = [];
    const send = WebSocket.prototype.send;
    WebSocket.prototype.send = function (raw) {
      try {
        const request = JSON.parse(raw);
        if (request.method === '_goose/esi/plan-review') {
          window.__planReviewWire.push({ request });
          this.addEventListener('message', function listener(event) {
            const response = JSON.parse(event.data);
            if (response.id !== request.id) return;
            window.__planReviewWire.push({ response }); this.removeEventListener('message', listener);
          });
        }
      } catch {}
      return send.call(this, raw);
    };
  });
  await reviewButton.focus(); await page.keyboard.press('Enter');
  await until(async () => (await page.getByRole('checkbox').count()) > 0 || (await page.getByText('Plan changed or review is unavailable.', { exact: false }).count()) > 0, 'native review result');
  if (!(await page.getByRole('checkbox').count())) throw new Error(`Native review failed: ${JSON.stringify(await page.evaluate(() => window.__planReviewWire))}`);
  await page.getByRole('checkbox', { name: 'Approve this plan' }).check();
  const submit = page.getByRole('button', { name: 'Submit', exact: true }).last();
  await submit.focus(); await page.keyboard.press('Enter');
  await until(async () => (await status(second)).implementation_allowed === true, 'native approval shared across chats');
  assert.equal((await readPlan()).approval.content_hash, plan.revision_diff.current_hash);
  for (const sessionId of [first, second]) await call(sessionId, 'shell', { command: 'printf approved' });
  pass('native keyboard confirmation approves exactly the displayed scope for both chats');
  const prior = await readPlan();
  const staleReview = await rpc(page, '_goose/esi/plan-review', { action: 'prepare', session_id: first, hash: prior.approval.content_hash, storage_revision: prior.storage_revision });
  await call(second, 'workspaceplan__save_draft', {
    title: prior.title, description: prior.description, architecture_notes: prior.architecture_notes,
    requirements: prior.requirements,
    tasks: prior.tasks.map((task, index) => ({ id: task.id, title: index ? task.title : 'Revised first task', description: task.description })),
    task_contracts: prior.task_contracts,
  });
  for (const sessionId of [first, second]) await assert.rejects(call(sessionId, 'shell', { command: 'touch must-not-exist' }));
  await assert.rejects(readFile(join(workspace, 'must-not-exist')), { code: 'ENOENT' });
  await assert.rejects(rpc(page, '_goose/esi/plan-review', { action: 'complete', session_id: first, token: staleReview.token, approve: true }));
  pass('second-chat revision revokes managed writes in both chats and rejects stale native review');
  await page.getByRole('button', { name: 'Refresh Canvas snapshot', exact: true }).last().click();
  plan = await status(first);
  await until(async () => (await frame.locator('#plan-details').textContent()).includes(plan.revision_diff.current_hash), 'refreshed Canvas hash');
  assert((await frame.locator('#plan-details').textContent()).includes('/tasks/'));
  assert(plan.revision_diff.affected_task_ids.length >= 1);
  const positions = new Map(plan.task_execution_order.map((id, index) => [id, index]));
  for (const [id, contract] of Object.entries(plan.task_contracts)) for (const dependency of contract.depends_on) assert(positions.get(dependency) < positions.get(id));
  await reviewButton.click();
  await page.getByRole('checkbox', { name: 'Approve this plan' }).check();
  await page.getByRole('button', { name: 'Submit', exact: true }).last().click();
  await until(async () => (await status(second)).implementation_allowed === true, 'revised approval');
  const revised = await readPlan();
  assert.equal(revised.approval_history.length, 2);
  assert.equal(revised.approval.content_hash, plan.revision_diff.current_hash);
  assert.equal(revised.approval_history[0].approval.content_hash, prior.approval.content_hash);
  pass('Canvas refresh exposes revision diff; reapproval preserves history and dependency order');
}
