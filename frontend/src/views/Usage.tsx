import { useCallback, useState } from "react";
import type { AiUsageResponse } from "../types/generated/AiUsageResponse";
import { api, errorMessage, friendlyErrorLabel, isUnauthorized } from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import SectionHelpButton from "../components/SectionHelpButton";
import {
  Button,
  EmptyState,
  SkeletonRows,
  StatusBadge,
  cellCls,
  numCellCls,
  rowDivideCls,
  rowHoverCls,
  tableCls,
  tableWrapCls,
  theadCls,
} from "../components/ui";

const POLL_INTERVAL_MS = 60_000;

function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function fmtCost(micros: number): string {
  return micros > 0 ? `$${(micros / 1_000_000).toFixed(4)}` : "—";
}

function fmtDateTime(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
  });
}

// The AI usage LOG. LLM model/routing configuration lives in Settings → AI.
export default function Usage({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  debugEnabled,
  onOpenDebug,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  debugEnabled: boolean;
  onOpenDebug: (diagnosticId?: string) => void;
}) {
  const [data, setData] = useState<AiUsageResponse | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadUsage = useCallback(async () => {
    try {
      setData(await api.aiUsage());
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setLoaded(true);
    }
  }, [onUnauthorized]);

  useAppCommand("refresh", () => void loadUsage());

  usePolling(loadUsage, { intervalMs: POLL_INTERVAL_MS });

  const recentFailures = data?.rows.filter((row) => !row.success).slice(0, 8) ?? [];

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-amber flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">AI Usage</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for AI Usage"
          />
        </div>
        <span className="text-xs text-zinc-400">
          AI calls, errors, and timing
          {data && data.rows.length >= 100 ? " · most recent 100" : ""} · updates
          every 60s
        </span>
      </div>

      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load AI usage: {error}
        </div>
      ) : null}

      {data ? (
        <div className="flex flex-col gap-3 md:flex-row">
          <section className="surface-card surface-flat surface-body-amber flex-1 rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
            <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
              Last 24 hours
            </div>
            <div className="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 text-sm md:grid-cols-4">
              <div>
                <span className="text-zinc-400">calls </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {data.totals_last_24h.calls}
                </span>
                {data.totals_last_24h.failures > 0 ? (
                  <span className="ml-1 text-xs text-red-400">
                    ({data.totals_last_24h.failures} failed)
                  </span>
                ) : null}
              </div>
              <div>
                <span className="text-zinc-400">in </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {fmtTokens(data.totals_last_24h.tokens_in)}
                </span>
              </div>
              <div>
                <span className="text-zinc-400">out </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {fmtTokens(data.totals_last_24h.tokens_out)}
                </span>
              </div>
              <div>
                <span className="text-zinc-400">cost </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {fmtCost(data.totals_last_24h.cost_micros)}
                </span>
              </div>
            </div>
          </section>
          <section className="surface-card surface-flat surface-body-amber flex-1 rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
            <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
              All time
            </div>
            <div className="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 text-sm md:grid-cols-4">
              <div>
                <span className="text-zinc-400">calls </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {data.totals_all_time.calls}
                </span>
                {data.totals_all_time.failures > 0 ? (
                  <span className="ml-1 text-xs text-red-400">
                    ({data.totals_all_time.failures} failed)
                  </span>
                ) : null}
              </div>
              <div>
                <span className="text-zinc-400">in </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {fmtTokens(data.totals_all_time.tokens_in)}
                </span>
              </div>
              <div>
                <span className="text-zinc-400">out </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {fmtTokens(data.totals_all_time.tokens_out)}
                </span>
              </div>
              <div>
                <span className="text-zinc-400">cost </span>
                <span className="font-semibold tabular-nums text-zinc-100">
                  {fmtCost(data.totals_all_time.cost_micros)}
                </span>
              </div>
            </div>
          </section>
        </div>
      ) : null}

      {debugEnabled && data && data.totals_last_24h.failures > 0 ? (
        <div className="flex items-center justify-between rounded-md border border-red-900/50 bg-red-950/25 px-3 py-2 text-sm text-red-200">
          <span>
            {data.totals_last_24h.failures} failed AI call
            {data.totals_last_24h.failures === 1 ? "" : "s"} in the last 24
            hours.
          </span>
          <Button variant="ghost" size="sm" onClick={() => onOpenDebug()}>
            Debug
          </Button>
        </div>
      ) : null}

      {recentFailures.length > 0 ? (
        <section className="surface-card surface-flat surface-body-amber rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
          <div className="mb-2 flex items-center justify-between">
            <div className="text-xs font-semibold uppercase tracking-wide text-red-300">
              Recent failures
            </div>
            <StatusBadge tone="critical">
              {recentFailures.length} shown
            </StatusBadge>
          </div>
          <div className={`${tableWrapCls} surface-flat surface-body-amber`}>
            <table className={tableCls}>
              <thead className={`${theadCls} surface-head-amber border-b border-zinc-800`}>
                <tr>
                  <th className={`${cellCls} font-medium`}>when</th>
                  <th className={`${cellCls} font-medium`}>purpose</th>
                  <th className={`${cellCls} font-medium`}>item/correlation</th>
                  <th className={`${numCellCls} font-medium`}>latency</th>
                  <th className={`${cellCls} font-medium`}>error</th>
                </tr>
              </thead>
              <tbody className={rowDivideCls}>
                {recentFailures.map((row) => (
                  <tr key={row.usage_id} className={rowHoverCls}>
                    <td
                      className={`${cellCls} whitespace-nowrap text-zinc-400`}
                      title={new Date(row.recorded_at_ms).toLocaleString()}
                    >
                      {fmtDateTime(row.recorded_at_ms)}
                    </td>
                    <td className={`${cellCls} font-mono text-zinc-200`}>
                      {row.purpose}
                    </td>
                    <td
                      className={`${cellCls} max-w-56 truncate font-mono text-zinc-300`}
                      title={row.correlation_id}
                    >
                      {row.correlation_id || "—"}
                    </td>
                    <td className={`${numCellCls} text-zinc-400`}>
                      {(row.latency_ms / 1000).toFixed(1)}s
                    </td>
                    <td className={`${cellCls} whitespace-nowrap`}>
                      <StatusBadge tone="critical" title={row.error_code ?? "unknown error"}>
                        {friendlyErrorLabel(row.error_code)}
                      </StatusBadge>
                      {debugEnabled ? (
                        <Button
                          className="ml-2"
                          variant="ghost"
                          size="sm"
                          onClick={() => onOpenDebug(`llm:${row.usage_id}`)}
                        >
                          Debug
                        </Button>
                      ) : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      ) : null}

      {loaded && data && data.rows.length === 0 && !error ? (
        <EmptyState title="No AI calls recorded yet.">
          Rows appear when AI sorting runs or you create a draft from
          the Queue.
        </EmptyState>
      ) : null}

      {!loaded && !error ? (
        <div className={`${tableWrapCls} surface-flat surface-body-amber`}>
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-amber`}>
              <tr>
                <th className={cellCls}>when</th>
                <th className={cellCls}>purpose</th>
                <th className={cellCls}>route</th>
                <th className={cellCls}>correlation</th>
                <th className={cellCls}>model</th>
                <th className={`${numCellCls} font-medium`}>in</th>
                <th className={`${numCellCls} font-medium`}>out</th>
                <th className={`${numCellCls} font-medium`}>latency</th>
                <th className={cellCls}>outcome</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              <SkeletonRows rows={5} cols={9} />
            </tbody>
          </table>
        </div>
      ) : null}

      {data && data.rows.length > 0 ? (
        <div className={`${tableWrapCls} surface-flat surface-body-amber`}>
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-amber border-b border-zinc-800`}>
              <tr>
                <th className={`${cellCls} font-medium`}>when</th>
                <th className={`${cellCls} font-medium`}>purpose</th>
                <th className={`${cellCls} font-medium`}>route</th>
                <th className={`${cellCls} font-medium`}>correlation</th>
                <th className={`${cellCls} font-medium`}>model</th>
                <th className={`${numCellCls} font-medium`}>in</th>
                <th className={`${numCellCls} font-medium`}>out</th>
                <th className={`${numCellCls} font-medium`}>latency</th>
                <th className={`${cellCls} font-medium`}>outcome</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {data.rows.map((row) => (
                <tr key={row.usage_id} className={rowHoverCls}>
                  <td
                    className={`${cellCls} whitespace-nowrap text-zinc-400`}
                    title={new Date(row.recorded_at_ms).toLocaleString()}
                  >
                    {fmtDateTime(row.recorded_at_ms)}
                  </td>
                  <td className={`${cellCls} font-mono text-zinc-200`}>
                    {row.purpose}
                  </td>
                  <td className={`${cellCls} text-zinc-400`}>{row.route}</td>
                  <td
                    className={`${cellCls} max-w-48 truncate font-mono text-zinc-400`}
                    title={row.correlation_id}
                  >
                    {row.correlation_id || "—"}
                  </td>
                  <td
                    className={`${cellCls} max-w-40 truncate text-zinc-400`}
                    title={`${row.provider} / ${row.model}`}
                  >
                    {row.model || "—"}
                  </td>
                  <td className={`${numCellCls} text-zinc-300`}>
                    {row.tokens_in != null ? fmtTokens(row.tokens_in) : "—"}
                  </td>
                  <td className={`${numCellCls} text-zinc-300`}>
                    {row.tokens_out != null ? fmtTokens(row.tokens_out) : "—"}
                  </td>
                  <td className={`${numCellCls} text-zinc-400`}>
                    {(row.latency_ms / 1000).toFixed(1)}s
                  </td>
                  <td className={`${cellCls} whitespace-nowrap`}>
                    {row.success ? (
                      <StatusBadge tone="ok">ok</StatusBadge>
                    ) : (
                      <>
                        <StatusBadge tone="critical" title={row.error_code ?? "unknown error"}>
                          {friendlyErrorLabel(row.error_code)}
                        </StatusBadge>
                        {debugEnabled ? (
                          <Button
                            className="ml-2"
                            variant="ghost"
                            size="sm"
                            onClick={() => onOpenDebug(`llm:${row.usage_id}`)}
                          >
                            Debug
                          </Button>
                        ) : null}
                      </>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </div>
  );
}
