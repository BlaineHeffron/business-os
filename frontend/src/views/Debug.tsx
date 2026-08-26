import { useCallback, useEffect, useState } from "react";
import type { DebugDiagnosticRow } from "../types/generated/DebugDiagnosticRow";
import type { DebugDiagnosticsResponse } from "../types/generated/DebugDiagnosticsResponse";
import {
  api,
  errorMessage,
  friendlyDiagnosticErrorLabel,
  isUnauthorized,
} from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import type { StatusTone } from "../lib/status";
import {
  Button,
  EmptyState,
  SkeletonRows,
  StatusBadge,
  cellCls,
  rowDivideCls,
  rowHoverCls,
  tableCls,
  tableWrapCls,
  theadCls,
} from "../components/ui";

const POLL_INTERVAL_MS = 60_000;
const diagnosticTimestampFormatter = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

function severityTone(severity: string): StatusTone {
  if (severity === "error") return "critical";
  if (severity === "warning") return "warning";
  return "neutral";
}

export default function Debug({
  onUnauthorized,
  focusDiagnosticId,
}: {
  onUnauthorized: () => void;
  focusDiagnosticId?: string | null;
}) {
  const [data, setData] = useState<DebugDiagnosticsResponse | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [spawnBusyId, setSpawnBusyId] = useState<string | null>(null);
  const [spawnResult, setSpawnResult] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setData(await api.debugDiagnostics());
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setLoaded(true);
    }
  }, [onUnauthorized]);

  useAppCommand("refresh", () => void load());

  usePolling(load, { intervalMs: POLL_INTERVAL_MS });

  useEffect(() => {
    if (focusDiagnosticId) setOpenId(focusDiagnosticId);
  }, [focusDiagnosticId]);

  const spawnAgent = useCallback(
    async (row: DebugDiagnosticRow) => {
      setSpawnBusyId(row.diagnostic_id);
      setSpawnResult(null);
      try {
        const result = await api.debugSpawnAgent({
          diagnostic_id: row.diagnostic_id,
          idempotency_key: crypto.randomUUID(),
        });
        setSpawnResult(
          `Spawned ${result.session_id}${
            result.thread_id ? ` in ${result.thread_id}` : ""
          }`,
        );
      } catch (err) {
        if (isUnauthorized(err)) onUnauthorized();
        else setSpawnResult(`Agent spawn failed: ${errorMessage(err)}`);
      } finally {
        setSpawnBusyId(null);
      }
    },
    [onUnauthorized],
  );

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-rose flex items-center justify-between">
        <h2 className="text-lg font-semibold text-zinc-100">Debug</h2>
        <span className="text-xs text-zinc-400">
          recent backend diagnostics
          {data && data.rows.length >= 200 ? " · latest 200 rows" : ""} ·
          polls every 60s
        </span>
      </div>

      {spawnResult ? (
        <div className="surface-card surface-flat surface-body-rose rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-300">
          {spawnResult}
        </div>
      ) : null}

      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load debug diagnostics: {error}
        </div>
      ) : null}

      {loaded && data && data.rows.length === 0 && !error ? (
        <EmptyState title="No backend diagnostics recorded.">
          Mutation failures, provider delivery errors, sync errors, document
          indexing errors, and failed LLM calls will appear here with
          correlation details.
        </EmptyState>
      ) : null}

      {!loaded && !error ? (
        <div className={`${tableWrapCls} surface-flat surface-body-rose`}>
          <table className={`${tableCls} min-w-[1080px] table-fixed`}>
            <thead className={`${theadCls} surface-head-rose`}>
              <tr>
                <th className={`${cellCls} w-44`}>when</th>
                <th className={`${cellCls} w-24`}>source</th>
                <th className={`${cellCls} w-40`}>category</th>
                <th className={`${cellCls} w-52`}>entity</th>
                <th className={cellCls}>error</th>
                <th className={`${cellCls} w-52`}>correlation</th>
                <th className={`${cellCls} w-52`}>reference</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              <SkeletonRows rows={5} cols={7} />
            </tbody>
          </table>
        </div>
      ) : null}

      {data && data.rows.length > 0 ? (
        <div className={`${tableWrapCls} surface-flat surface-body-rose`}>
          <table className={`${tableCls} min-w-[1080px] table-fixed`}>
            <thead className={`${theadCls} surface-head-rose border-b border-zinc-800`}>
              <tr>
                <th className={`${cellCls} w-44 font-medium`}>when</th>
                <th className={`${cellCls} w-24 font-medium`}>source</th>
                <th className={`${cellCls} w-40 font-medium`}>category</th>
                <th className={`${cellCls} w-52 font-medium`}>entity</th>
                <th className={`${cellCls} font-medium`}>error</th>
                <th className={`${cellCls} w-52 font-medium`}>correlation</th>
                <th className={`${cellCls} w-52 font-medium`}>reference</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {data.rows.map((row) => {
                const open = openId === row.diagnostic_id;
                const entity = [row.entity_kind, row.entity_id]
                  .filter(Boolean)
                  .join(" ");
                const occurredAt = new Date(row.occurred_at_ms);
                return (
                  <tr
                    key={row.diagnostic_id}
                    id={`debug-${row.diagnostic_id}`}
                    ref={
                      focusDiagnosticId === row.diagnostic_id
                        ? (el) => el?.scrollIntoView({ block: "center" })
                        : undefined
                    }
                    className={`${rowHoverCls} cursor-pointer align-top`}
                    onClick={() => setOpenId(open ? null : row.diagnostic_id)}
                  >
                    <td
                      className={`${cellCls} whitespace-nowrap text-zinc-400`}
                      title={occurredAt.toLocaleString()}
                    >
                      {diagnosticTimestampFormatter.format(occurredAt)}
                    </td>
                    <td className={cellCls}>
                      <StatusBadge tone={severityTone(row.severity)}>
                        {row.source}
                      </StatusBadge>
                    </td>
                    <td className={`${cellCls} font-mono text-zinc-300`}>
                      {row.category}
                      {open && row.operation ? (
                        <div className="mt-1 text-xs text-zinc-500">
                          {row.operation}
                        </div>
                      ) : null}
                    </td>
                    <td
                      className={`${cellCls} max-w-52 truncate font-mono text-zinc-400`}
                      title={entity || undefined}
                    >
                      {entity || "—"}
                    </td>
                    <td className={`${cellCls} min-w-0`}>
                      <StatusBadge tone="critical" title={row.error_code}>
                        {friendlyDiagnosticErrorLabel(row.error_code, {
                          source: row.source,
                          category: row.category,
                        })}
                      </StatusBadge>
                      {row.error_message ? (
                        <div
                          className="mt-1 max-w-full truncate text-xs leading-5 text-red-200"
                          title={row.error_message}
                        >
                          {row.error_message}
                        </div>
                      ) : null}
                      {open ? (
                        <div className="mt-2 space-y-1 font-mono text-xs text-zinc-500">
                          <div>{row.diagnostic_id}</div>
                          <div>code {row.error_code}</div>
                          {row.reference_id ? (
                            <div>ref {row.reference_id}</div>
                          ) : null}
                          {row.error_message ? (
                            <div className="max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-md border border-zinc-800 bg-zinc-950/70 p-2 text-red-200">
                              {row.error_message}
                            </div>
                          ) : null}
                          <Button
                            className="mt-2"
                            size="sm"
                            type="button"
                            busy={spawnBusyId === row.diagnostic_id}
                            onClick={(event) => {
                              event.stopPropagation();
                              void spawnAgent(row);
                            }}
                          >
                            Spawn agent
                          </Button>
                        </div>
                      ) : null}
                    </td>
                    <td className={`${cellCls} max-w-56 truncate font-mono text-zinc-400`}>
                      {row.correlation_id || "—"}
                    </td>
                    <td
                      className={`${cellCls} max-w-52 truncate font-mono text-zinc-400`}
                      title={row.reference_id || undefined}
                    >
                      {row.reference_id || "—"}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : null}
    </div>
  );
}
