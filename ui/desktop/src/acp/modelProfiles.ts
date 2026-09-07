import { getAcpClient } from './acpConnection';
import type { UserInput } from '../types/message';
import type { Session } from '../types/session';

export type ProfileThinking = 'off' | 'low' | 'medium' | 'high' | 'max';
export type ThinkingProtocol =
  'none' | 'enable_thinking' | 'chat_template' | 'reasoning_effort' | 'unsloth';
export interface ModelProfile {
  context_limit: number | null;
  max_tokens: number | null;
  temperature: number | null;
  top_p: number | null;
  top_k: number | null;
  min_p: number | null;
  presence_penalty: number | null;
  frequency_penalty: number | null;
  repetition_penalty?: number | null;
  thinking_levels?: Partial<Record<ProfileThinking, string>> | null;
  preserve_thinking_wire?: boolean;
  extended_sampling: boolean;
  thinking_protocol: ThinkingProtocol;
  thinking_effort: ProfileThinking | null;
  preserve_thinking: boolean;
}
export interface ModelProfileState {
  profile: ModelProfile | null;
  providerProfile?: ModelProfile | null;
  effectiveProfile?: ModelProfile | null;
  sessionProfile: ModelProfile | null;
  contextLimit: number;
  contextSource: 'manual' | 'global' | 'server' | 'catalog' | 'fallback' | 'session';
  serverContextLimit: number | null;
  contextWarning: boolean;
  thinkingEffort: ProfileThinking | null;
  thinkingProtocol: ThinkingProtocol;
}
export const emptyModelProfile: ModelProfile = {
  context_limit: null,
  max_tokens: null,
  temperature: null,
  top_p: null,
  top_k: null,
  min_p: null,
  presence_penalty: null,
  frequency_penalty: null,
  extended_sampling: false,
  thinking_protocol: 'none',
  thinking_effort: null,
  preserve_thinking: false,
};

async function request(action: string, params: Record<string, unknown>) {
  const client = await getAcpClient();
  return client.connection.agent.request(`_goose/esi/model-profile/${action}`, params);
}
export async function readModelProfile(
  provider: string,
  model: string,
  sessionId?: string | null
): Promise<ModelProfileState> {
  return (await request('read', { provider, model, sessionId })) as ModelProfileState;
}
export async function saveModelProfile(
  provider: string,
  model: string,
  profile: ModelProfile | null
) {
  await request('save', { provider, model, profile });
}
export async function applyModelProfile(provider: string, model: string, sessionId: string) {
  await request('apply', { provider, model, sessionId });
}
export async function setChatThinking(
  provider: string,
  model: string,
  sessionId: string,
  effort: ProfileThinking
) {
  await request('thinking', { provider, model, sessionId, effort });
}
export function thinkingChoices(
  protocol: ThinkingProtocol,
  levels?: ModelProfile['thinking_levels']
): ProfileThinking[] {
  if (protocol === 'none') return [];
  if (protocol === 'reasoning_effort' && levels) {
    return (['off', 'low', 'medium', 'high', 'max'] as const).filter((level) => levels[level]);
  }
  return protocol === 'chat_template' || protocol === 'enable_thinking'
    ? ['off', 'medium']
    : ['off', 'low', 'medium', 'high'];
}

export async function applyInitialChatThinking(session: Session, input?: UserInput) {
  const selection = input?.initialThinking;
  if (!selection) return;
  if (
    session.provider_name !== selection.provider ||
    session.model_config?.model_name !== selection.model
  ) {
    throw new Error(
      'The model changed before this chat started. Select its thinking setting again.'
    );
  }
  await setChatThinking(selection.provider, selection.model, session.id, selection.effort);
}
