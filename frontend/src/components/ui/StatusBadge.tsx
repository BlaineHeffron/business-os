import type { ReactNode } from "react";
import { statusDotCls, statusToneCls, type StatusTone } from "../../lib/status";

interface StatusBadgeProps {
  tone: StatusTone;
  children: ReactNode;
  title?: string;
  pulse?: boolean;
}

export default function StatusBadge({
  tone,
  children,
  title,
  pulse,
}: StatusBadgeProps) {
  return (
    <span
      title={title}
      className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ${statusToneCls(tone)}`}
    >
      <span
        className={`h-1.5 w-1.5 rounded-full ${statusDotCls(tone)} ${pulse ? "animate-pulse" : ""}`}
      />
      {children}
    </span>
  );
}
