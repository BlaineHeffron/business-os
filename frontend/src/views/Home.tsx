import { useCallback, useState, type ReactNode } from "react";
import type { HomeDashboardResponse } from "../types/generated/HomeDashboardResponse";
import type { HomeDashboardMetric } from "../types/generated/HomeDashboardMetric";
import type { HomeDashboardTarget } from "../types/generated/HomeDashboardTarget";
import type { HomeDashboardWidget } from "../types/generated/HomeDashboardWidget";
import type { HomeDashboardWidgetItem } from "../types/generated/HomeDashboardWidgetItem";
import type { HomeDashboardWidgetKind } from "../types/generated/HomeDashboardWidgetKind";
import type { SearchConsoleTrafficOverview } from "../types/generated/SearchConsoleTrafficOverview";
import {
  api,
  errorMessage,
  isUnauthorized,
} from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import type { StatusTone } from "../lib/status";
import {
  Bar,
  Button,
  Card,
  Donut,
  EmptyState,
  Funnel,
  KpiCard,
  MetricHelp,
  SkeletonList,
  Surface,
  Sparkline,
  StatusBadge,
  surfaceAccentClasses,
  type SurfaceAccent,
} from "../components/ui";

const POLL_INTERVAL_MS = 60_000;

const BUSINESS_SUMMARY_KIND: HomeDashboardWidgetKind = "business_summary";

type WidgetAccent = {
  header: string;
  border: string;
  tint: string;
  text: string;
  fill: string;
  focus: string;
  chartText: string;
};

const WIDGET_ACCENT: Record<HomeDashboardWidgetKind, WidgetAccent> = {
  business_summary: {
    header: "bg-sky-950/40 border-sky-700/60",
    border: "border-l-sky-500",
    tint: "bg-sky-950/50",
    text: "text-sky-400",
    fill: "bg-sky-500",
    focus: "focus-visible:ring-sky-500/70",
    chartText: "text-sky-400",
  },
  important_emails: {
    header: "bg-sky-950/50 border-sky-700/60",
    border: "border-l-sky-500",
    tint: "bg-sky-950/50",
    text: "text-sky-400",
    fill: "bg-sky-500",
    focus: "focus-visible:ring-sky-500/70",
    chartText: "text-sky-400",
  },
  recent_orders: {
    header: "bg-emerald-950/50 border-emerald-700/60",
    border: "border-l-emerald-500",
    tint: "bg-emerald-950/50",
    text: "text-emerald-400",
    fill: "bg-emerald-500",
    focus: "focus-visible:ring-emerald-500/70",
    chartText: "text-emerald-400",
  },
  open_tasks: {
    header: "bg-amber-950/50 border-amber-700/60",
    border: "border-l-amber-500",
    tint: "bg-amber-950/50",
    text: "text-amber-400",
    fill: "bg-amber-500",
    focus: "focus-visible:ring-amber-500/70",
    chartText: "text-amber-400",
  },
  sales_pipeline: {
    header: "bg-orange-950/50 border-orange-700/60",
    border: "border-l-orange-500",
    tint: "bg-orange-950/50",
    text: "text-orange-400",
    fill: "bg-orange-500",
    focus: "focus-visible:ring-orange-500/70",
    chartText: "text-orange-400",
  },
  inventory_alerts: {
    header: "bg-teal-950/50 border-teal-700/60",
    border: "border-l-teal-500",
    tint: "bg-teal-950/50",
    text: "text-teal-400",
    fill: "bg-teal-500",
    focus: "focus-visible:ring-teal-500/70",
    chartText: "text-teal-400",
  },
  financial_overview: {
    header: "bg-emerald-950/50 border-emerald-700/60",
    border: "border-l-emerald-500",
    tint: "bg-emerald-950/50",
    text: "text-emerald-400",
    fill: "bg-emerald-500",
    focus: "focus-visible:ring-emerald-500/70",
    chartText: "text-emerald-400",
  },
  system_health: {
    header: "bg-emerald-950/40 border-emerald-700/60",
    border: "border-l-emerald-500",
    tint: "bg-emerald-950/50",
    text: "text-emerald-400",
    fill: "bg-emerald-500",
    focus: "focus-visible:ring-emerald-500/70",
    chartText: "text-emerald-400",
  },
  help_shortcuts: {
    header: "bg-zinc-950/50 border-zinc-700/60",
    border: "border-l-zinc-500",
    tint: "bg-zinc-950/50",
    text: "text-zinc-400",
    fill: "bg-zinc-500",
    focus: "focus-visible:ring-zinc-500/70",
    chartText: "text-zinc-400",
  },
  work_queue_events: {
    header: "bg-violet-950/40 border-violet-700/60",
    border: "border-l-violet-500",
    tint: "bg-violet-950/50",
    text: "text-violet-400",
    fill: "bg-violet-500",
    focus: "focus-visible:ring-violet-500/70",
    chartText: "text-violet-400",
  },
  system_diagnostics: {
    header: "bg-rose-950/40 border-rose-700/60",
    border: "border-l-rose-500",
    tint: "bg-rose-950/50",
    text: "text-rose-400",
    fill: "bg-rose-500",
    focus: "focus-visible:ring-rose-500/70",
    chartText: "text-rose-400",
  },
};

// Banded layout: cards line up in fixed rows of three so every card in a row
// shares the same height (CSS grid items-stretch). Row 1 is the inbox/queue/
// tasks triplet, row 2 the sales/orders/financial triplet; the final row keeps
// inventory in one column, website traffic in the second, and short status boxes
// stacked in the third. Unlisted/leftover widgets append before the stacks so
// toggles in Settings still render.
const ROW_ORDER: HomeDashboardWidgetKind[] = [
  "important_emails",
  "work_queue_events",
  "open_tasks",
  "sales_pipeline",
  "recent_orders",
  "financial_overview",
];

const INVENTORY_KIND: HomeDashboardWidgetKind = "inventory_alerts";

const STACK_ORDER: HomeDashboardWidgetKind[] = ["system_health", "help_shortcuts"];

// Each card carries its own content hue (light theme): a coloured header band
// over a faint body wash, keyed off the same accent family the icon/border/chart
// already use. Plain CSS classes (see index.css), so the tint only lands in
// light mode and dark keeps its flat surfaces.
const CARD_COLOR: Record<HomeDashboardWidgetKind, SurfaceAccent> = {
  business_summary: "sky",
  important_emails: "sky",
  recent_orders: "emerald",
  open_tasks: "amber",
  sales_pipeline: "orange",
  inventory_alerts: "teal",
  financial_overview: "emerald",
  system_health: "emerald",
  help_shortcuts: "zinc",
  work_queue_events: "violet",
  system_diagnostics: "rose",
};

// How many list rows each card fills with. Tuned per widget so a card with a
// tall chart/metrics shows fewer rows and a list-only card shows more; the rest
// of the row height is taken up by the action pinned to the card's bottom.
const WIDGET_ITEMS: Partial<Record<HomeDashboardWidgetKind, number>> = {
  sales_pipeline: 2,
  recent_orders: 1,
  financial_overview: 0,
  inventory_alerts: 2,
  open_tasks: 2,
  important_emails: 3,
  work_queue_events: 3,
  system_health: 6,
  help_shortcuts: 3,
  system_diagnostics: 2,
};

const DONUT_SEGMENT_CLASSES = [
  "text-sky-400",
  "text-emerald-400",
  "text-amber-400",
  "text-rose-400",
  "text-violet-400",
  "text-teal-400",
];

const STATUS_TONES: readonly StatusTone[] = [
  "ok",
  "warning",
  "critical",
  "info",
  "ai",
  "progress",
  "neutral",
];

const VISIBLE_METRIC_LABELS: Partial<Record<HomeDashboardWidgetKind, readonly string[]>> = {
  sales_pipeline: ["Awaiting review"],
  open_tasks: ["Overdue", "Due today"],
  recent_orders: ["Exceptions", "Blocked"],
  inventory_alerts: ["Out of stock", "Reorder"],
  financial_overview: ["Accounts receivable", "Accounts payable", "Cash on hand"],
  // System health is a compact stacked status box — its connection rows already
  // show each provider's state with a tone badge, so the separate count tile is
  // dropped to keep the box inside its half-height slot.
  system_health: [],
};

function visibleItemLimit(widget: HomeDashboardWidget): number {
  return WIDGET_ITEMS[widget.kind] ?? 3;
}

function visibleMetrics(widget: HomeDashboardWidget): HomeDashboardMetric[] {
  const labels = VISIBLE_METRIC_LABELS[widget.kind];
  if (!labels) return widget.metrics;
  return widget.metrics.filter((metric) => labels.includes(metric.label));
}

function dashboardTitle(brandName: string): string {
  const trimmed = brandName.trim();
  return trimmed && trimmed !== "BusinessOS" ? `${trimmed} Dashboard` : "Dashboard";
}

function fmtCompact(value: number): string {
  return value.toLocaleString("en-US", {
    notation: Math.abs(value) >= 10_000 ? "compact" : "standard",
    maximumFractionDigits: Math.abs(value) >= 10_000 ? 1 : 0,
  });
}

function fmtPercentMicros(value: number): string {
  return `${(value / 10_000).toFixed(1)}%`;
}

function fmtPositionMicros(value: number): string {
  if (value <= 0) return "n/a";
  return (value / 1_000_000).toFixed(1);
}

function fmtSyncAge(ms: bigint | number | null | undefined): string {
  if (ms === null || ms === undefined) return "not synced";
  const value = typeof ms === "bigint" ? Number(ms) : ms;
  const delta = Date.now() - value;
  if (delta < 60_000) return "just now";
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function TrafficMiniRow({
  label,
  value,
  detail,
}: {
  label: ReactNode;
  value: string;
  detail?: string;
}) {
  return (
    <div className="flex items-center justify-between gap-3 border-t border-zinc-800/60 py-1.5 first:border-t-0 first:pt-0 last:pb-0">
      <div className="min-w-0">
        <div className="truncate text-[12px] font-medium text-zinc-200">{label}</div>
        {detail ? <div className="truncate text-[11px] text-zinc-500">{detail}</div> : null}
      </div>
      <div className="shrink-0 text-sm font-semibold tabular-nums text-zinc-100">{value}</div>
    </div>
  );
}

// Dense single-column top-N list used inside a grid (e.g. top landing pages
// beside top sources) so a small card can carry several rows per column.
function TrafficTopList({
  title,
  help,
  rows,
  emptyLabel,
}: {
  title: string;
  help?: ReactNode;
  rows: Array<{ label: string; value: string }>;
  emptyLabel: string;
}) {
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
        <span className="truncate">{title}</span>
        {help}
      </div>
      <div className="mt-1 space-y-0.5">
        {rows.length === 0 ? (
          <div className="text-[11px] text-zinc-600">{emptyLabel}</div>
        ) : (
          rows.map((row, i) => (
            <div
              key={`${row.label}-${i}`}
              className="flex items-center justify-between gap-2"
            >
              <span className="min-w-0 truncate text-[11px] text-zinc-300">
                {row.label}
              </span>
              <span className="shrink-0 text-[11px] font-semibold tabular-nums text-zinc-100">
                {row.value}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

function hasSearchConsoleTraffic(traffic: SearchConsoleTrafficOverview | null): boolean {
  return Boolean(
    traffic?.configured &&
      traffic.credential_connected &&
      traffic.scope_granted !== false &&
      traffic.last_synced_at_ms !== null &&
      traffic.last_synced_at_ms !== undefined,
  );
}

function hasAnalyticsTraffic(traffic: SearchConsoleTrafficOverview | null): boolean {
  return Boolean(
    traffic?.analytics_configured &&
      traffic.credential_connected &&
      traffic.analytics_last_synced_at_ms !== null &&
      traffic.analytics_last_synced_at_ms !== undefined,
  );
}

function trafficCardCount(traffic: SearchConsoleTrafficOverview | null): number {
  return (hasSearchConsoleTraffic(traffic) ? 1 : 0) + (hasAnalyticsTraffic(traffic) ? 1 : 0);
}

function analyticsSpamExclusionDetail(traffic: SearchConsoleTrafficOverview): string | null {
  const excluded = traffic.analytics_excluded_referrer_spam_week.sessions;
  if (excluded <= 0) return null;
  return `${fmtCompact(excluded)} suspected spam sessions excluded`;
}

function SearchConsoleHomeCard({ traffic }: { traffic: SearchConsoleTrafficOverview }) {
  if (!hasSearchConsoleTraffic(traffic)) return null;
  const topQuery = traffic.top_queries_week[0];
  const topPage = traffic.top_pages_week[0];
  const clicks = traffic.week.clicks;
  return (
    <Surface
      accent="violet"
      title="Google search traffic"
      subtitle={`${traffic.property_url ?? "Search Console"} · synced ${fmtSyncAge(
        traffic.last_synced_at_ms,
      )}`}
      actions={
        <MetricHelp label="What Google search traffic means">
          Visits and search appearances from Google Search results. These come from Google Search Console, not website page-view tracking.
        </MetricHelp>
      }
      className="overflow-visible"
      bodyClassName="p-3"
    >
      <div className="grid grid-cols-2 gap-3">
        <div>
          <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
            <span>Visits from search</span>
            <MetricHelp label="What visits from search means">
              People who clicked a Google Search result and landed on the website this week.
            </MetricHelp>
          </div>
          <div className="mt-0.5 text-2xl font-bold tabular-nums text-zinc-100">
            {fmtCompact(clicks)}
          </div>
          <div className="text-[11px] text-zinc-500">
            {fmtCompact(traffic.month_to_date.clicks)} MTD
          </div>
        </div>
        <div>
          <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
            <span>Search appearances</span>
            <MetricHelp label="What search appearances means">
              How often the website appeared in Google Search results. This is not page views.
            </MetricHelp>
          </div>
          <div className="mt-0.5 text-2xl font-bold tabular-nums text-zinc-100">
            {fmtCompact(traffic.week.impressions)}
          </div>
          <div className="text-[11px] text-zinc-500">
            CTR {fmtPercentMicros(traffic.week.ctr_micros)} · avg pos{" "}
            {fmtPositionMicros(traffic.week.position_micros)}
          </div>
        </div>
      </div>
      <div className="mt-3 space-y-0.5">
        <TrafficMiniRow
          label="Brand searches"
          value={fmtCompact(traffic.branded_week.clicks)}
          detail={`${fmtCompact(traffic.branded_week.impressions)} search appearances`}
        />
        <TrafficMiniRow
          label="Non-brand searches"
          value={fmtCompact(traffic.nonbranded_week.clicks)}
          detail={`${fmtCompact(traffic.nonbranded_week.impressions)} search appearances`}
        />
        {topQuery ? (
          <TrafficMiniRow
            label="Top search term"
            value={fmtCompact(topQuery.metrics.clicks)}
            detail={topQuery.value}
          />
        ) : null}
        {topPage ? (
          <TrafficMiniRow
            label="Top search landing page"
            value={fmtCompact(topPage.metrics.clicks)}
            detail={topPage.value}
          />
        ) : null}
      </div>
    </Surface>
  );
}

function AnalyticsHomeCard({ traffic }: { traffic: SearchConsoleTrafficOverview }) {
  if (!hasAnalyticsTraffic(traffic)) return null;
  const topLandingPages = traffic.top_landing_pages_week.slice(0, 4);
  const topSources = traffic.top_sources_week.slice(0, 4);
  const propertyId = traffic.analytics_property_id?.trim();
  const subtitlePrefix = propertyId ? `GA4 property ${propertyId}` : "GA4";
  const exclusionDetail = analyticsSpamExclusionDetail(traffic);
  return (
    <Surface
      accent="sky"
      title="Website traffic"
      subtitle={`${subtitlePrefix} · synced ${fmtSyncAge(
        traffic.analytics_last_synced_at_ms,
      )}`}
      actions={
        <MetricHelp label="What website traffic means">
          Website visits measured by Google Analytics 4. These numbers are separate from Google Search result appearances.
        </MetricHelp>
      }
      className="overflow-visible"
      bodyClassName="p-3"
    >
      <div className="grid grid-cols-3 gap-3">
        <div>
          <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
            <span>Sessions</span>
            <MetricHelp label="What sessions means">
              Website visits measured by Google Analytics 4. One session can include several page views.
            </MetricHelp>
          </div>
          <div className="mt-0.5 text-xl font-bold tabular-nums text-zinc-100">
            {fmtCompact(traffic.analytics_week.sessions)}
          </div>
        </div>
        <div>
          <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
            <span>Users</span>
            <MetricHelp label="What users means">
              People Google Analytics counted as visitors during the week.
            </MetricHelp>
          </div>
          <div className="mt-0.5 text-xl font-bold tabular-nums text-zinc-100">
            {fmtCompact(traffic.analytics_week.total_users)}
          </div>
        </div>
        <div>
          <div className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
            <span>Conversions</span>
            <MetricHelp label="What conversions means" align="left">
              Website actions marked as conversions in Google Analytics 4, such as a form submission or other configured goal.
            </MetricHelp>
          </div>
          <div className="mt-0.5 text-xl font-bold tabular-nums text-zinc-100">
            {fmtCompact(traffic.analytics_week.conversions)}
          </div>
        </div>
      </div>
      <div className="mt-2 text-[11px] text-zinc-500">
        {fmtCompact(traffic.analytics_month_to_date.sessions)} sessions MTD
        {exclusionDetail ? ` · ${exclusionDetail}` : ""}
      </div>
      <div className="mt-3 grid grid-cols-2 gap-x-4 gap-y-1 border-t border-zinc-800/60 pt-2">
        <TrafficTopList
          title="Top landing pages"
          help={
            <MetricHelp label="What top landing pages means">
              The pages visitors arrived on most, ranked by Google Analytics 4 sessions this week.
            </MetricHelp>
          }
          rows={topLandingPages.map((row) => ({
            label: row.value,
            value: fmtCompact(row.metrics.sessions),
          }))}
          emptyLabel="No pages yet"
        />
        <TrafficTopList
          title="Top sources"
          help={
            <MetricHelp label="What top sources means" align="left">
              The GA4 source and medium combinations that sent the most visitors this week, ranked by sessions.
            </MetricHelp>
          }
          rows={topSources.map((row) => ({
            label: row.value,
            value: fmtCompact(row.metrics.sessions),
          }))}
          emptyLabel="No sources yet"
        />
      </div>
    </Surface>
  );
}

function WebsiteTrafficStack({ traffic }: { traffic: SearchConsoleTrafficOverview | null }) {
  if (!traffic) return null;
  const cards: ReactNode[] = [];
  if (hasSearchConsoleTraffic(traffic)) {
    cards.push(<SearchConsoleHomeCard key="gsc" traffic={traffic} />);
  }
  if (hasAnalyticsTraffic(traffic)) {
    cards.push(<AnalyticsHomeCard key="ga4" traffic={traffic} />);
  }
  if (cards.length === 0) return null;
  return <div className="flex flex-col gap-3">{cards}</div>;
}

function ribbonTone(metric: HomeDashboardMetric): "ok" | "warning" | "info" {
  const label = metric.label.toLowerCase();
  if (label.includes("revenue")) return "ok";
  if (label.includes("order")) return "warning";
  return "info";
}

function normalizeTone(tone: string | null | undefined): StatusTone {
  return STATUS_TONES.includes(tone as StatusTone) ? (tone as StatusTone) : "neutral";
}

function statusLabel(tone: StatusTone): string {
  switch (tone) {
    case "ok":
      return "connected";
    case "warning":
      return "attention";
    case "critical":
      return "critical";
    case "ai":
      return "AI";
    case "progress":
      return "syncing";
    case "info":
      return "new";
    case "neutral":
      return "info";
  }
}

function itemStatusLabel(widget: HomeDashboardWidget, item: HomeDashboardWidgetItem): string {
  const tone = normalizeTone(item.tone);
  if (widget.kind === "open_tasks") {
    if (tone === "critical") return "overdue";
    if (tone === "warning") return "due today";
    return "open";
  }
  if (widget.kind === "system_health") return statusLabel(tone);
  if (widget.kind === "sales_pipeline") return "lead";
  if (widget.kind === "important_emails") return "email";
  if (widget.kind === "work_queue_events" && tone === "ai") return "AI draft";
  return statusLabel(tone);
}

function metricValueClass(widget: HomeDashboardWidget, metric: HomeDashboardMetric, isLead: boolean): string {
  const valueLength = (metric.value ?? "-").length;
  if (widget.kind === "financial_overview") {
    return valueLength > 10 ? "text-base" : valueLength > 8 ? "text-lg" : "text-xl";
  }
  if (valueLength > 10) return "text-base";
  if (valueLength > 8) return "text-lg";
  return isLead ? "text-xl" : "text-lg";
}

function metricTone(widget: HomeDashboardWidget, metric: HomeDashboardMetric): StatusTone | null {
  const label = metric.label.toLowerCase();
  if (widget.kind === "open_tasks") {
    if (label.includes("overdue")) return "critical";
    if (label.includes("due today")) return "warning";
    return "neutral";
  }
  if (widget.kind === "work_queue_events") return "warning";
  if (widget.kind === "important_emails" && label.includes("unread")) return "info";
  if (widget.kind === "system_health") {
    if (label.includes("attention")) return "warning";
    if (label.includes("connected")) return metric.value === "0" ? "neutral" : "ok";
  }
  return null;
}

function BusinessSummaryRibbon({
  widget,
  onNavigate,
}: {
  widget: HomeDashboardWidget | undefined;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  if (!widget || widget.state !== "ready" || widget.metrics.length === 0) return null;

  return (
    <div className="grid grid-cols-1 gap-3 lg:grid-cols-3">
      {widget.metrics.map((metric) => {
        const card = (
          <KpiCard
            label={metric.label}
            value={metric.value ?? "-"}
            tone={ribbonTone(metric)}
            hero
            className="surface-flat"
          />
        );
        return metric.target ? (
          <button
            key={metric.label}
            type="button"
            onClick={() => onNavigate(metric.target!)}
            className="block rounded-lg text-left transition hover:-translate-y-0.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
          >
            {card}
          </button>
        ) : (
          <div key={metric.label}>{card}</div>
        );
      })}
    </div>
  );
}

function WidgetChart({
  widget,
  accent,
  onNavigate,
}: {
  widget: HomeDashboardWidget;
  accent: WidgetAccent;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  const chartTotal =
    widget.chart?.type === "donut"
      ? widget.chart.segments.reduce((sum, segment) => sum + Math.max(0, segment.value), 0)
      : 0;
  const donutSegments =
    widget.chart?.type === "donut"
      ? widget.chart.segments.map((segment, index) => ({
          label: segment.label,
          value: segment.value,
          colorClassName: DONUT_SEGMENT_CLASSES[index % DONUT_SEGMENT_CLASSES.length],
        }))
      : [];
  const barItems =
    widget.chart?.type === "bar"
      ? widget.chart.items.map((item) => ({
          label: item.label,
          value: item.value,
          target: item.target ?? undefined,
        }))
      : [];
  const sparklinePoints =
    widget.chart?.type === "sparkline"
      ? widget.chart.points.map((point) => point.value)
      : [];
  const funnelStages =
    widget.chart?.type === "funnel"
      ? widget.chart.stages.map((stage) => ({
          label: stage.label,
          value: stage.value,
          target: stage.target ?? undefined,
        }))
      : [];

  if (!widget.chart) return null;

  return (
    <div className="border-b border-zinc-800/60 px-3 py-3">
      {widget.chart.type === "donut" ? (
        <div className="flex flex-col items-center gap-3 sm:flex-row sm:items-center">
          <Donut
            segments={donutSegments}
            size={116}
            thickness={14}
            title={widget.title}
            ariaLabel={`${widget.title} stage distribution`}
            rounded
            center={
              <div>
                <div className="text-3xl font-bold tabular-nums text-zinc-100">
                  {chartTotal}
                </div>
                <div className="text-xs text-zinc-500">orders</div>
              </div>
            }
          />
          <div className="min-w-0 flex-1 space-y-1.5">
            {widget.chart.segments.map((segment, index) => {
              const colorCls = DONUT_SEGMENT_CLASSES[index % DONUT_SEGMENT_CLASSES.length];
              return (
                <div
                  key={segment.label}
                  className="flex items-center justify-between gap-3 text-sm"
                >
                  <span className="flex min-w-0 items-center gap-2 text-zinc-400">
                    <span className={`h-2.5 w-2.5 shrink-0 rounded-full bg-current ${colorCls}`} />
                    <span className="truncate">{segment.label}</span>
                  </span>
                  <span className="font-semibold tabular-nums text-zinc-200">
                    {segment.value}
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      ) : widget.chart.type === "bar" ? (
        <div className="space-y-2">
          {widget.kind === "inventory_alerts" ? (
            <div className="text-xs font-medium text-zinc-400">
              Top stocked SKUs by value
            </div>
          ) : null}
          <Bar
            items={barItems}
            title={widget.title}
            ariaLabel={`${widget.title} top SKUs`}
            clean
            barClassName={accent.fill}
            onItemClick={(item) => {
              if (item.target) onNavigate(item.target as HomeDashboardTarget);
            }}
          />
        </div>
      ) : widget.chart.type === "funnel" ? (
        funnelStages.length === 0 ? (
          <div className="flex items-center justify-between gap-3 rounded-md border border-dashed border-zinc-800 bg-zinc-950/30 px-2.5 py-2">
            <div className="min-w-0">
              <div className="text-xs font-medium text-zinc-300">Deals pipeline</div>
              <div className="truncate text-[11px] text-zinc-500">
                HubSpot deals data not connected yet
              </div>
            </div>
            <StatusBadge tone="neutral">pending source</StatusBadge>
          </div>
        ) : (
          <div className="space-y-1.5">
            <div className="text-xs font-medium text-zinc-400">
              Deals pipeline
            </div>
            <Funnel
              stages={funnelStages}
              title={widget.title}
              ariaLabel="Sales pipeline stages"
              pendingLabel="HubSpot deals data not connected yet"
              onStageClick={(stage) => {
                if (stage.target) onNavigate(stage.target as HomeDashboardTarget);
              }}
            />
          </div>
        )
      ) : (
        <div className="space-y-1.5">
          <div className="text-xs font-medium text-zinc-400">
            Daily revenue · last 7 days
          </div>
          <Sparkline
            points={sparklinePoints}
            width={360}
            height={56}
            strokeWidth={3}
            title="Daily revenue · last 7 days"
            ariaLabel="Daily revenue last 7 days"
            className={`h-14 w-full ${accent.chartText}`}
            showArea
            showGrid
          />
          {widget.chart.points.length > 0 ? (
            <div className="flex items-center justify-between gap-2 text-[11px] text-zinc-500">
              <span>{widget.chart.points[0]?.label}</span>
              <span>{widget.chart.points.at(-1)?.label}</span>
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}

function MetricBlock({
  widget,
  metric,
  accent,
  tone,
  onNavigate,
}: {
  widget: HomeDashboardWidget;
  metric: HomeDashboardMetric;
  accent: WidgetAccent;
  tone?: StatusTone | null;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  const metricContent = (
    <>
      <div className="flex items-center gap-1.5">
        <div className="truncate text-[10px] font-semibold uppercase leading-tight tracking-wide text-zinc-500">
          {metric.label}
        </div>
        {tone ? <span className={`h-1.5 w-1.5 rounded-full ${tone === "critical" ? "bg-red-400" : tone === "warning" ? "bg-amber-400" : tone === "ok" ? "bg-emerald-400" : tone === "info" ? "bg-sky-400" : "bg-zinc-400"}`} /> : null}
      </div>
      <div
        className={`mt-0.5 max-w-full overflow-hidden text-ellipsis whitespace-nowrap font-bold leading-tight tabular-nums text-zinc-100 ${metricValueClass(widget, metric, false)}`}
        title={metric.value ?? undefined}
      >
        {metric.value ?? "-"}
      </div>
    </>
  );
  const cls = "min-w-0 flex-1 px-3 py-2";
  return metric.target ? (
    <button
      type="button"
      onClick={() => onNavigate(metric.target!)}
      className={`${cls} text-left transition hover:bg-zinc-500/[0.07] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset ${accent.focus}`}
    >
      {metricContent}
    </button>
  ) : (
    <div className={cls}>{metricContent}</div>
  );
}

function WidgetMetrics({
  widget,
  accent,
  onNavigate,
}: {
  widget: HomeDashboardWidget;
  accent: WidgetAccent;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  const metrics = visibleMetrics(widget);
  if (metrics.length === 0) return null;

  const block = (metric: HomeDashboardMetric) => (
    <MetricBlock
      key={metric.label}
      widget={widget}
      metric={metric}
      accent={accent}
      tone={metricTone(widget, metric)}
      onNavigate={onNavigate}
    />
  );

  // Numbers sit flush, side by side, split by hairline dividers (no inset
  // boxes), with a bottom rule separating them from the list below. Three
  // numbers (financials) wrap to a 2-up row plus a full-width row so long money
  // values keep their room instead of truncating at one-third card width.
  if (metrics.length === 3) {
    return (
      <div className="border-b border-zinc-800/60">
        <div className="flex divide-x divide-zinc-800/60 border-b border-zinc-800/60">
          {metrics.slice(0, 2).map(block)}
        </div>
        <div className="flex">{metrics.slice(2).map(block)}</div>
      </div>
    );
  }

  return (
    <div className="flex divide-x divide-zinc-800/60 border-b border-zinc-800/60">
      {metrics.map(block)}
    </div>
  );
}

function RowShell({
  item,
  accent,
  onNavigate,
  children,
  className = "",
}: {
  item: HomeDashboardWidgetItem;
  accent: WidgetAccent;
  onNavigate: (target: HomeDashboardTarget) => void;
  children: ReactNode;
  className?: string;
}) {
  const baseCls = `block w-full px-3 py-2 ${className}`;
  if (!item.target) {
    return <div className={baseCls}>{children}</div>;
  }
  const splitStockforge =
    Boolean(item.target.external_url) && Boolean(item.target.view);
  if (!splitStockforge) {
    return (
      <button
        type="button"
        onClick={() => onNavigate(item.target!)}
        className={`${baseCls} text-left transition hover:bg-zinc-500/[0.07] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset ${accent.focus}`}
      >
        {children}
      </button>
    );
  }
  const bosTarget = {
    ...item.target,
    external_url: undefined,
  };
  return (
    <div className={`${baseCls} flex items-start gap-2`}>
      <button
        type="button"
        onClick={() => onNavigate(bosTarget)}
        className={`min-w-0 flex-1 text-left transition hover:bg-zinc-500/[0.07] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset ${accent.focus}`}
      >
        {children}
      </button>
      <a
        href={item.target.external_url!}
        target="_blank"
        rel="noreferrer"
        className="shrink-0 text-xs text-sky-400 hover:text-sky-300"
        aria-label={`Open ${item.label} in Stockforge`}
      >
        ↗
      </a>
    </div>
  );
}

function WidgetItems({
  widget,
  accent,
  onNavigate,
}: {
  widget: HomeDashboardWidget;
  accent: WidgetAccent;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  if (widget.items.length === 0 && widget.state === "ready") {
    return (
      <div className="px-3 py-3 text-sm text-zinc-400">Nothing needs attention.</div>
    );
  }

  const visibleItems = widget.items.slice(0, visibleItemLimit(widget));
  const remainingCount = Math.max(0, widget.items.length - visibleItems.length);

  return (
    <div className="flex flex-1 flex-col divide-y divide-zinc-800/60">
      {visibleItems.map((item, index) => {
        const tone = normalizeTone(item.tone);
        const label = itemStatusLabel(widget, item);
        const key = `${item.label}-${index}`;

        if (widget.kind === "important_emails") {
          return (
            <RowShell key={key} item={item} accent={accent} onNavigate={onNavigate}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-[13px] font-semibold text-zinc-100">
                    {item.label}
                  </div>
                  {item.detail ? (
                    <div className="truncate text-[11px] text-zinc-400">
                      {item.detail}
                    </div>
                  ) : null}
                </div>
                <StatusBadge tone="info">{label}</StatusBadge>
              </div>
            </RowShell>
          );
        }

        if (widget.kind === "open_tasks") {
          return (
            <RowShell
              key={key}
              item={item}
              accent={accent}
              onNavigate={onNavigate}
              className={tone === "critical" ? "bg-red-950/20" : tone === "warning" ? "bg-amber-950/20" : ""}
            >
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-[13px] font-semibold text-zinc-100">
                    {item.label}
                  </div>
                  {item.detail ? (
                    <div className="truncate text-[11px] text-zinc-400">
                      {item.detail}
                    </div>
                  ) : null}
                </div>
                <StatusBadge tone={tone}>{label}</StatusBadge>
              </div>
            </RowShell>
          );
        }

        if (widget.kind === "sales_pipeline") {
          return (
            <RowShell key={key} item={item} accent={accent} onNavigate={onNavigate}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-[13px] font-semibold text-zinc-100">
                    {item.label}
                  </div>
                  {item.detail ? (
                    <div className="truncate text-[11px] text-zinc-400">
                      {item.detail}
                    </div>
                  ) : (
                    <div className="text-[11px] text-zinc-400">Awaiting lead review</div>
                  )}
                </div>
                <StatusBadge tone="info">{label}</StatusBadge>
              </div>
            </RowShell>
          );
        }

        if (widget.kind === "system_health") {
          return (
            <RowShell key={key} item={item} accent={accent} onNavigate={onNavigate}>
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-[13px] font-semibold text-zinc-100">
                    {item.label}
                  </div>
                  {item.detail ? (
                    <div className="truncate text-[11px] text-zinc-400">
                      {item.detail}
                    </div>
                  ) : null}
                </div>
                <StatusBadge tone={tone} pulse={tone === "progress"}>
                  {label}
                </StatusBadge>
              </div>
            </RowShell>
          );
        }

        if (widget.kind === "help_shortcuts") {
          return (
            <RowShell key={key} item={item} accent={accent} onNavigate={onNavigate}>
              <div className="min-w-0">
                <div className="truncate text-[13px] font-semibold text-zinc-100">
                  {item.label}
                </div>
                {item.detail ? (
                  <div className="truncate text-[11px] text-zinc-400">
                    {item.detail}
                  </div>
                ) : null}
              </div>
            </RowShell>
          );
        }

        return (
          <RowShell key={key} item={item} accent={accent} onNavigate={onNavigate}>
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="truncate text-[13px] font-medium text-zinc-200">
                  {item.label}
                </div>
                {item.detail ? (
                  <div className="truncate text-[11px] text-zinc-400">
                    {item.detail}
                  </div>
                ) : null}
              </div>
              {item.tone ? <StatusBadge tone={tone}>{label}</StatusBadge> : null}
            </div>
          </RowShell>
        );
      })}
      {remainingCount > 0 ? (
        widget.action ? (
          <button
            type="button"
            onClick={() => onNavigate(widget.action!.target)}
            className={`block w-full px-3 py-2 text-left text-xs font-medium text-zinc-500 transition hover:bg-zinc-500/[0.07] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset ${accent.focus}`}
          >
            {remainingCount} more in {widget.title}
          </button>
        ) : (
          <div className="px-3 py-2 text-xs font-medium text-zinc-500">
            {remainingCount} more
          </div>
        )
      ) : null}
    </div>
  );
}

function WidgetCard({
  widget,
  onNavigate,
}: {
  widget: HomeDashboardWidget;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  const accent = WIDGET_ACCENT[widget.kind];
  const color = CARD_COLOR[widget.kind];
  const surface = surfaceAccentClasses(color);
  const chartBeforeMetrics = widget.kind === "inventory_alerts";
  return (
    <Card
      className={`surface-flat ${surface.body} flex h-full flex-col overflow-hidden p-0`}
    >
      <div className={`flex items-center gap-2 border-b px-3 py-2 ${accent.header} ${surface.header}`}>
        <span
          className={`inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md ${accent.tint} ${accent.text}`}
          aria-hidden="true"
        >
          <span className={`h-2.5 w-2.5 rounded-full ${accent.fill}`} />
        </span>
        <div className="min-w-0 text-sm font-semibold leading-tight text-zinc-100">
          {widget.title}
        </div>
      </div>

      <div className="flex flex-1 flex-col">
        {widget.error_code ? (
          <div className="px-3 py-3 text-sm text-zinc-400">
            Not available for this operator.
          </div>
        ) : null}

        {chartBeforeMetrics ? (
          <WidgetChart widget={widget} accent={accent} onNavigate={onNavigate} />
        ) : null}

        <WidgetMetrics widget={widget} accent={accent} onNavigate={onNavigate} />

        {!chartBeforeMetrics ? (
          <WidgetChart widget={widget} accent={accent} onNavigate={onNavigate} />
        ) : null}

        <WidgetItems widget={widget} accent={accent} onNavigate={onNavigate} />

        {widget.action ? (
          <div className="px-3 py-2">
            <Button
              variant="secondary"
              size="sm"
              onClick={() => onNavigate(widget.action!.target)}
            >
              {widget.action.label}
            </Button>
          </div>
        ) : null}
      </div>
    </Card>
  );
}

export default function Home({
  brandName,
  onUnauthorized,
  onNavigate,
}: {
  brandName: string;
  onUnauthorized: () => void;
  onNavigate: (target: HomeDashboardTarget) => void;
}) {
  const [data, setData] = useState<HomeDashboardResponse | null>(null);
  const [traffic, setTraffic] = useState<SearchConsoleTrafficOverview | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [dashboard, trafficStatus] = await Promise.all([
        api.homeDashboard(),
        api.searchConsoleStatus().catch((err) => {
          if (isUnauthorized(err)) onUnauthorized();
          return null;
        }),
      ]);
      setData(dashboard);
      setTraffic(trafficStatus);
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

  const businessSummary = data?.widgets.find((widget) => widget.kind === BUSINESS_SUMMARY_KIND);
  const gridWidgets = data?.widgets.filter((widget) => widget.kind !== BUSINESS_SUMMARY_KIND) ?? [];
  const hasTrafficWidgets = trafficCardCount(traffic) > 0;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-2 lg:flex-row lg:items-start lg:justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight text-zinc-100">
            {dashboardTitle(brandName)}
          </h2>
        </div>
        <Button variant="secondary" size="sm" onClick={() => void load()}>
          Refresh
        </Button>
      </div>

      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load dashboard: {error}
        </div>
      ) : null}
      <BusinessSummaryRibbon widget={businessSummary} onNavigate={onNavigate} />

      {!loaded ? (
        <Card>
          <SkeletonList rows={6} />
        </Card>
      ) : data && data.widgets.length === 0 && !hasTrafficWidgets ? (
        <EmptyState title="No dashboard widgets enabled.">
          Turn on at least one available widget in Settings.
        </EmptyState>
      ) : data && (gridWidgets.length > 0 || hasTrafficWidgets) ? (
        (() => {
          const byKind = new Map(gridWidgets.map((widget) => [widget.kind, widget] as const));
          const placed = new Set<HomeDashboardWidgetKind>([
            ...ROW_ORDER,
            INVENTORY_KIND,
            ...STACK_ORDER,
          ]);
          const rowCards = ROW_ORDER.map((kind) => byKind.get(kind)).filter(
            (widget): widget is HomeDashboardWidget => Boolean(widget),
          );
          const inventoryCard = byKind.get(INVENTORY_KIND);
          const leftover = gridWidgets.filter((widget) => !placed.has(widget.kind));
          const stackCards = STACK_ORDER.map((kind) => byKind.get(kind)).filter(
            (widget): widget is HomeDashboardWidget => Boolean(widget),
          );
          return (
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3 xl:items-stretch">
              {[...rowCards, ...leftover].map((widget) => (
                <div key={widget.kind}>
                  <WidgetCard widget={widget} onNavigate={onNavigate} />
                </div>
              ))}
              {inventoryCard ? (
                <div>
                  <WidgetCard widget={inventoryCard} onNavigate={onNavigate} />
                </div>
              ) : null}
              <WebsiteTrafficStack traffic={traffic} />
              {stackCards.length > 0 ? (
                <div className="flex flex-col gap-3">
                  {stackCards.map((widget) => (
                    <div
                      key={widget.kind}
                      className={widget.kind === "help_shortcuts" ? "min-h-0 flex-none" : "min-h-0 flex-1"}
                    >
                      <WidgetCard widget={widget} onNavigate={onNavigate} />
                    </div>
                  ))}
                </div>
              ) : null}
            </div>
          );
        })()
      ) : null}
    </div>
  );
}
