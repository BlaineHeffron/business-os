import type { ReactNode } from "react";
import type { DraftPanelNotice } from "./useDraftPanel";

interface DraftPanelShellProps {
  loaded: boolean;
  notice: DraftPanelNotice;
  children: ReactNode;
}

export default function DraftPanelShell({
  loaded,
  notice,
  children,
}: DraftPanelShellProps) {
  if (!loaded) {
    return (
      <div className="px-3 py-2 text-xs text-zinc-400">Loading drafts…</div>
    );
  }

  const noticeCls =
    notice?.kind === "conflict"
      ? "mb-2 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-1.5 text-xs text-amber-300"
      : "mb-2 rounded-md border border-red-900/60 bg-red-950/30 px-3 py-1.5 text-xs text-red-300";

  return (
    <div className="border-t border-zinc-800/60 bg-zinc-950/60 px-4 py-3">
      {notice ? <div className={noticeCls}>{notice.text}</div> : null}
      {children}
    </div>
  );
}
