export type BarItem = {
  label: string;
  value: number;
  color?: string;
  target?: unknown;
  /** Optional pre-formatted value shown at the row end (defaults to the number). */
  display?: string;
};

const DEFAULT_COLOR = "var(--color-sky-400)";

/**
 * Dependency-light horizontal bar chart (CSS tracks, no chart library).
 * Generic over `items` so later slices (e.g. the top-SKU bar) can reuse it.
 * Bars are sized against the largest value; renders an empty hint when there
 * is nothing to show.
 */
export default function Bar({
  items,
  title,
  ariaLabel,
  emptyLabel = "No data",
  clean = false,
  barClassName = "bg-sky-500",
  onItemClick,
}: {
  items: BarItem[];
  title?: string;
  ariaLabel?: string;
  emptyLabel?: string;
  clean?: boolean;
  barClassName?: string;
  onItemClick?: (item: BarItem) => void;
}) {
  const max = items.reduce((peak, item) => Math.max(peak, item.value), 0);
  const hasInteractiveItems = Boolean(onItemClick && items.some((item) => item.target));

  if (items.length === 0 || max <= 0) {
    return <div className="text-xs text-zinc-500">{emptyLabel}</div>;
  }

  return (
    <div
      className={clean ? "flex flex-col gap-2.5" : "flex flex-col gap-2"}
      role={hasInteractiveItems ? "group" : "img"}
      aria-label={ariaLabel ?? title}
    >
      {title ? <div className="sr-only">{title}</div> : null}
      {items.map((item, index) => {
        const pct = Math.max(0, Math.min(100, (item.value / max) * 100));
        const body = (
          <>
            <div className="truncate text-xs font-medium text-zinc-400" title={item.label}>
              {item.label}
            </div>
            <div className={`${clean ? "h-3" : "h-2.5"} flex-1 overflow-hidden rounded-full bg-zinc-800/80`}>
              <div
                className={`h-full rounded-full ${item.color ? "" : barClassName}`}
                style={item.color ? { width: `${pct}%`, backgroundColor: item.color ?? DEFAULT_COLOR } : { width: `${pct}%` }}
              />
            </div>
            <div className="text-right text-xs font-semibold tabular-nums text-zinc-300">
              {item.display ?? item.value}
            </div>
          </>
        );
        const className = clean
          ? "grid grid-cols-[minmax(4rem,7rem)_1fr_3.5rem] items-center gap-3"
          : "flex items-center gap-2";
        if (item.target && onItemClick) {
          return (
            <button
              key={`${item.label}-${index}`}
              type="button"
              onClick={() => onItemClick(item)}
              aria-label={`${item.label}: ${item.display ?? item.value}`}
              className={`${className} rounded-sm text-left transition hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70`}
            >
              {body}
            </button>
          );
        }
        return (
          <div key={`${item.label}-${index}`} className={className}>
            {body}
          </div>
        );
      })}
    </div>
  );
}
