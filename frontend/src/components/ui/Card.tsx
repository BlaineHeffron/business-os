import type { ReactNode } from "react";
import { statusToneCls, type StatusTone } from "../../lib/status";

export type SurfaceAccent =
  | "sky"
  | "emerald"
  | "amber"
  | "orange"
  | "teal"
  | "violet"
  | "zinc"
  | "rose";

export function surfaceAccentClasses(accent: SurfaceAccent) {
  return {
    body: `surface-body-${accent}`,
    header: `surface-head-${accent}`,
    flat: "surface-card surface-flat",
  };
}

interface CardProps {
  children: ReactNode;
  className?: string;
}

export function Card({ children, className = "" }: CardProps) {
  return (
    <div
      className={`surface-card shadow-elevated min-w-0 rounded-lg border border-zinc-800 bg-zinc-900/40 p-4 ${className}`}
    >
      {children}
    </div>
  );
}

interface SurfaceProps {
  accent: SurfaceAccent;
  title?: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  headerClassName?: string;
  bodyClassName?: string;
  titleAs?: "h2" | "h3" | "div";
}

export function Surface({
  accent,
  title,
  subtitle,
  actions,
  children,
  className = "",
  headerClassName = "",
  bodyClassName = "p-4",
  titleAs = "div",
}: SurfaceProps) {
  const surface = surfaceAccentClasses(accent);
  const TitleTag = titleAs;
  return (
    <section
      className={`${surface.flat} ${surface.body} min-w-0 overflow-hidden rounded-lg border ${className}`}
    >
      {title || subtitle || actions ? (
        <div
          className={`${surface.header} flex items-start justify-between gap-3 border-b px-3 py-2 ${headerClassName}`}
        >
          <div className="min-w-0">
            {title ? (
              <TitleTag className="text-sm font-semibold leading-tight text-zinc-100">
                {title}
              </TitleTag>
            ) : null}
            {subtitle ? (
              <div className="mt-0.5 text-xs leading-snug text-zinc-400">
                {subtitle}
              </div>
            ) : null}
          </div>
          {actions ? <div className="min-w-0 shrink-0">{actions}</div> : null}
        </div>
      ) : null}
      <div className={bodyClassName}>{children}</div>
    </section>
  );
}

interface KpiCardProps {
  label: string;
  value: string;
  valueCls?: string;
  sub?: ReactNode;
  comparison?: ReactNode;
  footnote?: string;
  tone?: StatusTone;
  hero?: boolean;
  className?: string;
}

export function KpiCard({
  label,
  value,
  valueCls,
  sub,
  comparison,
  footnote,
  tone,
  hero,
  className = "",
}: KpiCardProps) {
  const heroBorder =
    tone === "ok"
      ? "border-emerald-700/60 ring-1 ring-inset ring-emerald-500/30"
      : tone === "warning"
        ? "border-amber-700/60 ring-1 ring-inset ring-amber-500/30"
        : tone === "critical"
          ? "border-red-700/60 ring-1 ring-inset ring-red-500/30"
          : hero
            ? "border-amber-700/60 ring-1 ring-inset ring-amber-500/30"
            : "border-zinc-800";

  const borderCls = hero || tone ? heroBorder : "border-zinc-800";

  const defaultValueCls = tone
    ? statusToneCls(tone).split(" ").find((c) => c.startsWith("text-")) ?? "text-zinc-100"
    : "text-zinc-100";

  return (
    <div
      className={`surface-card shadow-elevated rounded-lg border bg-zinc-900/40 p-4 ${borderCls} ${className}`}
    >
      <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
        {label}
      </div>
      <div
        className={`mt-1 font-bold tabular-nums ${hero ? "text-3xl" : "text-2xl"} ${
          valueCls ?? defaultValueCls
        }`}
      >
        {value}
      </div>
      {(sub ?? comparison) ? (
        <div className="mt-0.5 text-xs text-zinc-400">{sub ?? comparison}</div>
      ) : null}
      {footnote ? (
        <div className="mt-1 text-xs leading-snug text-zinc-500">{footnote}</div>
      ) : null}
    </div>
  );
}
