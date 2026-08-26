/**
 * Dependency-light sparkline: a single SVG polyline, no chart library. Generic
 * over `points` so later slices (e.g. the daily-revenue trend) can reuse it.
 * Stroke uses currentColor — set the line color with a text-* class. Renders
 * nothing meaningful (a flat baseline) when there are fewer than two points.
 */
export default function Sparkline({
  points,
  width = 120,
  height = 32,
  strokeWidth = 2,
  className = "text-sky-400",
  title,
  ariaLabel,
  showArea = false,
  showGrid = false,
}: {
  points: number[];
  width?: number;
  height?: number;
  strokeWidth?: number;
  className?: string;
  title?: string;
  ariaLabel?: string;
  showArea?: boolean;
  showGrid?: boolean;
}) {
  const pad = strokeWidth;
  const innerW = Math.max(1, width - pad * 2);
  const innerH = Math.max(1, height - pad * 2);
  const min = points.length ? Math.min(...points) : 0;
  const max = points.length ? Math.max(...points) : 0;
  const span = max - min;

  const coords = points.map((value, index) => {
    const x = pad + (points.length > 1 ? (index / (points.length - 1)) * innerW : innerW / 2);
    const y = pad + (span > 0 ? innerH - ((value - min) / span) * innerH : innerH / 2);
    return `${x.toFixed(2)},${y.toFixed(2)}`;
  });
  const areaPath =
    coords.length >= 2
      ? `M ${coords.join(" L ")} L ${width - pad},${height - pad} L ${pad},${height - pad} Z`
      : null;

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label={ariaLabel ?? title}
      className={className}
      preserveAspectRatio="none"
    >
      {title ? <title>{title}</title> : null}
      {showGrid ? (
        <g className="text-zinc-800/70" stroke="currentColor" strokeWidth="1">
          <line x1={pad} y1={height - pad} x2={width - pad} y2={height - pad} />
          <line x1={pad} y1={height / 2} x2={width - pad} y2={height / 2} opacity="0.45" />
        </g>
      ) : null}
      {showArea && areaPath ? (
        <path d={areaPath} fill="currentColor" opacity="0.12" />
      ) : null}
      {coords.length >= 2 ? (
        <polyline
          points={coords.join(" ")}
          fill="none"
          stroke="currentColor"
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      ) : (
        <line
          x1={pad}
          y1={height / 2}
          x2={width - pad}
          y2={height / 2}
          stroke="currentColor"
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          className="opacity-40"
        />
      )}
    </svg>
  );
}
