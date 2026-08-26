export type StatusTone =
  | "ok"
  | "warning"
  | "critical"
  | "info"
  | "ai"
  | "progress"
  | "neutral";

const toneMap: Record<StatusTone, string> = {
  ok: "bg-emerald-500/10 text-emerald-300 ring-1 ring-inset ring-emerald-500/30",
  warning: "bg-amber-500/10 text-amber-300 ring-1 ring-inset ring-amber-500/30",
  critical: "bg-red-500/10 text-red-300 ring-1 ring-inset ring-red-500/30",
  info: "bg-sky-500/10 text-sky-300 ring-1 ring-inset ring-sky-500/30",
  ai: "bg-violet-500/10 text-violet-300 ring-1 ring-inset ring-violet-500/30",
  progress: "bg-sky-500/10 text-sky-300 ring-1 ring-inset ring-sky-500/30",
  neutral: "bg-zinc-500/10 text-zinc-300 ring-1 ring-inset ring-zinc-500/30",
};

const dotMap: Record<StatusTone, string> = {
  ok: "bg-emerald-400",
  warning: "bg-amber-400",
  critical: "bg-red-400",
  info: "bg-sky-400",
  ai: "bg-violet-400",
  progress: "bg-sky-400",
  neutral: "bg-zinc-400",
};

export function statusToneCls(tone: StatusTone): string {
  return toneMap[tone];
}

export function statusDotCls(tone: StatusTone): string {
  return dotMap[tone];
}
