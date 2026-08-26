import type { EnrichmentRunStatus } from "../types/generated/EnrichmentRunStatus";

export function isTerminalEnrichmentStatus(status: EnrichmentRunStatus): boolean {
  return (
    status === "completed" ||
    status === "partial" ||
    status === "skipped" ||
    status === "failed"
  );
}
