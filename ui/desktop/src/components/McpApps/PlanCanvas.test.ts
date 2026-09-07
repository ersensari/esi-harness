import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { expect, it, vi } from 'vitest';

it('renders actual bundled Canvas scope safely with accessible read-only details', async () => {
  const html = readFileSync(
    resolve(process.cwd(), '../../crates/esi-development-visualizer/src/app.html'),
    'utf8'
  );
  const parsed = new DOMParser().parseFromString(html, 'text/html');
  document.body.innerHTML = parsed.body.innerHTML;
  const ctx = new Proxy({}, { get: () => vi.fn(), set: () => true });
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
    ctx as CanvasRenderingContext2D
  );
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  const post = vi.spyOn(window.parent, 'postMessage').mockImplementation(() => {});
  new Function(parsed.querySelector('script')!.textContent!)();
  const plan = {
    exists: true,
    status: 'revising',
    title: '<img src=x onerror=alert(1)>',
    content_hash: 'current-hash',
    storage_revision: 9,
    architecture_notes: 'Local adapter',
    tasks: [{ id: 'T1', title: 'Implement', status: 'pending' }],
    requirements: [],
    task_execution_order: ['T1'],
    task_contracts: {
      T1: {
        depends_on: [],
        affected_components: ['ui'],
        acceptance_criteria: [{ id: 'AC1', description: 'Visible result' }],
      },
    },
    revision_diff: {
      baseline: 'available',
      approved_hash: 'old-hash',
      affected_task_ids: ['T1'],
      changes: [{ path: '/title', before: 'Old', after: 'New' }],
    },
    approval_history: [{ approved_by: 'User', approved_at: 'Today' }],
  };
  const send = () =>
    window.dispatchEvent(
      new MessageEvent('message', {
        source: window,
        data: {
          jsonrpc: '2.0',
          method: 'ui/notifications/tool-result',
          params: {
            structuredContent: {
              status: 'blocked',
              workspace_plan: plan,
              validation_evidence: [],
              fingerprints: [],
              repair_budgets: [],
              approvals: [],
              events: [],
            },
          },
        },
      })
    );
  send();
  await Promise.resolve();
  const panel = document.getElementById('plan-details')!;
  expect(panel.textContent).toContain('current-hash');
  expect(panel.textContent).toContain('Storage revision: 9');
  expect(panel.textContent).toContain('AC1: Visible result');
  expect(panel.textContent).toContain('/title');
  expect(panel.textContent).toContain('Affected tasks: T1');
  expect(panel.querySelector('summary')).not.toBeNull();
  expect(document.querySelector('img')).toBeNull();
  expect(
    post.mock.calls.every(
      (call) => !String((call[0] as { method?: string }).method).includes('plan-review')
    )
  ).toBe(true);
  plan.revision_diff.baseline = 'unavailable';
  send();
  await Promise.resolve();
  expect(panel.textContent).toContain('empty diff does not mean unchanged scope');
  expect(panel.textContent).not.toContain('No semantic changes');
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
