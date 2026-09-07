import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlTestWrapper } from '../../i18n/test-utils';
import PlanReview from './PlanReview';

const request = vi.hoisted(() => vi.fn());
vi.mock('../../acp/acpConnection', () => ({
  getAcpClient: async () => ({ connection: { agent: { request } } }),
}));
const plan = { exists: true, content_hash: 'a'.repeat(64), storage_revision: 7 };
beforeEach(() => request.mockReset());
const setup = () =>
  render(<PlanReview sessionId="session-a" plan={plan} />, { wrapper: IntlTestWrapper });

describe('native Canvas plan review', () => {
  it('requires keyboard review and explicit confirmation of server scope', async () => {
    request
      .mockResolvedValueOnce({
        token: 'one-use',
        scope: { hash: plan.content_hash, revision: 7, title: 'Exact scope' },
      })
      .mockResolvedValueOnce({ approved: true });
    setup();
    const user = userEvent.setup();
    expect(request).not.toHaveBeenCalled();
    await user.tab();
    expect(screen.getByRole('button', { name: 'Review this plan revision' })).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(await screen.findByText(/Exact scope/)).toBeInTheDocument();
    expect(request).toHaveBeenCalledWith('_goose/esi/plan-review', {
      action: 'prepare',
      session_id: 'session-a',
      hash: plan.content_hash,
      storage_revision: 7,
    });
    const checkbox = screen.getByRole('checkbox');
    checkbox.focus();
    await user.keyboard(' ');
    await user.tab();
    await user.keyboard('{Enter}');
    await waitFor(() =>
      expect(request).toHaveBeenLastCalledWith('_goose/esi/plan-review', {
        action: 'complete',
        session_id: 'session-a',
        token: 'one-use',
        approve: true,
      })
    );
    expect(await screen.findByRole('status')).toHaveTextContent('Plan approved');
  });
  it('cancels without approving', async () => {
    request
      .mockResolvedValueOnce({ token: 'cancel', scope: {} })
      .mockResolvedValueOnce({ approved: false });
    setup();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Review this plan revision' }));
    await user.click(await screen.findByRole('button', { name: 'Cancel review' }));
    await waitFor(() =>
      expect(request).toHaveBeenLastCalledWith('_goose/esi/plan-review', {
        action: 'complete',
        session_id: 'session-a',
        token: 'cancel',
        approve: false,
      })
    );
  });
  it('rejects stale preparation and never opens an approval form', async () => {
    request.mockRejectedValueOnce(new Error('stale'));
    setup();
    await userEvent.click(screen.getByRole('button', { name: 'Review this plan revision' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Refresh');
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
  });
  it('does not expose review for missing or unsafe revisions', () => {
    render(
      <PlanReview
        sessionId="s"
        plan={{ ...plan, storage_revision: Number.MAX_SAFE_INTEGER + 1 }}
      />,
      { wrapper: IntlTestWrapper }
    );
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
    expect(request).not.toHaveBeenCalled();
  });
  it('refreshes read-only state without preparing or granting approval', async () => {
    const refresh = vi.fn().mockResolvedValue(undefined);
    render(<PlanReview sessionId="s" plan={plan} onRefresh={refresh} />, {
      wrapper: IntlTestWrapper,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Refresh Canvas snapshot' }));
    expect(refresh).toHaveBeenCalledOnce();
    expect(request).not.toHaveBeenCalled();
  });
  it('reports failed completion without claiming approval', async () => {
    request
      .mockResolvedValueOnce({ token: 'stale', scope: {} })
      .mockRejectedValueOnce(new Error('stale'));
    setup();
    await userEvent.click(screen.getByRole('button', { name: 'Review this plan revision' }));
    await userEvent.click(await screen.findByRole('checkbox'));
    await userEvent.click(screen.getByRole('button', { name: 'Submit' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Review expired or plan changed');
    expect(screen.queryByText(/^Plan approved/)).not.toBeInTheDocument();
  });
});
