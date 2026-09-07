import assert from 'node:assert/strict';

export function assertWikiFixtureOwnership(kind, name, labels, runId) {
  assert.match(runId, /^[a-f0-9]{8}$/);
  const prefixes = { container: ['wiki-http', 'wiki-db'], network: ['wiki-net'], image: ['wiki-pg'] };
  assert(prefixes[kind]?.some((prefix) => name === `forgeloop-ai-${prefix}-${runId}`));
  assert.equal(labels?.['forgeloop.managed'], 'true');
  assert.equal(labels?.['forgeloop.project'], 'forgeloop-ai');
  assert.equal(labels?.['forgeloop.task_id'], 'POST-122');
  assert.equal(labels?.['forgeloop.run_id'], runId);
}
