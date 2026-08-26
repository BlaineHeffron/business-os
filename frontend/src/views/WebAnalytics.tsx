import { useCallback, useState, type ReactNode } from "react";
import type { SearchConsoleTrafficOverview } from "../types/generated/SearchConsoleTrafficOverview";
import { api, ApiError, errorMessage, isUnauthorized } from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import Button from "../components/ui/Button";
import MetricHelp from "../components/ui/MetricHelp";

function fmtCompact(value: number): string {
  return value.toLocaleString("en-US", {
    notation: value >= 10_000 ? "compact" : "standard",
    maximumFractionDigits: value >= 10_000 ? 1 : 0,
  });
}

function fmtAgo(ms: bigint | number | null | undefined): string {
  if (ms === null || ms === undefined) return "never";
  const delta = Date.now() - Number(ms);
  if (delta < 60_000) return "just now";
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function fmtPercentMicros(value: number): string {
  return `${(value / 10_000).toFixed(1)}%`;
}

function fmtPositionMicros(value: number): string {
  return (value / 1_000_000).toFixed(1);
}

function analyticsSpamExclusionDetail(traffic: SearchConsoleTrafficOverview): string | null {
  const excluded = traffic.analytics_excluded_referrer_spam_week.sessions;
  if (excluded <= 0) return null;
  return `${fmtCompact(excluded)} suspected spam sessions excluded`;
}

function syncConflictReason(err: unknown): string | null {
  if (!(err instanceof ApiError) || err.status !== 409) return null;
  const body = err.body;
  if (
    body !== null &&
    typeof body === "object" &&
    "reason" in body &&
    typeof (body as { reason?: unknown }).reason === "string"
  ) {
    return (body as { reason: string }).reason;
  }
  return null;
}

function Kpi({
  label,
  value,
  detail,
  help,
}: {
  label: ReactNode;
  value: string;
  detail?: string;
  help?: string;
}) {
  return (
    <div className="surface-card surface-flat surface-body-sky rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
      <div className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-zinc-500">
        <span>{label}</span>
        {help ? <MetricHelp label={`What ${label} means`}>{help}</MetricHelp> : null}
      </div>
      <div className="mt-1 text-2xl font-bold tabular-nums text-zinc-100">
        {value}
      </div>
      {detail ? (
        <div className="mt-1 text-[11px] leading-snug text-zinc-600">{detail}</div>
      ) : null}
    </div>
  );
}

function DataTable({
  title,
  help,
  rows,
  valueLabel,
}: {
  title: string;
  help?: string;
  rows: Array<{ label: string; clicks?: number; sessions?: number }>;
  valueLabel: "clicks" | "sessions";
}) {
  return (
    <div className="surface-card surface-flat surface-body-sky rounded-lg border border-zinc-800 bg-zinc-900/40">
      <div className="flex items-center gap-1.5 border-b border-zinc-800 px-4 py-2 text-sm font-semibold text-zinc-100">
        <span>{title}</span>
        {help ? <MetricHelp label={`What ${title} means`}>{help}</MetricHelp> : null}
      </div>
      <div className="divide-y divide-zinc-800">
        {rows.length === 0 ? (
          <div className="px-4 py-6 text-sm text-zinc-500">No rows synced yet.</div>
        ) : (
          rows.slice(0, 8).map((row) => (
            <div key={row.label} className="flex items-center justify-between gap-4 px-4 py-2">
              <div className="min-w-0 truncate text-sm text-zinc-300">{row.label}</div>
              <div className="shrink-0 text-sm tabular-nums text-zinc-100">
                {fmtCompact(valueLabel === "clicks" ? row.clicks ?? 0 : row.sessions ?? 0)}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export default function WebAnalytics({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [traffic, setTraffic] = useState<SearchConsoleTrafficOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setTraffic(await api.searchConsoleStatus());
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useAppCommand("refresh", () => void load());

  usePolling(load, { intervalMs: 60_000 });

  const syncNow = async (source: "search_console" | "analytics") => {
    setNotice(null);
    try {
      const res =
        source === "analytics"
          ? await api.googleAnalyticsSyncNow()
          : await api.searchConsoleSyncNow();
      if (res.accepted) {
        setNotice(source === "analytics" ? "GA4 sync started." : "Search Console sync started.");
        await load();
      } else {
        setNotice("Traffic sync is already running or cooling down.");
      }
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else {
        const reason = syncConflictReason(err);
        if (reason === "google_analytics_not_configured") {
          setNotice("Configure a GA4 property before syncing Google Analytics.");
        } else if (reason === "search_console_not_configured") {
          setNotice("Select or configure a Search Console property before syncing.");
        } else if (reason === "sync_in_flight" || reason === "sync_cooldown") {
          setNotice("Traffic sync is already running or cooling down.");
        } else setNotice(errorMessage(err));
      }
    }
  };

  const selectProperty = async (siteUrl: string) => {
    if (!traffic) return;
    setNotice(null);
    try {
      await api.searchConsoleSelectProperty({
        site_url: siteUrl,
        expected_revision: traffic.selection_revision ?? null,
        idempotency_key: crypto.randomUUID(),
      });
      setNotice("Search Console property selected.");
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (err instanceof ApiError && err.status === 409) {
        setNotice("Search Console property changed elsewhere. Reload and try again.");
      } else setNotice(errorMessage(err));
    }
  };

  const propertyLabel = traffic?.property_url ?? "No property selected";
  const ga4Label = traffic?.analytics_property_id
    ? `GA4 ${traffic.analytics_property_id}`
    : "GA4 not configured";
  const analyticsExclusionDetail = traffic ? analyticsSpamExclusionDetail(traffic) : null;

  return (
    <div className="flex flex-col gap-6">
      <div className="surface-section-head surface-head-sky flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-lg font-semibold text-zinc-100">Web analytics</h2>
        <div className="flex flex-wrap items-center justify-end gap-2">
          {traffic?.properties.length ? (
            <select
              value={traffic.property_source === "config" ? "" : traffic.property_url ?? ""}
              onChange={(event) => {
                if (event.target.value) void selectProperty(event.target.value);
              }}
              disabled={traffic.property_source === "config"}
              className="max-w-72 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-300 disabled:opacity-50"
              title={
                traffic.property_source === "config"
                  ? "Search Console property is set by deployment config"
                  : "Select Search Console property"
              }
            >
              {traffic.property_source === "config" ? (
                <option value="">Configured: {traffic.property_url}</option>
              ) : null}
              {traffic.properties.map((property) => (
                <option key={property.site_url} value={property.site_url}>
                  {property.site_url}
                </option>
              ))}
            </select>
          ) : null}
          <Button
            size="sm"
            variant="secondary"
            onClick={() => void syncNow("search_console")}
            disabled={traffic?.in_flight === true}
          >
            {traffic?.in_flight ? "Syncing..." : "Sync Search Console"}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => void syncNow("analytics")}
            disabled={traffic?.in_flight === true || traffic?.analytics_configured !== true}
          >
            {traffic?.in_flight ? "Syncing..." : "Sync GA4"}
          </Button>
        </div>
      </div>

      {notice ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-300">
          {notice}
        </div>
      ) : null}
      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/30 px-3 py-2 text-sm text-red-300">
          {error}
        </div>
      ) : null}

      {!traffic ? (
        <div className="text-sm text-zinc-500">Loading...</div>
      ) : (
        <>
          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
            <Kpi
              label="Visits from Google search"
              value={traffic.configured ? fmtCompact(traffic.week.clicks) : "Pending"}
              detail={`${propertyLabel} · synced ${fmtAgo(traffic.last_synced_at_ms)}`}
              help="People who clicked a Google Search result and landed on the website this week. Source: Google Search Console."
            />
            <Kpi
              label="Google search appearances"
              value={traffic.configured ? fmtCompact(traffic.week.impressions) : "Pending"}
              detail={`CTR ${fmtPercentMicros(traffic.week.ctr_micros)} · avg position ${fmtPositionMicros(
                traffic.week.position_micros,
              )}`}
              help="How often the website appeared in Google Search results this week. This is not page views; one person can see a result without visiting the site."
            />
            <Kpi
              label="Website sessions"
              value={
                traffic.analytics_configured
                  ? fmtCompact(traffic.analytics_week.sessions)
                  : "Pending"
              }
              detail={`${ga4Label} · synced ${fmtAgo(traffic.analytics_last_synced_at_ms)}`}
              help="Visits measured on the website by Google Analytics 4. A session can include several page views."
            />
            <Kpi
              label="Conversions"
              value={
                traffic.analytics_configured
                  ? fmtCompact(traffic.analytics_week.conversions)
                  : "Pending"
              }
              detail={`${fmtCompact(traffic.analytics_month_to_date.sessions)} sessions MTD${
                analyticsExclusionDetail ? ` · ${analyticsExclusionDetail}` : ""
              }`}
              help="Website actions marked as conversions in Google Analytics 4, such as a form submission or other configured goal."
            />
          </div>

          <div className="grid gap-3 lg:grid-cols-2">
            <DataTable
              title="Search terms that brought visits"
              help="Google searches that produced clicks to the website. Low-volume searches may be hidden by Google, so these rows may not add up to total search visits."
              rows={traffic.top_queries_week.map((row) => ({
                label: row.value,
                clicks: row.metrics.clicks,
              }))}
              valueLabel="clicks"
            />
            <DataTable
              title="Landing pages by website sessions"
              help="Pages where visitors started a website session, measured by Google Analytics 4."
              rows={traffic.top_landing_pages_week.map((row) => ({
                label: row.value,
                sessions: row.metrics.sessions,
              }))}
              valueLabel="sessions"
            />
          </div>
        </>
      )}
    </div>
  );
}
