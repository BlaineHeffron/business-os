import type { StatusTone } from "../../lib/status";

/**
 * Superset draftTone used by all draft panels.
 * FollowUp and Content never pass dryRun so the dry-run branch is inert for
 * them — output identical to their local versions.
 */
export function draftTone(
  status: string,
  dryRun?: boolean | null,
): { tone: StatusTone; label: string } {
  if (status === "staged") return { tone: "info", label: "awaiting review" };
  if (status === "approved") {
    if (dryRun) return { tone: "warning", label: "dry-run" };
    return { tone: "ok", label: "approved" };
  }
  if (status === "rejected") return { tone: "critical", label: "rejected" };
  return { tone: "neutral", label: status };
}
