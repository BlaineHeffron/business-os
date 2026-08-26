export type FunnelStage = {
  label: string;
  value: number;
  color?: string;
  /** Optional pre-formatted value shown next to the stage (defaults to the number). */
  display?: string;
  target?: unknown;
};

const DEFAULT_COLOR = "var(--color-sky-400)";

/**
 * Dependency-light funnel: centered CSS bars that narrow by stage, no chart
 * library. Generic over `stages` so the slice-4 sales pipeline can reuse it.
 * When there are no stages (or every stage is zero) it renders the
 * pending-data state that slice ships until the deals source is wired.
 */
export default function Funnel({
  stages,
  title,
  ariaLabel,
  pendingLabel = "Pending deals data",
  onStageClick,
}: {
  stages: FunnelStage[];
  title?: string;
  ariaLabel?: string;
  pendingLabel?: string;
  onStageClick?: (stage: FunnelStage) => void;
}) {
  const max = stages.reduce((peak, stage) => Math.max(peak, stage.value), 0);

  if (stages.length === 0 || max <= 0) {
    return (
      <div className="flex h-full items-center justify-center rounded-md border border-dashed border-zinc-800 bg-zinc-950/40 px-3 py-6 text-xs text-zinc-500">
        {pendingLabel}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1.5" role="img" aria-label={ariaLabel ?? title}>
      {title ? <div className="sr-only">{title}</div> : null}
      {stages.map((stage, index) => {
        const pct = Math.max(6, Math.min(100, (stage.value / max) * 100));
        const body = (
          <>
            <div
              className="flex h-7 items-center justify-center rounded-sm text-xs font-medium text-zinc-950"
              style={{ width: `${pct}%`, backgroundColor: stage.color ?? DEFAULT_COLOR }}
            >
              {stage.display ?? stage.value}
            </div>
            <div className="text-[11px] text-zinc-400">{stage.label}</div>
          </>
        );
        return (
          <div key={`${stage.label}-${index}`} className="flex flex-col items-center gap-0.5">
            {stage.target && onStageClick ? (
              <button
                type="button"
                onClick={() => onStageClick(stage)}
                className="flex w-full flex-col items-center gap-0.5 rounded-sm transition hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
              >
                {body}
              </button>
            ) : (
              body
            )}
          </div>
        );
      })}
    </div>
  );
}
