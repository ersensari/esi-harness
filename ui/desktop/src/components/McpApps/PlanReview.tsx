import { useState } from 'react';
import { getAcpClient } from '../../acp/acpConnection';
import ElicitationRequest from '../ElicitationRequest';

type Review = { token: string; scope: Record<string, unknown> };

export default function PlanReview({
  sessionId,
  plan,
  onRefresh,
}: {
  sessionId: string;
  plan: unknown;
  onRefresh?: () => Promise<void>;
}) {
  const [review, setReview] = useState<Review>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const snapshot = plan as {
    content_hash?: string;
    storage_revision?: number;
    exists?: boolean;
  } | null;
  if (
    !snapshot?.exists ||
    !snapshot.content_hash ||
    !Number.isSafeInteger(snapshot.storage_revision)
  )
    return null;
  const request = async (params: Record<string, unknown>) => {
    const client = await getAcpClient();
    return client.connection.agent.request('_goose/esi/plan-review', {
      ...params,
      session_id: sessionId,
    });
  };
  const prepare = async () => {
    setBusy(true);
    setMessage('');
    setReview(undefined);
    try {
      const result = await request({
        action: 'prepare',
        hash: snapshot.content_hash,
        storage_revision: snapshot.storage_revision,
      });
      setReview(result as Review);
    } catch {
      setMessage(
        'Plan changed or review is unavailable. Refresh the Canvas snapshot and try again.'
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <section aria-label="Desktop plan approval" className="border border-border-primary p-3">
      <p className="text-sm">Desktop approval · Canvas remains read-only</p>
      <button type="button" disabled={busy} onClick={() => void prepare()}>
        Review this plan revision
      </button>
      {onRefresh && (
        <button
          type="button"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            setReview(undefined);
            setMessage('');
            try {
              await onRefresh();
            } catch {
              setMessage('Could not refresh the Canvas snapshot. No approval was changed.');
            } finally {
              setBusy(false);
            }
          }}
        >
          Refresh Canvas snapshot
        </button>
      )}
      {message && <p role="status">{message}</p>}
      {review && (
        <>
          <ElicitationRequest
            key={review.token}
            isCancelledMessage={false}
            isClicked={false}
            actionRequiredContent={{
              type: 'actionRequired',
              data: {
                actionType: 'elicitation',
                id: review.token,
                message: `Approve implementation of this exact workspace plan?\n${JSON.stringify(review.scope, null, 2)}`,
                requested_schema: {
                  type: 'object',
                  properties: { approve: { type: 'boolean', title: 'Approve this plan' } },
                  required: ['approve'],
                  additionalProperties: false,
                },
              },
            }}
            onSubmit={async (token, values) => {
              setBusy(true);
              try {
                const result = (await request({
                  action: 'complete',
                  token,
                  approve: values.approve === true,
                })) as { approved: boolean };
                setMessage(
                  result.approved
                    ? 'Plan approved. Refresh the Canvas snapshot; Wiki sync can be requested separately.'
                    : 'Plan was not approved.'
                );
                setReview(undefined);
                return true;
              } catch {
                setMessage(
                  'Review expired or plan changed. Refresh the Canvas snapshot and review again.'
                );
                setReview(undefined);
                return false;
              } finally {
                setBusy(false);
              }
            }}
          />
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              const token = review.token;
              setReview(undefined);
              void request({ action: 'complete', token, approve: false }).catch(() => {});
            }}
          >
            Cancel review
          </button>
        </>
      )}
    </section>
  );
}
