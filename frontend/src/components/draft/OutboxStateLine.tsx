import { useState } from "react";
import type { OutboxJobSummary } from "../../types/generated/OutboxJobSummary";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import { Button, StatusBadge } from "../ui";

interface OutboxStateLineProps {
  job: OutboxJobSummary | null | undefined;
  show: boolean;
  dryRunText: string;
  deliveredText: (job: OutboxJobSummary) => string;
  onUnauthorized?: () => void;
  onRetried?: () => void | Promise<void>;
}

export default function OutboxStateLine({
  job,
  show,
  dryRunText,
  deliveredText,
  onUnauthorized,
  onRetried,
}: OutboxStateLineProps) {
  const [retrying, setRetrying] = useState(false);
  const [retryError, setRetryError] = useState<string | null>(null);
  if (!show || !job) return null;

  const retry = async () => {
    setRetrying(true);
    setRetryError(null);
    try {
      await api.retryOutboxJob(job.job_id, {
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      await onRetried?.();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized?.();
      } else if (isRevisionConflict(err)) {
        setRetryError("Changed elsewhere — reload.");
        await onRetried?.();
      } else {
        setRetryError(`Retry failed: ${errorMessage(err)}`);
      }
    } finally {
      setRetrying(false);
    }
  };

  return (
    <div className="min-w-0 flex flex-col gap-1 text-xs" aria-live="polite">
      <div className="flex flex-col items-start gap-2 sm:flex-row sm:items-center">
        {job.status === "pending" ? (
          <StatusBadge tone="progress" pulse>
            Queued for delivery
          </StatusBadge>
        ) : job.status === "delivered" ? (
          job.dry_run ? (
            <StatusBadge tone="warning">Dry run complete</StatusBadge>
          ) : (
            <StatusBadge tone="ok">Delivered</StatusBadge>
          )
        ) : job.status === "delivery_outcome_unknown" ? (
          <StatusBadge tone="critical">Reconciliation required</StatusBadge>
        ) : (
          <StatusBadge tone="critical">Delivery failed</StatusBadge>
        )}
        <span className="min-w-0 w-full break-words text-xs text-zinc-400 [overflow-wrap:anywhere] sm:w-auto sm:flex-1">
          {job.status === "pending"
            ? job.attempts > 0
              ? "Couldn't send — we'll keep trying."
              : "Waiting to send."
            : job.status === "delivered" && job.dry_run
              ? dryRunText
              : job.status === "delivered"
                ? deliveredText(job)
                : job.status === "delivery_outcome_unknown"
                  ? "The provider may have accepted this write. Check that destination manually before doing anything else; BusinessOS will not retry an uncertain create."
                  : `Couldn't send: ${job.last_error ?? "unknown error"}. Fix the connection or payload, then retry only this destination.`}
        </span>
        {job.status === "failed_terminal" ? (
          <Button
            variant="secondary"
            size="sm"
            busy={retrying}
            disabled={retrying}
            onClick={() => void retry()}
          >
            {retrying ? "Retrying…" : "Retry"}
          </Button>
        ) : null}
      </div>
      {retryError ? <div className="text-red-300">{retryError}</div> : null}
    </div>
  );
}
