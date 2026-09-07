import { expect, it, vi } from 'vitest';
import { getAcpClient } from '../acpConnection';
import { renewWikiSession } from '../wiki';

vi.mock('../acpConnection', () => ({ getAcpClient: vi.fn() }));

it('sends a private config request and never returns the response payload', async () => {
  const request = vi.fn().mockResolvedValue({ token: 'must-not-return' });
  vi.mocked(getAcpClient).mockResolvedValue({
    connection: { agent: { request } },
  } as unknown as Awaited<ReturnType<typeof getAcpClient>>);
  expect(await renewWikiSession('alice', 'test-only-secret')).toBeUndefined();
  expect(request).toHaveBeenCalledExactlyOnceWith('_goose/esi/wiki/session/renew', {
    handle: 'alice',
    password: 'test-only-secret',
  });
});
