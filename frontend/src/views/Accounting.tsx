import { useCallback, useEffect, useRef, useState } from "react";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import type { AccountingAgingResponse } from "../types/generated/AccountingAgingResponse";
import type { AccountingConnectorStatus } from "../types/generated/AccountingConnectorStatus";
import type { AccountingCustomersResponse } from "../types/generated/AccountingCustomersResponse";
import type { AccountingFinancialsResponse } from "../types/generated/AccountingFinancialsResponse";
import type { AccountingInvoiceRow } from "../types/generated/AccountingInvoiceRow";
import type { AccountingInvoicesResponse } from "../types/generated/AccountingInvoicesResponse";
import type { AccountingSyncInfo } from "../types/generated/AccountingSyncInfo";
import type { CustomerTierSyncRun } from "../types/generated/CustomerTierSyncRun";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import SectionHelpButton from "../components/SectionHelpButton";
import {
  Button,
  EmptyState,
  KpiCard,
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
/** Faster while a sync runs so fresh numbers appear as they land. */
const SYNCING_POLL_INTERVAL_MS = 5_000;
const STALE_AFTER_MS = 24 * 60 * 60 * 1000;
const TABLE_PREVIEW_ROWS = 10;

/** AR bucket id → table filter + severity color (QBO/Xero convention). */
const BUCKETS: { id: string; color: string }[] = [
  { id: "current", color: "bg-zinc-500" },
  { id: "days_1_30", color: "bg-yellow-500" },
  { id: "days_31_60", color: "bg-amber-500" },
  { id: "days_61_90", color: "bg-orange-500" },
  { id: "days_90_plus", color: "bg-red-500" },
  { id: "no_due_date", color: "bg-zinc-700" },
];

function bucketColor(id: string): string {
  return BUCKETS.find((bucket) => bucket.id === id)?.color ?? "bg-zinc-600";
}

export function accountingInvoiceMatchesBucket(
  row: AccountingInvoiceRow,
  bucket: string,
): boolean {
  const open = row.status === "open" || row.status === "overdue";
  if (!open) return false;
  switch (bucket) {
    case "current":
      return row.due_date !== null && row.days_overdue <= 0;
    case "days_1_30":
      return row.days_overdue >= 1 && row.days_overdue <= 30;
    case "days_31_60":
      return row.days_overdue >= 31 && row.days_overdue <= 60;
    case "days_61_90":
      return row.days_overdue >= 61 && row.days_overdue <= 90;
    case "days_90_plus":
      return row.days_overdue > 90;
    case "no_due_date":
      return row.due_date === null;
    default:
      return false;
  }
}

function fmtMoney(cents: number): string {
  return (cents / 100).toLocaleString("en-US", {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
  });
}

/** Hero-card money: $52.3K-style, exact cents only in tables. */
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
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function providerLabel(provider: string | undefined): string {
  if (provider === "invoice_ninja") return "Invoice Ninja";
  if (provider === "stripe") return "Stripe";
  return "QuickBooks";
}

export function AccountingReconnectNotice({
  status,
}: {
  status: AccountingConnectorStatus;
}) {
  if (!status.reconnect_required) return null;
  const provider = providerLabel(status.provider);
  return (
    <div className="flex flex-col gap-3 rounded-md border border-amber-700/60 bg-amber-950/40 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
      <div>
        <div className="text-sm font-semibold text-amber-200">
          {provider} needs to be reconnected.
        </div>
        <div className="mt-1 text-xs text-amber-300">
          {provider} rejected the saved authorization. Reconnect to resume updates.
          Cached numbers remain available.
        </div>
      </div>
      {status.connect_url ? (
        <a
          href={status.connect_url}
          className="inline-flex shrink-0 items-center justify-center rounded-md bg-amber-600 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-amber-500 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/70"
        >
          Reconnect {provider}
        </a>
      ) : (
        <span className="text-xs text-amber-300">
          Ask an administrator to enable the OAuth connection.
        </span>
      )}
    </div>
  );
}

function outboxLabel(run: CustomerTierSyncRun): string | null {
  if (run.status === "staged") return null;
  const job = run.outbox_job;
  if (!job) return "queued";
  if (job.status === "delivered" && job.dry_run) return "dry-run delivered";
  if (job.status === "delivered") return "delivered";
  if (job.status === "failed_terminal") return "failed";
  return job.status;
}

export function CustomerTierSyncPanel({
  run,
  busy,
  onPreview,
  onApprove,
  onReject,
}: {
  run: CustomerTierSyncRun | null;
  busy: boolean;
  onPreview: () => void;
  onApprove: (run: CustomerTierSyncRun) => void;
  onReject: (run: CustomerTierSyncRun) => void;
}) {
  const actionCount = run?.plan.actions.length ?? 0;
  const skippedCount = run?.plan.skipped.length ?? 0;
  const statusTone =
    run?.status === "approved"
      ? "ok"
      : run?.status === "rejected"
        ? "neutral"
        : "info";
  const delivery = run ? outboxLabel(run) : null;

  return (
    <div className="border-t border-zinc-800 bg-zinc-950/30 px-3 py-3">
      <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <div className="text-sm font-semibold text-zinc-200">
              Shopify tier sync
            </div>
            {run ? (
              <StatusBadge tone={statusTone}>{run.status}</StatusBadge>
            ) : null}
            {delivery ? <StatusBadge tone="neutral">{delivery}</StatusBadge> : null}
          </div>
          <div className="mt-1 text-xs text-zinc-400">
            {run
              ? `${actionCount} mapped customer${actionCount === 1 ? "" : "s"} · ${skippedCount} skipped · ${run.plan.mapping_version}`
              : "Preview builds a reviewed plan from cached QuickBooks customer tiers."}
          </div>
          {run?.outbox_job?.last_error ? (
            <div className="mt-1 text-xs text-red-300">
              {run.outbox_job.last_error}
            </div>
          ) : null}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" onClick={onPreview} busy={busy}>
            Preview
          </Button>
          <Button
            size="sm"
            variant="success"
            disabled={!run || run.status !== "staged" || actionCount === 0}
            busy={busy}
            onClick={() => run && onApprove(run)}
          >
            Approve
          </Button>
          <Button
            size="sm"
            variant="ghost"
            disabled={!run || run.status !== "staged"}
            busy={busy}
            onClick={() => run && onReject(run)}
          >
            Reject
          </Button>
        </div>
      </div>
    </div>
  );
}

const MONTH_NAMES = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

function monthName(dateStr: string | null | undefined): string {
  if (!dateStr) return "last month";
  const month = Number(dateStr.slice(5, 7));
  return MONTH_NAMES[month - 1] ?? "last month";
}

/** "12 days overdue" / "due in 5 days" / "due today" — never raw dates. */
export function accountingDueLabel(
  row: AccountingInvoiceRow,
): { text: string; cls: string } {
  if (!row.due_date) return { text: "no due date", cls: "text-zinc-400" };
  if (row.days_overdue > 0) {
    return {
      text: `${row.days_overdue} day${row.days_overdue === 1 ? "" : "s"} overdue`,
      cls: "text-red-400",
    };
  }
  const due = new Date(`${row.due_date}T12:00:00`);
  const days = Math.round((due.getTime() - Date.now()) / 86_400_000);
  if (days <= 0) return { text: "due today", cls: "text-amber-300" };
  return { text: `due in ${days} day${days === 1 ? "" : "s"}`, cls: "text-zinc-400" };
}

/**
 * Monthly trend bars (pure CSS). With P&L data: gross margin per month with
 * a dashed baseline overlay. Without (invoice_totals): sales per month.
 */
export function AccountingMarginTrend({
  financials,
}: {
  financials: AccountingFinancialsResponse;
}) {
  const months = financials.months;
  const hasMargin = financials.metric_basis === "gross_margin" && financials.basis === "quickbooks_pnl";
  const barValue = (m: AccountingFinancialsResponse["months"][number]) =>
    hasMargin ? (m.gross_profit_cents ?? 0) : m.total_income_cents;
  if (months.length === 0) return null;
  const max = Math.max(
    ...months.map(barValue),
    hasMargin ? (financials.metric_baseline_cents ?? 0) : 0,
    1,
  );
  const baselinePct =
    hasMargin &&
    financials.metric_baseline_cents !== null &&
    financials.metric_baseline_cents !== undefined
      ? (financials.metric_baseline_cents / max) * 100
      : null;
  return (
    <div className="surface-card surface-flat surface-body-emerald rounded-lg border border-zinc-800 bg-zinc-900/40 p-4 lg:col-span-3">
      <div className="flex items-baseline justify-between">
        <div className="text-xs font-semibold uppercase tracking-wide text-zinc-400">
          {hasMargin ? "Gross margin by month" : "Sales by month"}
        </div>
        {baselinePct !== null ? (
          <div className="text-xs text-zinc-400">
            baseline {fmtMoneyCompact(financials.metric_baseline_cents ?? 0)}/mo
          </div>
        ) : null}
      </div>
      <div className="relative mt-3">
        <div className="flex h-36 items-end gap-1">
          {months.map((month) => {
            const value = barValue(month);
            const pct = Math.max((Math.max(value, 0) / max) * 100, 1);
            const marginPct =
              hasMargin && month.total_income_cents > 0
                ? Math.round(
                    ((month.gross_profit_cents ?? 0) / month.total_income_cents) * 100,
                  )
                : null;
            const title = `${monthName(month.month_start)} ${month.month_start.slice(0, 4)}${
              month.is_complete ? "" : " (so far)"
            } · Sales ${fmtMoney(month.total_income_cents)}${
              marginPct !== null
                ? ` · Margin ${fmtMoney(month.gross_profit_cents ?? 0)} (${marginPct}%)`
                : ""
            }`;
            return (
              <div
                key={month.month_start}
                className={`flex-1 rounded-t ${
                  month.is_complete ? "bg-emerald-500/70" : "bg-emerald-500/30"
                }`}
                style={{ height: `${pct}%` }}
                title={title}
              />
            );
          })}
        </div>
        {baselinePct !== null ? (
          <div
            className="pointer-events-none absolute inset-x-0 border-t border-dashed border-zinc-400/70"
            style={{ bottom: `${Math.min(baselinePct, 100)}%` }}
          />
        ) : null}
      </div>
      <div className="mt-1 flex gap-1">
        {months.map((month, index) => (
          <div key={month.month_start} className="flex-1 text-center text-xs text-zinc-500">
            {index % 2 === 0 ? monthName(month.month_start).slice(0, 1) : ""}
          </div>
        ))}
      </div>
      {hasMargin && baselinePct === null ? (
        <div className="mt-2 text-xs text-zinc-400">
          Baseline appears once {12 - financials.baseline_months_cached} more month
          {12 - financials.baseline_months_cached === 1 ? "" : "s"} of history syncs.
        </div>
      ) : null}
    </div>
  );
}

export default function Accounting({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  tierSyncEnabled,
  focusAccountingId,
  onFocusAccountingConsumed,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  tierSyncEnabled: boolean;
  focusAccountingId?: string | null;
  onFocusAccountingConsumed?: () => void;
}) {
  const [status, setStatus] = useState<AccountingConnectorStatus | null>(null);
  const [financials, setFinancials] = useState<AccountingFinancialsResponse | null>(null);
  const [aging, setAging] = useState<AccountingAgingResponse | null>(null);
  const [invoices, setInvoices] = useState<AccountingInvoicesResponse | null>(null);
  const [customersRes, setCustomersRes] = useState<AccountingCustomersResponse | null>(null);
  const [tierSyncRun, setTierSyncRun] = useState<CustomerTierSyncRun | null>(null);
  const [bucketFilter, setBucketFilter] = useState<string | null>(null);
  const [showAllRows, setShowAllRows] = useState(false);
  const [showCustomers, setShowCustomers] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const [tierSyncBusy, setTierSyncBusy] = useState(false);
  const invoiceSectionRef = useRef<HTMLDivElement | null>(null);

  const load = useCallback(async () => {
    try {
      const connector = await api.accountingStatus();
      setStatus(connector);
      if (connector.connected) {
        const [fin, agingRes, invoicesRes, customers, tierSyncRuns] = await Promise.all([
          api.accountingFinancials(),
          api.accountingAging(),
          api.accountingInvoices("all"),
          api.accountingCustomers(),
          tierSyncEnabled
            ? api.customerTierSyncRuns()
            : Promise.resolve({ runs: [] }),
        ]);
        setFinancials(fin);
        setAging(agingRes);
        setInvoices(invoicesRes);
        setCustomersRes(customers);
        setTierSyncRun(tierSyncRuns.runs[0] ?? null);
      }
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setLoaded(true);
    }
  }, [onUnauthorized, tierSyncEnabled]);

  const sync: AccountingSyncInfo | null = financials?.sync ?? invoices?.sync ?? null;
  const syncing = sync?.in_flight ?? false;

  usePolling(load, {
    intervalMs: syncing ? SYNCING_POLL_INTERVAL_MS : POLL_INTERVAL_MS,
  });

  const syncNow = async () => {
    if (status?.reconnect_required) {
      setNotice(`${providerLabel(status.provider)} must be reconnected before updates can resume.`);
      return;
    }
    setSyncBusy(true);
    setNotice(null);
    try {
      await api.accountingSyncNow();
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice("Refresh already running or just finished — give it a minute.");
    } finally {
      setSyncBusy(false);
    }
  };

  const previewTierSync = async () => {
    setTierSyncBusy(true);
    setNotice(null);
    try {
      const run = await api.customerTierSyncPreview({
        idempotency_key: crypto.randomUUID(),
      });
      setTierSyncRun(run);
      setShowCustomers(true);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(errorMessage(err));
    } finally {
      setTierSyncBusy(false);
    }
  };

  const runTierSyncAction = async (
    run: CustomerTierSyncRun,
    action: "approve" | "reject",
  ) => {
    setTierSyncBusy(true);
    setNotice(null);
    try {
      const body = {
        idempotency_key: crypto.randomUUID(),
        expected_revision: run.revision,
      };
      if (action === "approve") await api.customerTierSyncApprove(run.run_id, body);
      else await api.customerTierSyncReject(run.run_id, body);
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(errorMessage(err));
    } finally {
      setTierSyncBusy(false);
    }
  };

  // Command palette integrations — must come before any early return.
  useAppCommand("refresh", () => void load());
  useAppCommand("accounting.sync", () => void syncNow());

  useEffect(() => {
    if (!focusAccountingId || !loaded) return;
    if (focusAccountingId === "invoices") {
      requestAnimationFrame(() => {
        invoiceSectionRef.current?.scrollIntoView({ block: "center" });
      });
      onFocusAccountingConsumed?.();
      return;
    }
    onFocusAccountingConsumed?.();
  }, [focusAccountingId, loaded, onFocusAccountingConsumed]);

  if (!loaded) {
    return (
      <div className="flex flex-col gap-4">
        <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
          {Array.from({ length: 4 }).map((_, i) => (
            <div key={i} className="h-24 animate-pulse rounded-lg border border-zinc-800 bg-zinc-900/40" />
          ))}
        </div>
        <div className={tableWrapCls}>
          <table className={tableCls}>
            <tbody className={rowDivideCls}>
              <SkeletonRows rows={8} cols={5} />
            </tbody>
          </table>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Couldn&apos;t load the accounting view: {error}
      </div>
    );
  }

  if (status && !status.connected) {
    return (
      <div className="mx-auto mt-12 max-w-md">
        <EmptyState
          title={`${providerLabel(status.provider)} isn't connected`}
          action={
            status.connect_url ? (
              <a
                href={status.connect_url}
                className="inline-flex items-center justify-center rounded-md bg-[var(--accent)] px-3 py-1.5 text-sm font-medium text-white transition hover:bg-[var(--accent-hover)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
              >
                Connect {providerLabel(status.provider)}
              </a>
            ) : status.blocked_reason ? (
              <p className="text-xs text-amber-300">{status.blocked_reason}</p>
            ) : undefined
          }
        >
          Connect the company accounting system to see sales and unpaid invoices here.
        </EmptyState>
      </div>
    );
  }

  const lastSynced = sync?.last_synced_at_ms ?? null;
  const stale = lastSynced !== null && Date.now() - lastSynced > STALE_AFTER_MS;
  const showMetricCard =
    financials !== null &&
    financials.metric_basis !== "gross_margin" &&
    (financials.metric_basis !== "invoice_totals" ||
      financials.metric_baseline_cents != null ||
      financials.metric_pending_reason != null);
  const provider = providerLabel(status?.provider);
  const reconnectRequired = status?.reconnect_required ?? false;
  const metricAboveBaseline = financials?.metric_above_baseline_cents ?? null;
  const metricValue = financials?.metric_value_cents ?? null;
  const metricBaseline = financials?.metric_baseline_cents ?? null;
  const metricLabel = financials?.metric_basis_label || "Financial metric";
  const overdueCents = (aging?.buckets ?? [])
    .filter((bucket) => bucket.bucket !== "current" && bucket.bucket !== "no_due_date")
    .reduce((total, bucket) => total + bucket.balance_cents, 0);
  const overdueCount = (invoices?.invoices ?? []).filter(
    (row) => row.status === "overdue",
  ).length;
  const agingTotal = aging?.total_open_cents ?? 0;

  const openRows = (invoices?.invoices ?? [])
    .filter((row) =>
      bucketFilter
        ? accountingInvoiceMatchesBucket(row, bucketFilter)
        : row.status === "open" || row.status === "overdue",
    )
    .sort((a, b) => b.days_overdue - a.days_overdue);
  const visibleRows = showAllRows ? openRows : openRows.slice(0, TABLE_PREVIEW_ROWS);

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-emerald flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">Accounting</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Accounting"
          />
        </div>
        <div className="flex items-center gap-3">
          {status?.environment === "sandbox" ? (
            <StatusBadge tone="warning">test company</StatusBadge>
          ) : null}
          <span
            className={`text-xs ${stale ? "text-amber-300" : "text-zinc-400"}`}
            title={`${sync?.invoice_count ?? 0} invoices, ${sync?.customer_count ?? 0} customers on file`}
          >
            {syncing
              ? `Updating from ${provider}…`
              : `Updated ${fmtAgo(lastSynced)} from ${provider}`}
          </span>
          <Button
            variant="secondary"
            size="sm"
            busy={syncBusy}
            disabled={syncBusy || syncing || reconnectRequired}
            onClick={() => void syncNow()}
          >
            {reconnectRequired
              ? "Reconnect required"
              : syncBusy
                ? "Syncing…"
                : syncing
                  ? "Updating…"
                  : "Refresh"}
          </Button>
        </div>
      </div>

      {reconnectRequired ? (
        <AccountingReconnectNotice status={status!} />
      ) : null}

      {notice ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-300">
          {notice}
        </div>
      ) : null}
      {sync?.last_error && !reconnectRequired ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-300">
          The last accounting refresh hit a problem; numbers may be behind.
          Try Refresh in a couple of minutes.
        </div>
      ) : null}

      <div
        className={`grid grid-cols-2 gap-4 ${
          showMetricCard ? "lg:grid-cols-4" : "lg:grid-cols-3"
        }`}
      >
        {showMetricCard ? (
        <KpiCard
          hero
          tone={metricAboveBaseline !== null && metricAboveBaseline >= 0 ? "ok" : metricAboveBaseline !== null ? "critical" : undefined}
          label={`${metricLabel} above baseline · ${monthName(
            financials?.months.at(-1)?.month_start ?? null,
          )} to date`}
          value={
            metricAboveBaseline !== null
              ? `${metricAboveBaseline >= 0 ? "+" : ""}${fmtMoneyCompact(metricAboveBaseline)}`
              : "Pending"
          }
          valueCls={
            metricAboveBaseline === null
              ? "text-zinc-400"
              : metricAboveBaseline >= 0
                ? "text-emerald-300"
                : "text-red-300"
          }
          comparison={
            metricValue !== null && metricBaseline !== null ? (
              <>
                {metricLabel} {fmtMoneyCompact(metricValue)} vs baseline{" "}
                {fmtMoneyCompact(metricBaseline)}/mo
              </>
            ) : (
              (financials?.metric_pending_reason ?? "Metric inputs are not complete.")
            )
          }
          footnote={
            financials?.metric_basis === "gross_margin"
              ? "Baseline = average monthly margin, last 4 completed quarters"
              : "Configured financial metric basis"
          }
        />
        ) : null}
        <KpiCard
          label="Sales this week"
          value={fmtMoneyCompact(financials?.week_to_date_cents ?? 0)}
          comparison={
            financials?.prior_week_to_date_cents !== null &&
            financials?.prior_week_to_date_cents !== undefined
              ? `Prior WTD: ${fmtMoneyCompact(financials.prior_week_to_date_cents)}`
              : "Prior week-to-date not on file yet"
          }
        />
        <KpiCard
          label="Sales this month"
          value={fmtMoneyCompact(financials?.month_to_date_cents ?? 0)}
          comparison={
            financials?.prior_month_to_date_cents !== null &&
            financials?.prior_month_to_date_cents !== undefined
              ? `${monthName(
                  financials.months.at(-2)?.month_start ?? null,
                )} MTD: ${fmtMoneyCompact(financials.prior_month_to_date_cents)}`
              : "Prior month-to-date not on file yet"
          }
        />
        <KpiCard
          label="Owed to you"
          value={fmtMoneyCompact(agingTotal)}
          comparison={
            overdueCents > 0 ? (
              <span className="text-amber-300">
                {fmtMoneyCompact(overdueCents)} overdue · {overdueCount} invoice
                {overdueCount === 1 ? "" : "s"}
              </span>
            ) : (
              "Nothing overdue — nice."
            )
          }
        />
      </div>

      <div className="grid gap-4 lg:grid-cols-5">
        {financials ? <AccountingMarginTrend financials={financials} /> : null}

        <div className="surface-card surface-flat surface-body-emerald rounded-lg border border-zinc-800 bg-zinc-900/40 p-4 lg:col-span-2">
          <div className="text-sm font-semibold text-zinc-200">
            Unpaid invoices by age
          </div>
          <div className="mt-2 flex items-baseline gap-4">
            <div>
              <span className="text-lg font-bold tabular-nums text-zinc-100">
                {fmtMoneyCompact(agingTotal)}
              </span>
              <span className="ml-1.5 text-xs text-zinc-400">open</span>
            </div>
            <div>
              <span className="text-lg font-bold tabular-nums text-amber-300">
                {fmtMoneyCompact(overdueCents)}
              </span>
              <span className="ml-1.5 text-xs text-zinc-400">overdue</span>
            </div>
          </div>
          {agingTotal > 0 && aging ? (
            <>
              <div className="mt-3 flex h-3 w-full overflow-hidden rounded-full bg-zinc-800">
                {aging.buckets
                  .filter((bucket) => bucket.balance_cents > 0)
                  .map((bucket) => (
                    <div
                      key={bucket.bucket}
                      className={bucketColor(bucket.bucket)}
                      style={{ width: `${(bucket.balance_cents / agingTotal) * 100}%` }}
                      title={`${bucket.label}: ${fmtMoney(bucket.balance_cents)}`}
                    />
                  ))}
              </div>
              <div className="mt-3 flex flex-col gap-1.5">
                {aging.buckets
                  .filter((bucket) => bucket.invoice_count > 0)
                  .map((bucket) => (
                    <button
                      key={bucket.bucket}
                      onClick={() => {
                        setBucketFilter((current) =>
                          current === bucket.bucket ? null : bucket.bucket,
                        );
                        setShowAllRows(false);
                      }}
                      className={`flex items-center gap-2 rounded px-1.5 py-0.5 text-left text-xs hover:bg-zinc-800/60 ${
                        bucketFilter === bucket.bucket ? "bg-zinc-800" : ""
                      }`}
                      title="Show these invoices below"
                    >
                      <span className={`h-2 w-2 shrink-0 rounded-full ${bucketColor(bucket.bucket)}`} />
                      <span className="flex-1 text-zinc-300">{bucket.label}</span>
                      <span className="tabular-nums text-zinc-200">
                        {fmtMoney(bucket.balance_cents)}
                      </span>
                      <span className="w-8 text-right text-zinc-400">
                        {bucket.invoice_count}
                      </span>
                    </button>
                  ))}
              </div>
            </>
          ) : (
            <p className="mt-3 text-sm text-zinc-400">No unpaid invoices on file.</p>
          )}
        </div>
      </div>

      <div className="surface-card surface-flat surface-body-emerald rounded-lg border border-zinc-800">
        <div ref={invoiceSectionRef} />
        <div className="surface-head-emerald flex items-center gap-2 border-b border-zinc-800 px-3 py-2">
          <span className="text-sm font-semibold text-zinc-200">Open invoices</span>
          {bucketFilter ? (
            <button
              onClick={() => setBucketFilter(null)}
              className="rounded-full bg-zinc-800 px-2 py-0.5 text-xs text-zinc-300 ring-1 ring-inset ring-zinc-600 hover:bg-zinc-700"
              title="Clear the age filter"
            >
              {aging?.buckets.find((b) => b.bucket === bucketFilter)?.label} ✕
            </button>
          ) : null}
          <span className="ml-auto text-xs text-zinc-400">
            most overdue first
          </span>
        </div>
        {visibleRows.length === 0 ? (
          <div className="p-8">
            {sync && !sync.backfill_complete ? (
              <EmptyState title={`Invoices are still loading from ${provider}`}>
                Check back in a minute or hit Refresh.
              </EmptyState>
            ) : bucketFilter ? (
              <EmptyState title="No invoices in this age range.">
                <Button variant="ghost" size="sm" onClick={() => setBucketFilter(null)}>
                  Clear filter
                </Button>
              </EmptyState>
            ) : (
              <EmptyState variant="celebrate" title="No unpaid invoices — nice." />
            )}
          </div>
        ) : (
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-emerald`}>
              <tr>
                <th className={cellCls}>Invoice</th>
                <th className={cellCls}>Customer</th>
                <th className={cellCls}>Due</th>
                <th className={numCellCls}>Balance</th>
                <th className={cellCls}>Status</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {visibleRows.map((row) => {
                const due = accountingDueLabel(row);
                return (
                  <tr
                    key={row.invoice_id}
                    className={rowHoverCls}
                    title={`Issued ${row.txn_date ?? "—"} · Total ${fmtMoney(row.total_cents)}`}
                  >
                    <td className={`${cellCls} font-mono text-xs text-zinc-300`}>
                      {row.doc_number ?? row.invoice_id}
                    </td>
                    <td className={`${cellCls} text-zinc-200`}>
                      {row.customer_name ?? "—"}
                    </td>
                    <td className={`${cellCls} ${due.cls}`}>{due.text}</td>
                    <td className={`${numCellCls} text-zinc-200`}>
                      {fmtMoney(row.balance_cents)}
                    </td>
                    <td className={cellCls}>
                      <StatusBadge
                        tone={
                          row.status === "overdue"
                            ? "critical"
                            : row.status === "open"
                              ? "info"
                              : "neutral"
                        }
                      >
                        {row.status}
                      </StatusBadge>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {openRows.length > TABLE_PREVIEW_ROWS ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowAllRows((open) => !open)}
            className="w-full border-t border-zinc-800 rounded-none rounded-b-lg"
          >
            {showAllRows
              ? "Show fewer"
              : `Show all ${openRows.length} invoices`}
          </Button>
        ) : null}
      </div>

      <div className="surface-card surface-flat surface-body-emerald rounded-lg border border-zinc-800">
        <button
          onClick={() => setShowCustomers((open) => !open)}
          className="surface-head-emerald flex w-full items-center justify-between px-3 py-2 text-sm font-semibold text-zinc-200 hover:bg-zinc-900/60"
        >
          <span>Customers ({customersRes?.customers.length ?? 0})</span>
          <span className="text-xs text-zinc-400">{showCustomers ? "Hide" : "Show"}</span>
        </button>
        {tierSyncEnabled ? (
          <CustomerTierSyncPanel
            run={tierSyncRun}
            busy={tierSyncBusy}
            onPreview={() => void previewTierSync()}
            onApprove={(run) => void runTierSyncAction(run, "approve")}
            onReject={(run) => void runTierSyncAction(run, "reject")}
          />
        ) : null}
        {showCustomers && customersRes ? (
          <table className={`${tableCls} border-t border-zinc-800`}>
            <thead className={`${theadCls} surface-head-emerald`}>
              <tr>
                <th className={cellCls}>Customer</th>
                <th className={cellCls}>Company</th>
                <th className={cellCls}>Email</th>
                <th className={cellCls}>Tier</th>
                <th className={cellCls}>Active</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {customersRes.customers.map((row) => (
                <tr key={row.customer_id} className={rowHoverCls}>
                  <td className={`${cellCls} text-zinc-200`}>{row.display_name}</td>
                  <td className={`${cellCls} text-zinc-400`}>{row.company_name ?? "—"}</td>
                  <td className={`${cellCls} text-zinc-400`}>{row.email ?? "—"}</td>
                  <td className={cellCls}>
                    {row.tier ? (
                      <span className="rounded-full border border-teal-800 bg-teal-950/50 px-2 py-0.5 text-xs font-semibold text-teal-300">
                        {row.tier}
                      </span>
                    ) : (
                      <span className="text-zinc-500">—</span>
                    )}
                  </td>
                  <td className={`${cellCls} text-zinc-400`}>{row.active ? "yes" : "no"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
      </div>
    </div>
  );
}
