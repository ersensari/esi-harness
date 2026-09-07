import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { IntlTestWrapper } from '../../../i18n/test-utils';
import { ModelProfileControls } from './ModelProfileControls';
import {
  applyModelProfile,
  emptyModelProfile,
  readModelProfile,
  saveModelProfile,
  setChatThinking,
  type ModelProfileState,
} from '../../../acp/modelProfiles';
vi.mock('../../../acp/modelProfiles', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../acp/modelProfiles')>()),
  readModelProfile: vi.fn(),
  saveModelProfile: vi.fn(),
  applyModelProfile: vi.fn(),
  setChatThinking: vi.fn(),
}));
const profile = {
  ...emptyModelProfile,
  thinking_protocol: 'unsloth' as const,
  thinking_effort: 'low' as const,
};
const state: ModelProfileState = {
  profile,
  sessionProfile: profile,
  contextLimit: 32768,
  contextSource: 'server',
  serverContextLimit: 32768,
  contextWarning: false,
  thinkingEffort: 'low',
  thinkingProtocol: 'unsloth',
};
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(readModelProfile).mockResolvedValue(state);
  vi.mocked(saveModelProfile).mockResolvedValue();
  vi.mocked(applyModelProfile).mockResolvedValue();
  vi.mocked(setChatThinking).mockResolvedValue();
});
function setup(busy = false) {
  return render(
    <ModelProfileControls
      provider="custom_unsloth"
      model="manual"
      sessionId="chat-1"
      busy={busy}
    />,
    { wrapper: IntlTestWrapper }
  );
}

it('uses advertised defaults without saving, exposes only advertised thinking, and refreshes', async () => {
  const discovered = {
    ...profile,
    context_limit: 262144,
    temperature: 0.65,
    thinking_protocol: 'reasoning_effort' as const,
    thinking_effort: 'max' as const,
    thinking_levels: { off: 'none', max: 'xhigh' },
  };
  vi.mocked(readModelProfile).mockResolvedValue({
    ...state,
    profile: null,
    providerProfile: discovered,
    effectiveProfile: discovered,
    sessionProfile: discovered,
    thinkingProtocol: 'reasoning_effort',
    thinkingEffort: 'max',
  });
  setup();
  const selector = await screen.findByLabelText('Thinking');
  expect(selector).toHaveValue('max');
  expect(selector.querySelectorAll('option')).toHaveLength(2);
  fireEvent.click(screen.getByRole('button', { name: 'Model settings' }));
  expect(screen.getByLabelText('Context limit (tokens)')).toHaveValue(262144);
  expect(screen.getByLabelText('Temperature')).toHaveValue(0.65);
  expect(screen.getByText(/Provider defaults detected/)).toBeInTheDocument();
  expect(saveModelProfile).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Refresh provider settings' }));
  await waitFor(() => expect(readModelProfile).toHaveBeenCalledTimes(2));
  expect(applyModelProfile).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Reset profile' }));
  await waitFor(() =>
    expect(saveModelProfile).toHaveBeenCalledWith('custom_unsloth', 'manual', null)
  );
});
it('changes this chat thinking without saving global or profile defaults', async () => {
  setup();
  fireEvent.change(await screen.findByLabelText('Thinking'), { target: { value: 'off' } });
  await waitFor(() =>
    expect(setChatThinking).toHaveBeenCalledWith('custom_unsloth', 'manual', 'chat-1', 'off')
  );
  expect(saveModelProfile).not.toHaveBeenCalled();
});

it('queues a first-message choice without persisting defaults', async () => {
  const change = vi.fn();
  render(
    <ModelProfileControls
      provider="custom_unsloth"
      model="manual"
      onInitialThinkingChange={change}
    />,
    { wrapper: IntlTestWrapper }
  );
  fireEvent.change(await screen.findByRole('combobox', { name: 'Thinking' }), {
    target: { value: 'off' },
  });
  expect(change).toHaveBeenCalledWith('off');
  expect(saveModelProfile).not.toHaveBeenCalled();
  expect(setChatThinking).not.toHaveBeenCalled();
});
it('saves independent sampling, preservation and manual context, then applies to this chat', async () => {
  setup();
  await screen.findByLabelText('Thinking');
  fireEvent.click(screen.getByRole('button', { name: 'Model settings' }));
  expect(screen.getByText(/Effective context: 32,768/)).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText('Context limit (tokens)'), { target: { value: '16384' } });
  fireEvent.change(screen.getByLabelText('Temperature'), { target: { value: '0.7' } });
  fireEvent.click(screen.getByLabelText('My server supports top-k and min-p'));
  fireEvent.change(screen.getByLabelText(/Top-k \(/), { target: { value: '20' } });
  fireEvent.click(screen.getByLabelText('Preserve returned thinking in conversation context'));
  fireEvent.click(screen.getByRole('button', { name: 'Save and apply to this chat' }));
  await waitFor(() =>
    expect(saveModelProfile).toHaveBeenCalledWith(
      'custom_unsloth',
      'manual',
      expect.objectContaining({
        context_limit: 16384,
        temperature: 0.7,
        top_k: 20,
        preserve_thinking: true,
        thinking_effort: 'low',
      })
    )
  );
  expect(applyModelProfile).toHaveBeenCalledWith('custom_unsloth', 'manual', 'chat-1');
});
it('disables mutations while responding and labels fallback context honestly', async () => {
  vi.mocked(readModelProfile).mockResolvedValue({
    ...state,
    contextSource: 'fallback',
    serverContextLimit: null,
  });
  setup(true);
  expect(await screen.findByLabelText('Thinking')).toBeDisabled();
  fireEvent.click(screen.getByRole('button', { name: 'Model settings' }));
  expect(screen.getByText(/fallback estimate \(not detected\)/)).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Save and apply to this chat' })).toBeDisabled();
});
it('does not offer thinking for an unsupported model and shows a server limit conflict', async () => {
  vi.mocked(readModelProfile).mockResolvedValue({
    ...state,
    thinkingProtocol: 'none',
    contextWarning: true,
  });
  setup();
  fireEvent.click(screen.getByRole('button', { name: 'Model settings' }));
  await screen.findByText(/client limit exceeds the server-reported/);
  expect(screen.queryByRole('combobox', { name: 'Thinking' })).not.toBeInTheDocument();
});
it('retains the dialog and surfaces a failed save', async () => {
  vi.mocked(saveModelProfile).mockRejectedValue(new Error('Output must be smaller than context'));
  setup();
  await screen.findByLabelText('Thinking');
  fireEvent.click(screen.getByRole('button', { name: 'Model settings' }));
  fireEvent.click(screen.getByRole('button', { name: 'Save and apply to this chat' }));
  expect(await screen.findByRole('alert')).toHaveTextContent('Output must be smaller than context');
  expect(applyModelProfile).not.toHaveBeenCalled();
});
