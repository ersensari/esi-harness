import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import ExtensionTrust from './ExtensionTrust';
import { IntlTestWrapper } from '../../../../i18n/test-utils';

const request = vi.hoisted(() => vi.fn());
vi.mock('../../../../acp/acpConnection', () => ({
  getAcpClient: async () => ({ connection: { agent: { request } } }),
}));

beforeEach(() => request.mockReset());

describe('ExtensionTrust', () => {
  it('reads trust, grants and revokes through the private settings channel', async () => {
    request
      .mockResolvedValueOnce({ trusted: false })
      .mockResolvedValueOnce({ trusted: true })
      .mockResolvedValueOnce({ trusted: false });
    render(<ExtensionTrust configKey="fetch" />, { wrapper: IntlTestWrapper });
    const toggle = screen.getByRole('switch', { name: 'Trust fetch' });
    await waitFor(() => expect(toggle).not.toBeDisabled());
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'));
    expect(request).toHaveBeenLastCalledWith('_goose/esi/extension-trust', {
      configKey: 'fetch',
      trusted: true,
    });
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'false'));
  });

  it('does not pretend a failed save succeeded', async () => {
    request.mockResolvedValueOnce({ trusted: false }).mockRejectedValueOnce(new Error('offline'));
    render(<ExtensionTrust configKey="fetch" />, { wrapper: IntlTestWrapper });
    const toggle = screen.getByRole('switch');
    await waitFor(() => expect(toggle).not.toBeDisabled());
    fireEvent.click(toggle);
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not update Trust');
    expect(toggle).toHaveAttribute('aria-checked', 'false');
  });
});
