import type { ReactNode } from "react";
import { StatusBadge } from "../ui";
import { draftTone } from "./tone";

interface DraftStatusHeaderProps {
  status: string;
  dryRun?: boolean | null;
  confidence: string;
  model: string;
  /** Optional extra content appended inside the header row (e.g. calendar picker trigger). */
  extra?: ReactNode;
}

export default function DraftStatusHeader({
  status,
  dryRun,
  confidence,
  model,
  extra,
}: DraftStatusHeaderProps) {
  const { tone, label } = draftTone(status, dryRun);
  return (
    <div className="flex items-center gap-2">
      <StatusBadge tone={tone} pulse={status === "staged"}>
        {label}
      </StatusBadge>
      <span className="text-xs text-zinc-400">
        confidence: {confidence} · model: {model || "?"}
      </span>
      {extra}
    </div>
  );
}
