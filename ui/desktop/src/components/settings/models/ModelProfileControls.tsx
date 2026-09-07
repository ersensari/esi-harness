import { useEffect, useState } from 'react';
import { Settings2 } from 'lucide-react';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '../../ui/dialog';
import { Button } from '../../ui/button';
import { defineMessages, useIntl } from '../../../i18n';
import {
  applyModelProfile,
  emptyModelProfile,
  readModelProfile,
  saveModelProfile,
  setChatThinking,
  thinkingChoices,
  type ModelProfile,
  type ModelProfileState,
  type ProfileThinking,
  type ThinkingProtocol,
} from '../../../acp/modelProfiles';

const messages = defineMessages({
  session: { id: 'modelProfile.session', defaultMessage: 'saved chat setting' },
  settings: { id: 'modelProfile.settings', defaultMessage: 'Model settings' },
  thinking: { id: 'modelProfile.thinking', defaultMessage: 'Thinking' },
  off: { id: 'modelProfile.off', defaultMessage: 'Off' },
  on: { id: 'modelProfile.on', defaultMessage: 'On' },
  low: { id: 'modelProfile.low', defaultMessage: 'Low' },
  medium: { id: 'modelProfile.medium', defaultMessage: 'Medium' },
  high: { id: 'modelProfile.high', defaultMessage: 'High' },
  max: { id: 'modelProfile.max', defaultMessage: 'Maximum' },
  repetition_penalty: { id: 'modelProfile.repetition', defaultMessage: 'Repetition penalty' },
  detected: {
    id: 'modelProfile.detected',
    defaultMessage:
      'Provider defaults detected. Saving creates a manual profile; Reset restores automatic settings.',
  },
  undetected: {
    id: 'modelProfile.undetected',
    defaultMessage:
      'Provider settings unavailable or not advertised. Manual settings and existing defaults remain available.',
  },
  refresh: { id: 'modelProfile.refresh', defaultMessage: 'Refresh provider settings' },
  auto: { id: 'modelProfile.auto', defaultMessage: 'Server default' },
  context_limit: { id: 'modelProfile.context', defaultMessage: 'Context limit (tokens)' },
  max_tokens: { id: 'modelProfile.output', defaultMessage: 'Maximum output (tokens)' },
  temperature: { id: 'modelProfile.temperature', defaultMessage: 'Temperature' },
  top_p: { id: 'modelProfile.topP', defaultMessage: 'Top-p' },
  top_k: {
    id: 'modelProfile.topK',
    defaultMessage: 'Top-k (-1 or 0 disables, depending on server)',
  },
  min_p: { id: 'modelProfile.minP', defaultMessage: 'Min-p' },
  presence_penalty: { id: 'modelProfile.presence', defaultMessage: 'Presence penalty' },
  frequency_penalty: { id: 'modelProfile.frequency', defaultMessage: 'Frequency penalty' },
  extended: { id: 'modelProfile.extended', defaultMessage: 'My server supports top-k and min-p' },
  protocol: {
    id: 'modelProfile.protocol',
    defaultMessage: 'Thinking API supported by this model/server',
  },
  none: { id: 'modelProfile.none', defaultMessage: 'Not configured / unsupported' },
  preserve: {
    id: 'modelProfile.preserve',
    defaultMessage: 'Preserve returned thinking in conversation context',
  },
  preserveHelp: {
    id: 'modelProfile.preserveHelp',
    defaultMessage:
      'Replays reasoning returned by the server; requires server support. This does not enable thinking.',
  },
  scope: {
    id: 'modelProfile.scope',
    defaultMessage:
      'Saved only for this provider and exact model name. Empty fields use existing defaults. This does not resize server context or VRAM.',
  },
  contextInfo: {
    id: 'modelProfile.contextInfo',
    defaultMessage: 'Effective context: {limit} · source: {source}',
  },
  warning: {
    id: 'modelProfile.warning',
    defaultMessage:
      'The client limit exceeds the server-reported allocation ({limit}). Reduce the client limit or configure the server separately.',
  },
  manual: { id: 'modelProfile.manual', defaultMessage: 'model profile' },
  global: { id: 'modelProfile.global', defaultMessage: 'global setting' },
  server: { id: 'modelProfile.server', defaultMessage: 'server metadata' },
  catalog: { id: 'modelProfile.catalog', defaultMessage: 'model catalog' },
  fallback: { id: 'modelProfile.fallback', defaultMessage: 'fallback estimate (not detected)' },
  save: { id: 'modelProfile.save', defaultMessage: 'Save profile' },
  saveApply: { id: 'modelProfile.saveApply', defaultMessage: 'Save and apply to this chat' },
  reset: { id: 'modelProfile.reset', defaultMessage: 'Reset profile' },
  wait: {
    id: 'modelProfile.wait',
    defaultMessage: 'Settings apply between responses. Start a chat to use its thinking selector.',
  },
  apply: { id: 'modelProfile.apply', defaultMessage: 'Apply saved profile to this chat' },
});

const fields = [
  'context_limit',
  'max_tokens',
  'temperature',
  'top_p',
  'top_k',
  'min_p',
  'presence_penalty',
  'frequency_penalty',
  'repetition_penalty',
] as const;
type NumericField = (typeof fields)[number];
const limits: Record<NumericField, [number, number | undefined, number]> = {
  context_limit: [1, undefined, 1],
  max_tokens: [1, 2147483647, 1],
  temperature: [0, 2, 0.01],
  top_p: [0, 1, 0.01],
  top_k: [-1, undefined, 1],
  min_p: [0, 1, 0.01],
  presence_penalty: [-2, 2, 0.01],
  frequency_penalty: [-2, 2, 0.01],
  repetition_penalty: [0, 10, 0.01],
};

export function ModelProfileControls({
  provider,
  model,
  sessionId,
  busy = false,
  initialThinking,
  onInitialThinkingChange,
  onContextResolved,
}: {
  provider: string;
  model: string;
  sessionId?: string | null;
  busy?: boolean;
  initialThinking?: ProfileThinking;
  onInitialThinkingChange?: (effort: ProfileThinking | null) => void;
  onContextResolved?: (limit: number) => void;
}) {
  const intl = useIntl();
  const [state, setState] = useState<ModelProfileState | null>(null);
  const [draft, setDraft] = useState<ModelProfile>({ ...emptyModelProfile });
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setState(null);
    setError('');
    readModelProfile(provider, model, sessionId)
      .then((value) => {
        if (!cancelled) {
          setState(value);
          onContextResolved?.(value.contextLimit);
          setDraft({ ...emptyModelProfile, ...(value.effectiveProfile ?? value.profile) });
        }
      })
      .catch((error: unknown) => {
        if (!cancelled) setError(String(error));
      });
    return () => {
      cancelled = true;
    };
  }, [provider, model, sessionId, revision, onContextResolved]);

  const action = async (work: () => Promise<void>, close = false) => {
    setSaving(true);
    setError('');
    try {
      await work();
      if (close) setOpen(false);
      setRevision((value) => value + 1);
    } catch (error) {
      setError(String(error));
    } finally {
      setSaving(false);
    }
  };
  const effortLabel = (effort: ProfileThinking, protocol: ThinkingProtocol) =>
    intl.formatMessage(
      messages[effort === 'medium' && thinkingChoices(protocol).length === 2 ? 'on' : effort]
    );
  const protocol =
    !sessionId && onInitialThinkingChange
      ? ((state?.effectiveProfile ?? state?.profile)?.thinking_protocol ?? 'none')
      : (state?.thinkingProtocol ?? 'none');
  const activeProfile = sessionId
    ? state?.sessionProfile
    : (state?.effectiveProfile ?? state?.profile);
  const choices = thinkingChoices(protocol, activeProfile?.thinking_levels);
  const selectedEffort = sessionId
    ? state?.thinkingEffort
    : (initialThinking ?? activeProfile?.thinking_effort);
  const disabled = busy || saving || !state;

  return (
    <div className="flex items-center gap-2 text-xs">
      <button
        type="button"
        aria-label={intl.formatMessage(messages.settings)}
        title={intl.formatMessage(messages.settings)}
        onClick={() => setOpen(true)}
        className="text-text-secondary hover:text-text-primary"
      >
        <Settings2 className="h-4 w-4" />
      </button>
      {choices.length > 0 && (
        <label className="flex items-center gap-1" title={intl.formatMessage(messages.wait)}>
          {intl.formatMessage(messages.thinking)}
          <select
            aria-label={intl.formatMessage(messages.thinking)}
            value={selectedEffort ?? ''}
            disabled={disabled || (!sessionId && !onInitialThinkingChange)}
            className="bg-background-primary text-text-primary rounded border border-border-primary"
            onChange={(event) => {
              if (sessionId)
                void action(() =>
                  setChatThinking(provider, model, sessionId, event.target.value as ProfileThinking)
                );
              else
                onInitialThinkingChange?.(
                  event.target.value ? (event.target.value as ProfileThinking) : null
                );
            }}
          >
            {selectedEffort == null && (
              <option value="" disabled={!!sessionId}>
                {intl.formatMessage(messages.auto)}
              </option>
            )}
            {choices.map((value) => (
              <option key={value} value={value}>
                {effortLabel(value, protocol)}
              </option>
            ))}
          </select>
        </label>
      )}
      {error && !open && (
        <span role="alert" className="text-red-500 max-w-48 truncate" title={error}>
          {error}
        </span>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-w-xl max-h-[85vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>
              {intl.formatMessage(messages.settings)} — {model}
            </DialogTitle>
          </DialogHeader>
          <p className="text-xs text-text-secondary">
            {provider}. {intl.formatMessage(messages.scope)}
          </p>
          {state && (
            <p>
              {intl.formatMessage(messages.contextInfo, {
                limit: state.contextLimit.toLocaleString(),
                source: intl.formatMessage(messages[state.contextSource]),
              })}
            </p>
          )}
          {state?.contextWarning && (
            <p role="alert" className="text-amber-600">
              {intl.formatMessage(messages.warning, { limit: state.serverContextLimit ?? 0 })}
            </p>
          )}
          {state && (
            <p className="text-xs text-text-secondary">
              {intl.formatMessage(state.providerProfile ? messages.detected : messages.undetected)}
            </p>
          )}
          <Button
            type="button"
            variant="outline"
            disabled={disabled}
            onClick={() => setRevision((value) => value + 1)}
          >
            {intl.formatMessage(messages.refresh)}
          </Button>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void action(async () => {
                await saveModelProfile(provider, model, draft);
                if (sessionId) await applyModelProfile(provider, model, sessionId);
              }, true);
            }}
          >
            <fieldset disabled={disabled} className="space-y-4">
              <div className="grid grid-cols-2 gap-3">
                {fields
                  .filter(
                    (field) => draft.extended_sampling || (field !== 'top_k' && field !== 'min_p')
                  )
                  .map((field) => (
                    <label key={field} className="flex flex-col gap-1 text-sm">
                      {intl.formatMessage(messages[field])}
                      <input
                        type="number"
                        aria-label={intl.formatMessage(messages[field])}
                        value={draft[field] ?? ''}
                        min={limits[field][0]}
                        max={limits[field][1]}
                        step={limits[field][2] === 1 ? 1 : 'any'}
                        placeholder={intl.formatMessage(messages.auto)}
                        className="rounded border border-border-primary bg-background-primary p-2"
                        onChange={(event) =>
                          setDraft({
                            ...draft,
                            [field]: event.target.value === '' ? null : event.target.valueAsNumber,
                          })
                        }
                      />
                    </label>
                  ))}
              </div>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={draft.extended_sampling}
                  onChange={(event) =>
                    setDraft({
                      ...draft,
                      extended_sampling: event.target.checked,
                      ...(!event.target.checked ? { top_k: null, min_p: null } : {}),
                    })
                  }
                />
                {intl.formatMessage(messages.extended)}
              </label>
              <label className="flex flex-col gap-1 text-sm">
                {intl.formatMessage(messages.protocol)}
                <select
                  value={draft.thinking_protocol}
                  aria-label={intl.formatMessage(messages.protocol)}
                  className="rounded border border-border-primary bg-background-primary p-2"
                  onChange={(event) =>
                    setDraft({
                      ...draft,
                      thinking_protocol: event.target.value as ThinkingProtocol,
                      thinking_effort: null,
                      thinking_levels: null,
                      preserve_thinking_wire: false,
                    })
                  }
                >
                  <option value="none">{intl.formatMessage(messages.none)}</option>
                  <option value="unsloth">Unsloth (enable_thinking + reasoning_effort)</option>
                  <option value="chat_template">
                    llama.cpp (chat_template_kwargs.enable_thinking)
                  </option>
                  <option value="enable_thinking">enable_thinking</option>
                  <option value="reasoning_effort">
                    reasoning_effort (none / low / medium / high)
                  </option>
                </select>
              </label>
              {draft.thinking_protocol !== 'none' && (
                <label className="flex flex-col gap-1 text-sm">
                  {intl.formatMessage(messages.thinking)}
                  <select
                    aria-label="Default thinking"
                    value={draft.thinking_effort ?? ''}
                    className="rounded border border-border-primary bg-background-primary p-2"
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        thinking_effort: event.target.value
                          ? (event.target.value as ProfileThinking)
                          : null,
                      })
                    }
                  >
                    <option value="">{intl.formatMessage(messages.auto)}</option>
                    {thinkingChoices(draft.thinking_protocol, draft.thinking_levels).map(
                      (value) => (
                        <option key={value} value={value}>
                          {effortLabel(value, draft.thinking_protocol)}
                        </option>
                      )
                    )}
                  </select>
                </label>
              )}
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={draft.preserve_thinking}
                  onChange={(event) =>
                    setDraft({ ...draft, preserve_thinking: event.target.checked })
                  }
                />
                {intl.formatMessage(messages.preserve)}
              </label>
              <p className="text-xs text-text-secondary">
                {intl.formatMessage(messages.preserveHelp)}
              </p>
              <div className="flex flex-wrap gap-2">
                <Button type="submit">
                  {intl.formatMessage(sessionId ? messages.saveApply : messages.save)}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() =>
                    void action(async () => {
                      await saveModelProfile(provider, model, null);
                      if (sessionId) await applyModelProfile(provider, model, sessionId);
                    }, true)
                  }
                >
                  {intl.formatMessage(messages.reset)}
                </Button>
                {sessionId && (state?.effectiveProfile ?? state?.profile) && (
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() =>
                      void action(() => applyModelProfile(provider, model, sessionId), true)
                    }
                  >
                    {intl.formatMessage(messages.apply)}
                  </Button>
                )}
              </div>
            </fieldset>
          </form>
          {error && (
            <p role="alert" className="text-red-500">
              {error}
            </p>
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}
