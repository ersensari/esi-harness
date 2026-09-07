import { useState } from 'react';
import { renewWikiSession } from '../../../../acp/wiki';
import { defineMessages, useIntl } from '../../../../i18n';
import { Button } from '../../../ui/button';

const messages = defineMessages({
  title: { id: 'wikiSession.title', defaultMessage: 'Wiki session' },
  note: {
    id: 'wikiSession.note',
    defaultMessage:
      'Sign in to the saved, enabled Wiki endpoint. Save endpoint changes first. Credentials are not sent to chat.',
  },
  handle: { id: 'wikiSession.handle', defaultMessage: 'Wiki handle' },
  password: { id: 'wikiSession.password', defaultMessage: 'Wiki password' },
  renew: { id: 'wikiSession.renew', defaultMessage: 'Renew Wiki session' },
  busy: { id: 'wikiSession.busy', defaultMessage: 'Signing in…' },
  success: {
    id: 'wikiSession.success',
    defaultMessage:
      'Wiki authorization updated. Retry pending plan capture. Existing Wiki tool connections may need reconnecting.',
  },
  failure: {
    id: 'wikiSession.failure',
    defaultMessage:
      'Wiki sign-in failed. Check your credentials and the saved, enabled endpoint. HTTPS or local loopback HTTP is required.',
  },
});

export default function WikiSessionForm() {
  const intl = useIntl();
  const [handle, setHandle] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<'success' | 'failure' | null>(null);

  const renew = async () => {
    if (busy || !handle.trim() || !password) return;
    setBusy(true);
    setResult(null);
    const submittedPassword = password;
    setPassword('');
    try {
      await renewWikiSession(handle.trim(), submittedPassword);
      setResult('success');
    } catch {
      // Never surface arbitrary remote errors: they can echo credentials.
      setResult('failure');
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      className="space-y-2 rounded border p-3"
      aria-label={intl.formatMessage(messages.title)}
    >
      <h3>{intl.formatMessage(messages.title)}</h3>
      <p className="text-sm">{intl.formatMessage(messages.note)}</p>
      <label className="block">
        {intl.formatMessage(messages.handle)}
        <input
          className="block w-full rounded border p-2"
          value={handle}
          maxLength={256}
          autoComplete="off"
          disabled={busy}
          onChange={(event) => setHandle(event.target.value)}
        />
      </label>
      <label className="block">
        {intl.formatMessage(messages.password)}
        <input
          className="block w-full rounded border p-2"
          type="password"
          value={password}
          maxLength={4096}
          autoComplete="off"
          disabled={busy}
          onChange={(event) => setPassword(event.target.value)}
        />
      </label>
      <Button type="button" disabled={busy || !handle.trim() || !password} onClick={renew}>
        {intl.formatMessage(busy ? messages.busy : messages.renew)}
      </Button>
      {result && <p role="status">{intl.formatMessage(messages[result])}</p>}
    </section>
  );
}
