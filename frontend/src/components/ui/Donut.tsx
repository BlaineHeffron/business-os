import type { ReactNode } from "react";

export type DonutSegment = {
  label: string;
  value: number;
  color?: string;
  colorClassName?: string;
};

const PALETTE = [
  "var(--color-sky-400)",
  "var(--color-emerald-400)",
  "var(--color-amber-400)",
  "var(--color-red-400)",
  "var(--color-violet-400)",
  "var(--color-rose-400)",
];

/**
 * Dependency-light donut: stacked SVG ring arcs, no chart library. Generic
 * over `segments` so later slices (e.g. the production-orders stage donut) can
 * reuse it. Renders a neutral ring + "No data" when every segment is zero, and
 * exposes a center slot for a total.
 */
export default function Donut({
  segments,
  size = 120,
  thickness = 14,
  center,
  title,
  ariaLabel,
  rounded = false,
}: {
  segments: DonutSegment[];
  size?: number;
  thickness?: number;
  center?: ReactNode;
  title?: string;
  ariaLabel?: string;
  rounded?: boolean;
}) {
  const radius = (size - thickness) / 2;
  const circumference = 2 * Math.PI * radius;
  const total = segments.reduce((sum, segment) => sum + Math.max(0, segment.value), 0);

  let offset = 0;
  const arcs =
    total > 0
      ? segments.map((segment, index) => {
          const fraction = Math.max(0, segment.value) / total;
          const dash = fraction * circumference;
          const arc = (
            <circle
              key={`${segment.label}-${index}`}
              cx={size / 2}
              cy={size / 2}
              r={radius}
              fill="none"
              stroke={segment.colorClassName ? "currentColor" : (segment.color ?? PALETTE[index % PALETTE.length])}
              strokeWidth={thickness}
              strokeDasharray={`${dash} ${circumference - dash}`}
              strokeDashoffset={-offset}
              strokeLinecap={rounded ? "round" : "butt"}
              className={segment.colorClassName}
            >
              <title>{`${segment.label}: ${segment.value}`}</title>
            </circle>
          );
          offset += dash;
          return arc;
        })
      : null;

  return (
    <div className="relative inline-flex items-center justify-center" style={{ width: size, height: size }}>
      <svg
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        role="img"
        aria-label={ariaLabel ?? title}
        style={{ transform: "rotate(-90deg)" }}
      >
        {title ? <title>{title}</title> : null}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="currentColor"
          strokeWidth={thickness}
          className="text-zinc-800/80"
        />
        {arcs}
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center text-center">
        {total > 0 ? center : <span className="text-xs text-zinc-500">No data</span>}
      </div>
    </div>
  );
}
