import { useEffect, useState } from 'react';
import { getAcpClient } from '../../../../acp/acpConnection';
import { Switch } from '../../../ui/switch';
import { defineMessages, useIntl } from '../../../../i18n';

const messages = defineMessages({
  title: { id: 'extensionTrust.title', defaultMessage: 'Trust — unrestricted execution' },
  toggle: { id: 'extensionTrust.toggle', defaultMessage: 'Trust {name}' },
  help: {
    id: 'extensionTrust.help',
    defaultMessage:
      'Trusted extensions can access files and networks outside the workspace. Tool permissions still apply. After granting trust, reconnect the extension or start a new chat.',
  },
  loadError: {
    id: 'extensionTrust.loadError',
    defaultMessage: 'Could not load Trust. Save the extension and reopen Settings.',
  },
  saveError: {
    id: 'extensionTrust.saveError',
    defaultMessage: 'Could not update Trust. The previous permission remains unchanged.',
  },
});

export default function ExtensionTrust({ configKey }: { configKey: string }) {
  const intl = useIntl();
  const [trusted, setTrusted] = useState(false);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState('');

  async function request(value?: boolean): Promise<boolean> {
    const client = await getAcpClient();
    const result = (await client.connection.agent.request('_goose/esi/extension-trust', {
      configKey,
      ...(value === undefined ? {} : { trusted: value }),
    })) as { trusted: boolean };
    return result.trusted;
  }

  useEffect(() => {
    let active = true;
    setBusy(true);
    request()
      .then((value) => {
        if (active) setTrusted(value);
      })
      .catch(() => {
        if (active) setError(intl.formatMessage(messages.loadError));
      })
      .finally(() => {
        if (active) setBusy(false);
      });
    return () => {
      active = false;
    };
    // The request identity is the persisted configuration key.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configKey]);

  async function change(value: boolean) {
    setBusy(true);
    setError('');
    try {
      setTrusted(await request(value));
    } catch {
      setError(intl.formatMessage(messages.saveError));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="mt-3 border-t border-border-default pt-3">
      <label className="flex items-center justify-between gap-2">
        <span>{intl.formatMessage(messages.title)}</span>
        <Switch
          aria-label={intl.formatMessage(messages.toggle, { name: configKey })}
          checked={trusted}
          disabled={busy}
          onCheckedChange={change}
        />
      </label>
      <p className="mt-1 text-xs">{intl.formatMessage(messages.help)}</p>
      {error && (
        <p role="alert" className="mt-1 text-xs text-red-500">
          {error}
        </p>
      )}
    </div>
  );
}
