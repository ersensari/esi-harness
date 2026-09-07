import { beforeEach, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlTestWrapper } from '../../../../i18n/test-utils';
import { renewWikiSession } from '../../../../acp/wiki';
import WikiSessionForm from './WikiSessionForm';

vi.mock('../../../../acp/wiki', () => ({ renewWikiSession: vi.fn() }));
beforeEach(() => vi.clearAllMocks());

it('clears the password and uses only the dedicated renewal request', async () => {
  vi.mocked(renewWikiSession).mockResolvedValue(undefined);
  const user = userEvent.setup();
  render(<WikiSessionForm />, { wrapper: IntlTestWrapper });
  expect(screen.getByRole('button')).toBeDisabled();
  await user.type(screen.getByLabelText('Wiki handle'), 'alice');
  await user.type(screen.getByLabelText('Wiki password'), 'test-only-secret');
  await user.click(screen.getByRole('button', { name: 'Renew Wiki session' }));
  await waitFor(() => expect(renewWikiSession).toHaveBeenCalledWith('alice', 'test-only-secret'));
  expect(screen.getByLabelText('Wiki password')).toHaveValue('');
  expect(screen.getByRole('status')).toHaveTextContent('Wiki authorization updated');
});

it('does not echo credentials from an error response', async () => {
  vi.mocked(renewWikiSession).mockRejectedValue(new Error('test-only-secret'));
  const user = userEvent.setup();
  render(<WikiSessionForm />, { wrapper: IntlTestWrapper });
  await user.type(screen.getByLabelText('Wiki handle'), 'alice');
  await user.type(screen.getByLabelText('Wiki password'), 'test-only-secret');
  await user.click(screen.getByRole('button', { name: 'Renew Wiki session' }));
  await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('Wiki sign-in failed'));
  expect(screen.getByLabelText('Wiki password')).toHaveValue('');
  expect(screen.queryByText(/test-only-secret/)).not.toBeInTheDocument();
});
