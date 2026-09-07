import { beforeEach, expect, it, vi } from 'vitest';
import { getAcpClient } from '../acpConnection';
import {
  applyModelProfile,
  emptyModelProfile,
  readModelProfile,
  saveModelProfile,
  setChatThinking,
  thinkingChoices,
  applyInitialChatThinking,
} from '../modelProfiles';
import type { Session } from '../../types/session';
vi.mock('../acpConnection', () => ({ getAcpClient: vi.fn() }));
const request = vi.fn();
beforeEach(() => {
  request.mockReset().mockResolvedValue({});
  vi.mocked(getAcpClient).mockResolvedValue({
    connection: { agent: { request } },
  } as unknown as Awaited<ReturnType<typeof getAcpClient>>);
});
it('keeps profile persistence and chat-only thinking as separate private requests', async () => {
  await saveModelProfile('custom_u', 'model', emptyModelProfile);
  await setChatThinking('custom_u', 'model', 'chat-1', 'off');
  await applyModelProfile('custom_u', 'model', 'chat-1');
  expect(request.mock.calls).toEqual([
    [
      '_goose/esi/model-profile/save',
      { provider: 'custom_u', model: 'model', profile: emptyModelProfile },
    ],
    [
      '_goose/esi/model-profile/thinking',
      { provider: 'custom_u', model: 'model', sessionId: 'chat-1', effort: 'off' },
    ],
    [
      '_goose/esi/model-profile/apply',
      { provider: 'custom_u', model: 'model', sessionId: 'chat-1' },
    ],
  ]);
});
it('returns context provenance and exposes only supported choices', async () => {
  request.mockResolvedValue({ contextLimit: 32768, contextSource: 'server' });
  expect(await readModelProfile('custom_u', 'model')).toEqual({
    contextLimit: 32768,
    contextSource: 'server',
  });
  expect(thinkingChoices('none')).toEqual([]);
  expect(thinkingChoices('chat_template')).toEqual(['off', 'medium']);
  expect(thinkingChoices('unsloth')).toEqual(['off', 'low', 'medium', 'high']);
});

it('applies a first-message choice only to the matching new session', async () => {
  const session = {
    id: 'new-chat',
    provider_name: 'custom_u',
    model_config: { model_name: 'manual' },
  } as Session;
  const input = {
    msg: 'hello',
    images: [],
    initialThinking: { provider: 'custom_u', model: 'manual', effort: 'off' as const },
  };
  await applyInitialChatThinking(session, input);
  expect(request).toHaveBeenCalledExactlyOnceWith('_goose/esi/model-profile/thinking', {
    provider: 'custom_u',
    model: 'manual',
    sessionId: 'new-chat',
    effort: 'off',
  });
  request.mockClear();
  await expect(
    applyInitialChatThinking({ ...session, provider_name: 'another' }, input)
  ).rejects.toThrow('model changed');
  expect(request).not.toHaveBeenCalled();
});
