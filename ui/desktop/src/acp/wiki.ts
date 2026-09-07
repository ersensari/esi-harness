import { getAcpClient } from './acpConnection';

/** Dedicated configuration request; credentials must never become chat/tool input. */
export async function renewWikiSession(handle: string, password: string): Promise<void> {
  const client = await getAcpClient();
  await client.connection.agent.request('_goose/esi/wiki/session/renew', { handle, password });
}
