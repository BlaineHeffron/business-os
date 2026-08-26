import { useCallback, useRef, useState } from "react";
import type { DigestTrafficMetrics } from "../types/generated/DigestTrafficMetrics";
import type { OwnerReport } from "../types/generated/OwnerReport";
import type { OwnerReportWithRevision } from "../types/generated/OwnerReportWithRevision";
import { api, ApiError, errorMessage, isUnauthorized } from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import SectionHelpButton from "../components/SectionHelpButton";

const POLL_INTERVAL_MS = 60_000;
/** Faster after Generate-now so fresh digests appear as they land. */
const GENERATING_POLL_INTERVAL_MS = 5_000;
/** Stop fast-polling if no fresh digest landed. */
const GENERATING_TIMEOUT_MS = 3 * 60_000;

function fmtMoney(cents: number): string {
  return (cents / 100).toLocaleString("en-US", {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
  });
}

/** Hero-card money: $52.3K-style, exact cents in tooltips. */
function fmtMoneyCompact(cents: number): string {
  const sign = cents < 0 ? "-" : "";
  const dollars = Math.abs(cents) / 100;
  if (dollars >= 1_000_000) return `${sign}$${(dollars / 1_000_000).toFixed(2)}M`;
  if (dollars >= 100_000) return `${sign}$${Math.round(dollars / 1_000)}K`;
  if (dollars >= 10_000) return `${sign}$${(dollars / 1_000).toFixed(1)}K`;
  return `${sign}$${dollars.toLocaleString("en-US", { maximumFractionDigits: 0 })}`;
}

function fmtAgo(ms: number | null): string {
  if (ms === null) return "never";
  const delta = Date.now() - ms;
  if (delta < 60_000) return "just now";
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function periodTitle(report: OwnerReport): string {
  return report.period_kind === "weekly"
    ? `This week (${report.period_start} → ${report.period_end})`
    : `Month to date (${report.period_start} → ${report.period_end})`;
}

function comparisonText(current: number, prior: number | null | undefined): string | null {
  if (prior === null || prior === undefined) return null;
  if (prior === 0) return `prior period ${fmtMoney(0)}`;
  const pct = Math.round(((current - prior) / Math.abs(prior)) * 100);
  const arrow = pct >= 0 ? "▲" : "▼";
  return `${arrow} ${Math.abs(pct)}% vs prior period (${fmtMoneyCompact(prior)})`;
}

function fmtCompact(value: number): string {
  return value.toLocaleString("en-US", {
    notation: value >= 10_000 ? "compact" : "standard",
    maximumFractionDigits: value >= 10_000 ? 1 : 0,
  });
}

function fmtPercentBps(bps: number | null | undefined): string {
  if (bps === null || bps === undefined) return "n/a";
  return `${(bps / 100).toFixed(1)}%`;
}

function hasMetric(report: OwnerReport, id: string): boolean {
  const sections = report.metrics.metric_sections;
  return sections.length === 0 || sections.includes(id);
}

/** Same Label → Value → Comparison shape as the Accounting KPI cards. */
function KpiCard({
  label,
  value,
  valueCls,
  comparison,
  footnote,
  hero,
}: {
  label: string;
  value: string;
  valueCls?: string;
  comparison?: React.ReactNode;
  footnote?: string;
  hero?: boolean;
}) {
  return (
    <div
      className={`surface-card surface-flat surface-body-violet rounded-lg border bg-zinc-900/40 p-4 ${
        hero ? "border-emerald-700/60 ring-1 ring-inset ring-emerald-500/30" : "border-zinc-800"
      }`}
    >
      <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
        {label}
      </div>
      <div
        className={`mt-1 font-bold tabular-nums ${hero ? "text-3xl" : "text-2xl"} ${
          valueCls ?? "text-zinc-100"
        }`}
      >
        {value}
      </div>
      {comparison ? (
        <div className="mt-0.5 text-xs text-zinc-400">{comparison}</div>
      ) : null}
      {footnote ? (
        <div className="mt-1 text-[11px] leading-snug text-zinc-600">{footnote}</div>
      ) : null}
    </div>
  );
}

/** A metric we have NOT built yet — named, never silently absent. */
function PendingMetricCard({ label, note }: { label: string; note: string }) {
  return (
    <div className="surface-card surface-flat surface-body-violet rounded-lg border border-dashed border-zinc-800 bg-zinc-900/20 p-4">
      <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-zinc-600">
        {label}
        <span className="rounded-full border border-amber-900 bg-amber-950/40 px-1.5 py-0.5 text-[9px] font-semibold normal-case tracking-normal text-amber-400">
          coming soon
        </span>
      </div>
      <div className="mt-1 text-2xl font-bold text-zinc-700">—</div>
      <div className="mt-1 text-[11px] leading-snug text-zinc-600">{note}</div>
    </div>
  );
}

function PendingSetupCard({ label, note }: { label: string; note: string }) {
  return (
    <div className="surface-card surface-flat surface-body-violet rounded-lg border border-dashed border-zinc-800 bg-zinc-900/20 p-4">
      <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-zinc-600">
        {label}
        <span className="rounded-full border border-amber-900 bg-amber-950/40 px-1.5 py-0.5 text-[9px] font-semibold normal-case tracking-normal text-amber-400">
          pending setup
        </span>
      </div>
      <div className="mt-1 text-2xl font-bold text-zinc-700">—</div>
      <div className="mt-1 text-[11px] leading-snug text-zinc-600">{note}</div>
    </div>
  );
}

function TrafficReadinessCards({ traffic }: { traffic: DigestTrafficMetrics }) {
  return (
    <>
      {!traffic.behavior_configured ? (
        <PendingSetupCard
          label="Website behavior"
          note={
            traffic.behavior_pending_reason ??
            "GA4 behavior and acquisition data is not configured."
          }
        />
      ) : null}
      {!traffic.conversion_tracking_configured ? (
        <PendingSetupCard
          label="Conversions"
          note={
            traffic.conversion_tracking_pending_reason ??
            "GA4 conversion events are not configured."
          }
        />
      ) : null}
      {!traffic.retargeting_configured ? (
        <PendingSetupCard
          label="Retargeting"
          note={
            traffic.retargeting_pending_reason ??
            "Retargeting setup is outside BusinessOS writes."
          }
        />
      ) : null}
    </>
  );
}

function BehaviorAnalyticsCard({ traffic }: { traffic: DigestTrafficMetrics }) {
  if (!traffic.behavior_configured) return null;
  if (!traffic.behavior_has_data) {
    return (
      <PendingSetupCard
        label="Website behavior"
        note="GA4 is configured but no behavior snapshots are available for this period yet."
      />
    );
  }
  const landingPages = traffic.top_landing_pages_week ?? [];
  const sources = traffic.top_sources_week ?? [];
  const topPage = landingPages[0];
  const topSource = sources[0];
  const footnote = [
    `${fmtCompact(traffic.behavior_week.total_users)} users`,
    `${fmtCompact(traffic.behavior_week.conversions)} conversions`,
    topPage ? `top page ${topPage.value}` : null,
    topSource ? `top source ${topSource.value}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <KpiCard
      label="Website behavior"
      value={fmtCompact(traffic.behavior_week.sessions)}
      comparison={`${fmtCompact(traffic.behavior_month_to_date.sessions)} sessions MTD`}
      footnote={footnote}
    />
  );
}

function TrafficCard({ traffic }: { traffic: DigestTrafficMetrics }) {
  if (!traffic.configured) {
    return (
      <>
        <PendingMetricCard
          label="Organic search"
          note="Pending Search Console property/access configuration."
        />
        <BehaviorAnalyticsCard traffic={traffic} />
        <TrafficReadinessCards traffic={traffic} />
      </>
    );
  }
  if (!traffic.has_data) {
    return (
      <>
        <div className="surface-card surface-flat surface-body-violet rounded-lg border border-dashed border-zinc-800 bg-zinc-900/20 p-4">
          <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-zinc-600">
            Organic search
            <span className="rounded-full border border-amber-900 bg-amber-950/40 px-1.5 py-0.5 text-[9px] font-semibold normal-case tracking-normal text-amber-400">
              pending data
            </span>
          </div>
          <div className="mt-1 text-2xl font-bold text-zinc-700">—</div>
          <div className="mt-1 text-[11px] leading-snug text-zinc-600">
            Search Console is configured but no traffic snapshots are available for this period yet.
            Sync from Web analytics, then regenerate this report.
          </div>
        </div>
        <BehaviorAnalyticsCard traffic={traffic} />
        <TrafficReadinessCards traffic={traffic} />
      </>
    );
  }
  return (
    <>
      <KpiCard
        label="Organic search"
        value={fmtCompact(traffic.totals.clicks)}
        comparison={`${fmtCompact(traffic.branded.clicks)} branded · ${fmtCompact(
          traffic.nonbranded.clicks,
        )} non-branded`}
        footnote={`${fmtCompact(traffic.totals.impressions)} Search impressions${
          traffic.last_synced_at_ms ? ` · synced ${fmtAgo(Number(traffic.last_synced_at_ms))}` : ""
        }`}
      />
      <BehaviorAnalyticsCard traffic={traffic} />
      <TrafficReadinessCards traffic={traffic} />
    </>
  );
}

function DealMetricCard({ report }: { report: OwnerReport }) {
  const deals = report.metrics.deals;
  if (deals.status === "available") {
    return (
      <KpiCard
        label="Close rate"
        value={fmtPercentBps(deals.close_rate_bps)}
        comparison={`${deals.won_deals ?? 0} won · ${deals.lost_deals ?? 0} lost`}
        footnote={
          deals.avg_contact_to_close_days !== null &&
          deals.avg_contact_to_close_days !== undefined
            ? `Avg contact-to-close: ${deals.avg_contact_to_close_days} days (${deals.contact_to_close_sample ?? 0} deals with both dates).`
            : "Contact-to-close needs both started and closed date fields on the closed deals."
        }
      />
    );
  }
  const limited = deals.status === "limited_data";
  return (
    <div
      className={`surface-card surface-flat surface-body-violet rounded-lg border border-dashed p-4 ${
        limited
          ? "border-amber-900 bg-amber-950/20"
          : "border-zinc-800 bg-zinc-900/20"
      }`}
    >
      <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-zinc-600">
        {deals.source === "hubspot_deals" ? "Close rate" : "CRM business metrics"}
        <span
          className={`rounded-full border px-1.5 py-0.5 text-[9px] font-semibold normal-case tracking-normal ${
            limited
              ? "border-amber-900 bg-amber-950/40 text-amber-400"
              : "border-zinc-800 bg-zinc-950 text-zinc-500"
          }`}
        >
          {limited ? "limited" : "setup needed"}
        </span>
      </div>
      <div className="mt-1 text-2xl font-bold text-zinc-700">—</div>
      <div className="mt-1 text-[11px] leading-snug text-zinc-600">
        {deals.message ||
          (limited
            ? "The configured CRM returned limited business metric data."
            : "Configure the CRM business metric mapping to enable this report.")}
      </div>
    </div>
  );
}

function EmailStatus({ entry }: { entry: OwnerReportWithRevision }) {
  if (!entry.outbox_job) return null;
  const job = entry.outbox_job;
  return (
    <div className="text-xs">
      {job.status === "pending" ? (
        <span className="text-sky-300">
          Gmail draft is queued — we&apos;ll create it shortly.
        </span>
      ) : job.status === "delivered" ? (
        job.dry_run ? (
          <span className="text-amber-300">
            Tested successfully, but live Gmail drafts are turned off — ask
            your administrator to enable them.
          </span>
        ) : (
          <span className="text-emerald-300">
            Digest draft created in Gmail — open Gmail to review and send to
            the owners.
          </span>
        )
      ) : (
        <span className="text-red-300">
          Couldn&apos;t send the digest — try again or contact your administrator.
        </span>
      )}
    </div>
  );
}

function ReportSection({
  entry,
  onEmail,
  emailBusy,
}: {
  entry: OwnerReportWithRevision;
  onEmail: (entry: OwnerReportWithRevision) => void;
  emailBusy: boolean;
}) {
  const report = entry.report;
  const metrics = report.metrics;
  const sales = metrics.sales;
  const invoiceBasis = sales.basis === "invoice_totals";
  const showBaselineMetric = sales.metric_basis !== "gross_margin";
  const metricDelta = sales.metric_above_baseline_cents ?? null;
  const metricBaseline = sales.metric_baseline_cents ?? null;
  const metricLabel = sales.metric_basis_label || "Financial metric";
  const showSales = hasMetric(report, "sales");
  const showCalls = hasMetric(report, "calls");
  const showFollowUps = hasMetric(report, "follow_ups");
  const showInventory = hasMetric(report, "inventory");
  const showOrders = hasMetric(report, "orders");
  const showClaims = hasMetric(report, "damage_claims");
  const showSiteTraffic = hasMetric(report, "site_traffic");
  const showDeals = hasMetric(report, "close_rate");
  return (
    <section className="flex flex-col gap-3">
      <div className="surface-section-head surface-head-violet flex items-center justify-between">
        <h3 className="text-sm font-semibold text-zinc-200">{periodTitle(report)}</h3>
        <div className="flex items-center gap-3">
          <span className="text-xs text-zinc-500">
            assembled {fmtAgo(Number(report.generated_at_ms))}
          </span>
          <button
            onClick={() => onEmail(entry)}
            disabled={emailBusy || entry.report.outbox_job_id !== null}
            className="rounded-md border border-zinc-700 px-2.5 py-1 text-xs text-zinc-300 hover:bg-zinc-800 disabled:opacity-40"
            title="Creates a Gmail draft of this digest addressed to the report recipients."
          >
            {entry.report.outbox_job_id !== null ? "Email staged" : "Email to owners"}
          </button>
        </div>
      </div>

      <EmailStatus entry={entry} />

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        {showSales ? (
          <KpiCard
            label={report.period_kind === "weekly" ? "Sales this week" : "Sales this month"}
            value={fmtMoneyCompact(sales.period_sales_cents)}
            comparison={comparisonText(sales.period_sales_cents, sales.prior_period_sales_cents)}
            footnote={
              invoiceBasis
                ? "Invoice totals — counts invoices only (no sales receipts or credit notes)."
                : "From QuickBooks P&L."
            }
            hero
          />
        ) : null}
        {showSales && showBaselineMetric && metricDelta !== null ? (
          <KpiCard
            label={`${metricLabel} above baseline`}
            value={fmtMoneyCompact(metricDelta)}
            valueCls={metricDelta >= 0 ? "text-emerald-300" : "text-red-300"}
            comparison={
              metricBaseline !== null &&
              metricBaseline !== undefined
                ? `baseline ${fmtMoneyCompact(metricBaseline)}/mo`
                : undefined
            }
            footnote={
              sales.metric_basis === "gross_margin"
                ? "Average monthly margin of the prior four completed quarters."
                : "Configured financial metric basis."
            }
          />
        ) : showSales && showBaselineMetric ? (
          <KpiCard
            label={`${metricLabel} above baseline`}
            value="—"
            valueCls="text-zinc-600"
            footnote={
              sales.metric_pending_reason ??
              (invoiceBasis
                ? "Financial metric needs provider data or an imported baseline."
                : `Appears once all 12 baseline months sync (${sales.baseline_months_cached}/12 cached).`)
            }
          />
        ) : null}
        {showCalls ? (
          <KpiCard
            label={metrics.calls.label}
            value={
              metrics.calls.configured
                ? String(metrics.calls.call_log_messages)
                : "Pending"
            }
            valueCls={metrics.calls.configured ? undefined : "text-zinc-600"}
            footnote={
              metrics.calls.configured
                ? metrics.calls.source_label
                : (metrics.calls.pending_reason ?? "Call-volume source is not configured.")
            }
          />
        ) : null}
        {showFollowUps ? (
          <KpiCard
            label="Follow-ups completed"
            value={String(metrics.follow_ups.done_in_period)}
            comparison={`${metrics.follow_ups.open} open · ${metrics.follow_ups.due_today} due today`}
            footnote={
              metrics.follow_ups.overdue > 0
                ? `${metrics.follow_ups.overdue} overdue (${metrics.follow_ups.escalated} escalated, ${metrics.follow_ups.critical} critical)`
                : "Nothing overdue."
            }
          />
        ) : null}
        {showOrders && metrics.orders.configured ? (
          <KpiCard
            label="Orders in period"
            value={String(metrics.orders.orders_in_period)}
            footnote={`Backlog now: ${metrics.orders.exceptions} exceptions · ${metrics.orders.deduction_failed} deduction failed · ${metrics.orders.needs_mapping} need SKU mapping · ${metrics.orders.packed_missing_photo} packed w/o photo · ${metrics.orders.blocked} blocked`}
          />
        ) : null}
        {showClaims && metrics.claims.configured ? (
          <>
            <KpiCard
              label="Damage events"
              value={String(metrics.claims.damage_events_in_period)}
              comparison={
                `${metrics.claims.damage_open} open · ${metrics.claims.damage_resolved} resolved`
              }
              footnote={
                metrics.claims.damage_by_type.length > 0
                  ? metrics.claims.damage_by_type
                      .slice(0, 3)
                      .map((s) => `${s.count} ${s.damage_type}`)
                      .join(" · ")
                  : metrics.claims.damage_by_severity.length > 0
                    ? metrics.claims.damage_by_severity
                        .map((s) => `${s.count} ${s.severity}`)
                        .join(" · ")
                    : "No damage patterns in this period."
              }
            />
            <KpiCard
              label="Damage queue"
              value={String(
                metrics.claims.queue_open +
                  metrics.claims.queue_accepted +
                  metrics.claims.queue_dismissed,
              )}
              comparison={`${metrics.claims.queue_open} open · ${metrics.claims.queue_accepted} accepted`}
              footnote={`${metrics.claims.queue_dismissed} dismissed · claim packets: ${metrics.claims.claims_drafted_in_period} drafted, ${metrics.claims.claims_approved_in_period} approved`}
            />
          </>
        ) : null}
        {showSiteTraffic ? <TrafficCard traffic={metrics.traffic} /> : null}
        {showDeals ? <DealMetricCard report={report} /> : null}
      </div>

      {showInventory ? (
        <div className="surface-card surface-flat surface-body-violet rounded-lg border border-zinc-800 p-3">
          <div className="mb-3 text-xs font-semibold uppercase tracking-wide text-zinc-400">
            Inventory
          </div>
          {metrics.inventory.configured ? (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
              <KpiCard
                label="Stocked SKUs"
                value={String(metrics.inventory.stocked_sku_count)}
                comparison={`${metrics.inventory.out_of_stock_count} out · ${metrics.inventory.critical_count} critical`}
                footnote="Same stocked-item rules as the Inventory report."
              />
              <KpiCard
                label="Stocked valuation"
                value={fmtMoneyCompact(metrics.inventory.stock_value_cents)}
                footnote="Stocked materials only, at cached unit cost."
              />
              <KpiCard
                label="Inbound on open POs"
                value={fmtMoneyCompact(metrics.inventory.inbound_open_po_cents)}
                footnote="Purchase orders not received or cancelled."
              />
            </div>
          ) : (
            <PendingMetricCard
              label="Inventory"
              note={metrics.inventory.pending_reason ?? "Inventory reporting is not configured."}
            />
          )}
        </div>
      ) : null}

    </section>
  );
}

export default function Reports({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
}) {
  const [reports, setReports] = useState<OwnerReportWithRevision[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [generateBusy, setGenerateBusy] = useState(false);
  const [emailBusy, setEmailBusy] = useState(false);
  // Set after Generate-now: fast-poll until a digest newer than this lands.
  const generatingSince = useRef<number | null>(null);
  const [generating, setGenerating] = useState(false);

  const load = useCallback(async () => {
    try {
      const res = await api.ownerReports();
      setReports(res.reports);
      setError(null);
      if (generatingSince.current !== null) {
        const newest = Math.max(
          0,
          ...res.reports.map((entry) => Number(entry.report.generated_at_ms)),
        );
        if (
          newest >= generatingSince.current ||
          Date.now() - generatingSince.current > GENERATING_TIMEOUT_MS
        ) {
          generatingSince.current = null;
          setGenerating(false);
        }
      }
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useAppCommand("refresh", () => void load());

  usePolling(load, {
    intervalMs: generating ? GENERATING_POLL_INTERVAL_MS : POLL_INTERVAL_MS,
  });

  const generateNow = async () => {
    setGenerateBusy(true);
    setNotice(null);
    try {
      const res = await api.ownerReportsGenerate();
      if (res.accepted) {
        generatingSince.current = Date.now();
        setGenerating(true);
      } else {
        setNotice("A digest generation just ran — give it a minute and retry.");
      }
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(errorMessage(err));
    } finally {
      setGenerateBusy(false);
    }
  };

  const emailReport = async (entry: OwnerReportWithRevision) => {
    setEmailBusy(true);
    setNotice(null);
    try {
      await api.emailOwnerReport(entry.report.report_id, {
        idempotency_key: crypto.randomUUID(),
        expected_revision: entry.revision,
        actor_id: null,
      });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (err instanceof ApiError && err.code === "owner_report_to_addr_unset") {
        setNotice(
          "No owner email address configured — ask your administrator to add the report recipients.",
        );
      } else setNotice(errorMessage(err));
    } finally {
      setEmailBusy(false);
    }
  };

  if (reports === null && error === null) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Couldn&apos;t load the reports view: {error}
      </div>
    );
  }

  // Show the newest weekly + newest MTD digest (history stays via the API).
  const weekly = reports?.find((entry) => entry.report.period_kind === "weekly") ?? null;
  const mtd = reports?.find((entry) => entry.report.period_kind === "mtd") ?? null;

  return (
    <div className="flex flex-col gap-6">
      <div className="surface-section-head surface-head-violet flex flex-wrap items-center justify-between">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <h2 className="shrink-0 whitespace-nowrap text-lg font-semibold text-zinc-100">
            Owner reports
          </h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Reports"
          />
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-3">
          <button
            onClick={() => void generateNow()}
            disabled={generateBusy || generating}
            className="rounded-md border border-zinc-700 px-2.5 py-1 text-xs text-zinc-300 hover:bg-zinc-800 disabled:opacity-40"
          >
            {generating ? "Generating…" : "Generate now"}
          </button>
        </div>
      </div>

      {notice ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-300">
          {notice}
        </div>
      ) : null}

      {!weekly && !mtd ? (
        <div className="surface-card surface-flat surface-body-violet mx-auto mt-12 max-w-md rounded-lg border border-dashed border-zinc-800 bg-zinc-900/40 p-8 text-center">
          <h3 className="text-lg font-semibold text-zinc-100">No digest yet</h3>
          <p className="mt-2 text-sm text-zinc-400">
            Generate the first weekly + month-to-date owner digest from the
            data already synced (accounting, calls, tasks, orders, claims).
            Scheduled reports can be set up by your administrator.
          </p>
          <button
            onClick={() => void generateNow()}
            disabled={generateBusy || generating}
            className="mt-4 rounded-md bg-[var(--accent)] px-4 py-2 text-sm font-semibold text-white hover:bg-[var(--accent-hover)] disabled:opacity-40"
          >
            {generating ? "Generating…" : "Generate digest"}
          </button>
        </div>
      ) : (
        <>
          {weekly ? (
            <ReportSection entry={weekly} onEmail={(e) => void emailReport(e)} emailBusy={emailBusy} />
          ) : null}
          {mtd ? (
            <ReportSection entry={mtd} onEmail={(e) => void emailReport(e)} emailBusy={emailBusy} />
          ) : null}
        </>
      )}
    </div>
  );
}
