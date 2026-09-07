import assert from 'node:assert/strict';
import test from 'node:test';
import { assertWikiFixtureOwnership } from './wiki-release-ownership.mjs';

const labels = {
  'forgeloop.managed': 'true', 'forgeloop.project': 'forgeloop-ai',
  'forgeloop.task_id': 'POST-122', 'forgeloop.run_id': '1234abcd',
};
test('accepts only the four exact related fixture resource names', () => {
  for (const [kind, prefix] of [['container', 'wiki-http'], ['container', 'wiki-db'], ['network', 'wiki-net'], ['image', 'wiki-pg']]) {
    assert.doesNotThrow(() => assertWikiFixtureOwnership(kind, `forgeloop-ai-${prefix}-1234abcd`, labels, '1234abcd'));
  }
});
for (const key of Object.keys(labels)) {
  test(`rejects foreign or absent ownership field ${key}`, () => {
    for (const value of ['foreign', undefined]) {
      assert.throws(() => assertWikiFixtureOwnership('container', 'forgeloop-ai-wiki-db-1234abcd', { ...labels, [key]: value }, '1234abcd'));
    }
  });
}
test('a prefixed name without labels never grants cleanup authority', () => {
  assert.throws(() => assertWikiFixtureOwnership('container', 'forgeloop-ai-wiki-db-1234abcd', null, '1234abcd'));
});
test('rejects incorrect relationships and identifier injection', () => {
  assert.throws(() => assertWikiFixtureOwnership('container', 'forgeloop-ai-wiki-db-other', labels, '1234abcd'));
  assert.throws(() => assertWikiFixtureOwnership('volume', 'forgeloop-ai-wiki-db-1234abcd', labels, '1234abcd'));
  assert.throws(() => assertWikiFixtureOwnership('container', 'forgeloop-ai-wiki-db-1234abcd', labels, '../1234abcd'));
});
