import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import type { InventoryAlertsResponse } from "../types/generated/InventoryAlertsResponse";
import type { InventoryOrderRow } from "../types/generated/InventoryOrderRow";
import type { InventoryOrdersResponse } from "../types/generated/InventoryOrdersResponse";
import type { InventoryPurchaseOrdersResponse } from "../types/generated/InventoryPurchaseOrdersResponse";
import type { InventoryStockResponse } from "../types/generated/InventoryStockResponse";
import type { InventoryStockRow } from "../types/generated/InventoryStockRow";
import type { InventorySyncInfo } from "../types/generated/InventorySyncInfo";
import type { StockforgeConnectorStatus } from "../types/generated/StockforgeConnectorStatus";
import { ApiError, api, errorMessage, isRevisionConflict, isUnauthorized } from "../lib/api";
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
const STALE_AFTER_MS = 6 * 60 * 60 * 1000;
const TABLE_PREVIEW_ROWS = 20;

/** Live-board pipeline, in flow order (Stockforge's own column order). */
const PIPELINE: { key: string; label: string; color: string }[] = [
  { key: "NEW", label: "New", color: "bg-sky-500" },
  { key: "PICKING", label: "Picking", color: "bg-indigo-500" },
  { key: "PACKED", label: "Packed", color: "bg-violet-500" },
  { key: "SHIPPED", label: "Shipped", color: "bg-teal-500" },
  { key: "DELIVERED", label: "Delivered", color: "bg-emerald-600" },
];

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

export type StockScope = "stocked" | "catalog";
export type StockStatusFilter = "all" | "out" | "critical" | "warning" | "attention";

function StockforgeName({
  href,
  children,
}: {
  href?: string | null;
  children: string;
}) {
  return (
    <span className="inline-flex min-w-0 items-center gap-1.5">
      <span className="truncate">{children}</span>
      {href ? (
        <a
          href={href}
          target="_blank"
          rel="noreferrer"
          className="shrink-0 text-xs text-sky-400 hover:text-sky-300"
          aria-label={`Open ${children} in Stockforge`}
        >
          ↗
        </a>
      ) : null}
    </span>
  );
}

export function matchesStockStatus(status: string, filter: StockStatusFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "out":
      return status === "out";
    case "critical":
      return status === "critical";
    case "warning":
      return status === "warning";
    case "attention":
      return status === "out" || status === "critical" || status === "warning";
  }
}

export function scopedStockRows(
  materials: InventoryStockRow[],
  scope: StockScope,
): InventoryStockRow[] {
  return scope === "catalog" ? materials : materials.filter((row) => row.is_stocked);
}

export function filterStockRows(
  materials: InventoryStockRow[],
  options: {
    scope: StockScope;
    status: StockStatusFilter;
    vendor: string;
    category: string;
    query: string;
  },
): InventoryStockRow[] {
  const needle = options.query.trim().toLowerCase();
  return scopedStockRows(materials, options.scope).filter((row) => {
    if (!matchesStockStatus(row.stock_status, options.status)) return false;
    if (options.vendor !== "all" && (row.vendor_name ?? "") !== options.vendor) return false;
    if (options.category !== "all" && (row.category ?? "") !== options.category) return false;
    if (!needle) return true;
    return row.name.toLowerCase().includes(needle) || (row.sku ?? "").toLowerCase().includes(needle);
  });
}

export function refreshConflictNotice(
  reason: string | null,
  nextAllowedLabel: string | null,
): string {
  if (reason === "sync_cooldown") {
    return nextAllowedLabel
      ? `Recently refreshed. Next refresh available ${nextAllowedLabel}. Your numbers are up to date.`
      : "Recently refreshed. Your numbers are up to date.";
  }
  if (reason === "sync_in_flight") {
    return "A refresh is already running.";
  }
  return "Refresh already running or just finished — give it a minute.";
}

function lastErrorCopy(sync: InventorySyncInfo): string {
  switch (sync.last_error_class) {
    case "rate_limited":
      return "Stockforge rate-limited the last refresh. These numbers are still the last successful snapshot.";
    case "auth":
      return "Stockforge rejected the API key. Ask an admin to replace the connector key.";
    case "timeout":
      return "The last Stockforge refresh timed out. These numbers are still the last successful snapshot.";
    default:
      return "The last Stockforge refresh hit a problem. These numbers are still the last successful snapshot.";
  }
}

function csvEscape(value: string): string {
  const safe = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  if (/[",\n]/.test(safe)) return `"${safe.replaceAll('"', '""')}"`;
  return safe;
}

export function buildStockCsv(rows: InventoryStockRow[]): string {
  const header = [
    "name",
    "sku",
    "status",
    "on_hand",
    "reserved",
    "incoming",
    "available",
    "days_of_cover",
    "dead_stock",
    "unit",
    "vendor",
    "category",
    "value_cents",
    "scope",
  ];
  const lines = [header.join(",")];
  for (const row of rows) {
    lines.push(
      [
        csvEscape(row.name),
        csvEscape(row.sku ?? ""),
        row.stock_status,
        String(row.quantity),
        row.reserved_qty === null || row.reserved_qty === undefined ? "" : String(row.reserved_qty),
        row.incoming_qty === null || row.incoming_qty === undefined ? "" : String(row.incoming_qty),
        row.available_qty === null || row.available_qty === undefined ? "" : String(row.available_qty),
        row.days_until_stockout === null || row.days_until_stockout === undefined
          ? ""
          : String(row.days_until_stockout),
        row.dead_stock ? "yes" : "no",
        csvEscape(row.unit ?? ""),
        csvEscape(row.vendor_name ?? ""),
        csvEscape(row.category ?? ""),
        String(row.stock_value_cents),
        row.is_stocked ? "stocked" : "catalog",
      ].join(","),
    );
  }
  return `${lines.join("\n")}\n`;
}

function exportStockCsv(rows: InventoryStockRow[]): void {
  const blob = new Blob([buildStockCsv(rows)], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = "bos-inventory-snapshot.csv";
  anchor.click();
  URL.revokeObjectURL(url);
}

export function uniqueSorted(values: Array<string | null | undefined>): string[] {
  return [...new Set(values.filter((value): value is string => Boolean(value && value.trim())))].sort((left, right) =>
    left.localeCompare(right),
  );
}

function syncNowReason(err: unknown): string | null {
  if (!(err instanceof ApiError) || !err.body || typeof err.body !== "object" || !("reason" in err.body)) {
    return null;
  }
  const reason = (err.body as { reason?: unknown }).reason;
  return typeof reason === "string" ? reason : null;
}

function fmtNextAllowed(ms: number): string {
  const delta = ms - Date.now();
  if (delta <= 0) return "now";
  const minutes = Math.ceil(delta / 60_000);
  return minutes <= 1 ? "in about a minute" : `in about ${minutes} minutes`;
}

function fmtQty(quantity: number, unit: string | null): string {
  const rounded =
    Math.abs(quantity) >= 100 || Number.isInteger(quantity)
      ? quantity.toLocaleString("en-US", { maximumFractionDigits: 0 })
      : quantity.toLocaleString("en-US", { maximumFractionDigits: 2 });
  return unit ? `${rounded} ${unit}` : rounded;
}

/** Unknown reserved/incoming/available is empty. Known zero is 0. */
export function fmtKnownQty(quantity: number | null | undefined, unit: string | null): string {
  return quantity === null || quantity === undefined ? "—" : fmtQty(quantity, unit);
}

/** Missing prediction stays unknown; an explicit Stockforge zero stays 0d. */
export function fmtDaysOfCover(days: number | null | undefined): string {
  return days === null || days === undefined ? "—" : `${days}d`;
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

/** Why an order needs a human, in operator words (worst problem first). */
function orderProblems(row: InventoryOrderRow, staleAfterDays: number): string[] {
  const problems: string[] = [];
  if (row.exception) problems.push("shipping exception");
  if (row.deduction_failed) problems.push("inventory deduction failed");
  if (row.depletion_reversed > 0) problems.push("inventory depletion reversed");
  if (row.needs_mapping) problems.push("unmapped SKU");
  if (row.blocked && row.blocked_reasons.length > 0) {
    problems.push(...row.blocked_reasons);
  } else if (row.blocked) {
    problems.push("blocked");
  }
  if (
    ["NEW", "PICKING", "PACKED"].includes(row.board_status) &&
    row.age_days > staleAfterDays
  ) {
    problems.push(`unshipped for ${row.age_days} days`);
  }
  return [...new Set(problems)];
}

function platformLabel(row: InventoryOrderRow): string {
  return row.platform ? row.platform.toLowerCase() : "source";
}

function mappingLabel(row: InventoryOrderRow): string {
  if (row.needs_mapping) return "unmapped";
  if (row.item_count > 0 && row.mapped_line_count >= row.item_count) return "mapped";
  if (row.mapped_line_count > 0) return `${row.mapped_line_count}/${row.item_count} mapped`;
  return "not checked";
}

function depletionLabel(row: InventoryOrderRow): {
  text: string;
  tone: "critical" | "warning" | "ok" | "neutral";
} {
  if (row.deduction_failed || row.depletion_failed > 0) {
    return { text: "failed", tone: "critical" };
  }
  if (row.depletion_reversed > 0) {
    return { text: "reversed", tone: "warning" };
  }
  if (
    row.deducted ||
    (row.depletion_total > 0 && row.depletion_applied >= row.depletion_total)
  ) {
    return { text: "depleted", tone: "ok" };
  }
  if (row.needs_mapping) return { text: "needs mapping", tone: "warning" };
  if (row.item_count > 0 && row.mapped_line_count >= row.item_count) {
    return { text: "awaiting", tone: "neutral" };
  }
  return { text: "not ready", tone: "neutral" };
}

/** Decorative pipeline stage badge — sequential encoding, not a status tone. */
function orderStageBadge(status: string): { bg: string; text: string; ring: string } {
  switch (status) {
    case "NEW":
      return { bg: "bg-sky-950/60", text: "text-sky-300", ring: "ring-sky-800" };
    case "PICKING":
      return { bg: "bg-indigo-950/60", text: "text-indigo-300", ring: "ring-indigo-800" };
    case "PACKED":
      return { bg: "bg-violet-950/60", text: "text-violet-300", ring: "ring-violet-800" };
    case "SHIPPED":
      return { bg: "bg-teal-950/60", text: "text-teal-300", ring: "ring-teal-800" };
    case "DELIVERED":
      return { bg: "bg-emerald-950/60", text: "text-emerald-300", ring: "ring-emerald-800" };
    default:
      return { bg: "bg-red-950/60", text: "text-red-300", ring: "ring-red-800" };
  }
}

/** The live-board summary strip: count per pipeline stage + exceptions. */
function PipelineStrip({
  orders,
  boardUrl,
}: {
  orders: InventoryOrdersResponse;
  boardUrl: string | null;
}) {
  const counts: Record<string, number> = {
    NEW: orders.pipeline.new_count,
    PICKING: orders.pipeline.picking_count,
    PACKED: orders.pipeline.packed_count,
    SHIPPED: orders.pipeline.shipped_count,
    DELIVERED: orders.pipeline.delivered_count,
  };
  return (
    <div className="surface-card surface-flat surface-body-teal rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
      <div className="flex items-baseline justify-between">
        <div className="text-xs font-semibold uppercase tracking-wide text-zinc-400">
          Order board · last {orders.window_days} days
        </div>
        {boardUrl ? (
          <a
            href={boardUrl}
            target="_blank"
            rel="noreferrer"
            className="text-xs text-sky-400 hover:text-sky-300"
            title="The full interactive board (drag, pack, scan) lives in Stockforge"
          >
            Open this week in Stockforge →
          </a>
        ) : null}
      </div>
      <div className="mt-3 flex items-stretch gap-2">
        {PIPELINE.map((stage, index) => (
          <div key={stage.key} className="flex flex-1 items-center gap-2">
            <div className="flex-1 rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-2">
              <div className="flex items-center gap-1.5">
                <span className={`h-1.5 w-1.5 rounded-full ${stage.color}`} />
                <span className="text-xs uppercase tracking-wide text-zinc-400">
                  {stage.label}
                </span>
              </div>
              <div className="mt-0.5 text-xl font-bold tabular-nums text-zinc-100">
                {counts[stage.key]}
              </div>
            </div>
            {index < PIPELINE.length - 1 ? (
              <span className="text-zinc-700">→</span>
            ) : null}
          </div>
        ))}
        <div className="flex flex-1 items-center">
          <div
            className={`flex-1 rounded-md border px-3 py-2 ${
              orders.pipeline.exception_count > 0
                ? "border-red-800 bg-red-950/30"
                : "border-zinc-800 bg-zinc-950/60"
            }`}
          >
            <div className="flex items-center gap-1.5">
              <span
                className={`h-1.5 w-1.5 rounded-full ${
                  orders.pipeline.exception_count > 0 ? "bg-red-500" : "bg-zinc-600"
                }`}
              />
              <span className="text-xs uppercase tracking-wide text-zinc-400">
                Exceptions
              </span>
            </div>
            <div
              className={`mt-0.5 text-xl font-bold tabular-nums ${
                orders.pipeline.exception_count > 0 ? "text-red-300" : "text-zinc-100"
              }`}
            >
              {orders.pipeline.exception_count}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default function Inventory({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  focusInventoryId,
  onFocusInventoryConsumed,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  focusInventoryId?: string | null;
  onFocusInventoryConsumed?: () => void;
}) {
  const [status, setStatus] = useState<StockforgeConnectorStatus | null>(null);
  const [stock, setStock] = useState<InventoryStockResponse | null>(null);
  const [alerts, setAlerts] = useState<InventoryAlertsResponse | null>(null);
  const [orders, setOrders] = useState<InventoryOrdersResponse | null>(null);
  const [pos, setPos] = useState<InventoryPurchaseOrdersResponse | null>(null);
  const [showAllStock, setShowAllStock] = useState(false);
  const [stockScope, setStockScope] = useState<StockScope>("stocked");
  const [stockStatusFilter, setStockStatusFilter] = useState<StockStatusFilter>("all");
  const [stockQuery, setStockQuery] = useState("");
  const [vendorFilter, setVendorFilter] = useState("all");
  const [categoryFilter, setCategoryFilter] = useState("all");
  const [showAllAlerts, setShowAllAlerts] = useState(false);
  const [showAllReorders, setShowAllReorders] = useState(false);
  const [showAllOrders, setShowAllOrders] = useState(false);
  const [showPos, setShowPos] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const [focusedInventoryId, setFocusedInventoryId] = useState<string | null>(null);
  const [focusedInventorySection, setFocusedInventorySection] = useState<string | null>(null);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  const ordersSectionRef = useRef<HTMLDivElement | null>(null);
  const alertsSectionRef = useRef<HTMLDivElement | null>(null);
  const reorderSectionRef = useRef<HTMLDivElement | null>(null);
  const stockSectionRef = useRef<HTMLDivElement | null>(null);

  const load = useCallback(async () => {
    try {
      const connector = await api.stockforgeStatus();
      setStatus(connector);
      if (connector.configured) {
        const [stockRes, alertsRes, ordersRes, posRes] = await Promise.all([
          api.inventoryStock(),
          api.inventoryAlerts(),
          api.inventoryOrders(),
          api.inventoryPurchaseOrders(),
        ]);
        setStock(stockRes);
        setAlerts(alertsRes);
        setOrders(ordersRes);
        setPos(posRes);
      }
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setLoaded(true);
    }
  }, [onUnauthorized]);

  const sync: InventorySyncInfo | null = stock?.sync ?? orders?.sync ?? null;
  const syncing = sync?.in_flight ?? false;

  usePolling(load, {
    intervalMs: syncing ? SYNCING_POLL_INTERVAL_MS : POLL_INTERVAL_MS,
  });

  const syncNow = async () => {
    setSyncBusy(true);
    setNotice(null);
    try {
      await api.inventorySyncNow();
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        const reason = syncNowReason(err);
        const body = err instanceof ApiError && err.body && typeof err.body === "object" ? err.body : null;
        const nextAllowed =
          body && "next_allowed_at_ms" in body && typeof (body as { next_allowed_at_ms?: unknown }).next_allowed_at_ms === "number"
            ? (body as { next_allowed_at_ms: number }).next_allowed_at_ms
            : null;
        setNotice(
          refreshConflictNotice(
            reason,
            nextAllowed ? fmtNextAllowed(nextAllowed) : null,
          ),
        );
      } else setNotice(errorMessage(err));
    } finally {
      setSyncBusy(false);
    }
  };

  // Command palette integrations — must come before any early return.
  useAppCommand("refresh", () => void load());
  useAppCommand("inventory.sync", () => void syncNow());

  const lastSynced = sync?.last_synced_at_ms ?? null;
  const stale = lastSynced !== null && Date.now() - lastSynced > STALE_AFTER_MS;
  const kpis = stock?.kpis ?? null;
  const lowStockCount =
    (kpis?.warning_count ?? 0) + (kpis?.critical_count ?? 0) + (kpis?.out_of_stock_count ?? 0);
  const attentionRows = (orders?.orders ?? []).filter(
    (row) => orderProblems(row, orders?.controls.stale_after_days ?? 3).length > 0,
  );
  const openOrderCount =
    (orders?.pipeline.new_count ?? 0) +
    (orders?.pipeline.picking_count ?? 0) +
    (orders?.pipeline.packed_count ?? 0);
  const pendingReorders = useMemo(() => alerts?.reorder_suggestions ?? [], [alerts]);
  const activeAlerts = useMemo(() => alerts?.alerts ?? [], [alerts]);
  const visibleAlerts = showAllAlerts
    ? activeAlerts
    : activeAlerts.slice(0, TABLE_PREVIEW_ROWS);
  const visibleReorders = showAllReorders
    ? pendingReorders
    : pendingReorders.slice(0, TABLE_PREVIEW_ROWS);
  const reorderTotal = pendingReorders.reduce(
    (total, row) => total + row.estimated_cost_cents,
    0,
  );
  const boardUrl = status?.order_board_url ?? null;

  const materials = stock?.materials ?? [];
  const scopedMaterials = useMemo(
    () => scopedStockRows(materials, stockScope),
    [materials, stockScope],
  );
  const vendors = useMemo(
    () => uniqueSorted(scopedMaterials.map((row) => row.vendor_name)),
    [scopedMaterials],
  );
  const categories = useMemo(
    () => uniqueSorted(scopedMaterials.map((row) => row.category)),
    [scopedMaterials],
  );
  const filteredStock = useMemo(
    () =>
      filterStockRows(materials, {
        scope: stockScope,
        status: stockStatusFilter,
        vendor: vendorFilter,
        category: categoryFilter,
        query: stockQuery,
      }),
    [categoryFilter, materials, stockQuery, stockScope, stockStatusFilter, vendorFilter],
  );

  useEffect(() => {
    if (vendorFilter !== "all" && !vendors.includes(vendorFilter)) {
      setVendorFilter("all");
    }
  }, [vendorFilter, vendors]);

  useEffect(() => {
    if (categoryFilter !== "all" && !categories.includes(categoryFilter)) {
      setCategoryFilter("all");
    }
  }, [categoryFilter, categories]);
  const visibleStock: InventoryStockRow[] = showAllStock
    ? filteredStock
    : filteredStock.slice(0, TABLE_PREVIEW_ROWS);
  const allOrders = orders?.orders ?? [];
  const visibleOrders = showAllOrders ? allOrders : allOrders.slice(0, TABLE_PREVIEW_ROWS);
  const openPos = pos?.purchase_orders ?? [];

  useEffect(() => {
    if (focusedInventoryId) {
      rowRefs.current.get(focusedInventoryId)?.scrollIntoView({ block: "nearest" });
    }
  }, [focusedInventoryId, showAllAlerts, showAllOrders, showAllReorders, showAllStock]);

  useEffect(() => {
    if (!focusInventoryId || !loaded) return;
    if (focusInventoryId === "orders:blocked") {
      setShowAllOrders(true);
      setFocusedInventorySection(focusInventoryId);
      requestAnimationFrame(() => {
        ordersSectionRef.current?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }
    if (focusInventoryId === "alerts:critical") {
      setFocusedInventorySection(focusInventoryId);
      requestAnimationFrame(() => {
        alertsSectionRef.current?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }
    if (focusInventoryId === "reorder") {
      setFocusedInventorySection(focusInventoryId);
      requestAnimationFrame(() => {
        reorderSectionRef.current?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }
    if (focusInventoryId === "stock:out") {
      setStockScope("stocked");
      setStockStatusFilter("out");
      setShowAllStock(true);
      setFocusedInventorySection(focusInventoryId);
      requestAnimationFrame(() => {
        stockSectionRef.current?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }

    const separator = focusInventoryId.indexOf(":");
    if (separator === -1) {
      onFocusInventoryConsumed?.();
      return;
    }
    const kind = focusInventoryId.slice(0, separator);
    const id = focusInventoryId.slice(separator + 1);
    if (!id) {
      onFocusInventoryConsumed?.();
      return;
    }
    if (kind === "order") {
      const found = allOrders.some((row) => row.order_id === id);
      if (!found) {
        onFocusInventoryConsumed?.();
        return;
      }
      setShowAllOrders(true);
      setFocusedInventorySection(null);
      setFocusedInventoryId(focusInventoryId);
      requestAnimationFrame(() => {
        rowRefs.current.get(focusInventoryId)?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }
    if (kind === "alert") {
      const found = activeAlerts.some((row) => row.alert_id === id);
      if (!found) {
        onFocusInventoryConsumed?.();
        return;
      }
      setShowAllAlerts(true);
      setFocusedInventorySection(null);
      setFocusedInventoryId(focusInventoryId);
      requestAnimationFrame(() => {
        rowRefs.current.get(focusInventoryId)?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }
    if (kind === "material") {
      const found = materials.find((row) => row.material_id === id);
      if (!found) {
        onFocusInventoryConsumed?.();
        return;
      }
      if (!found.is_stocked) setStockScope("catalog");
      setShowAllStock(true);
      setFocusedInventorySection(null);
      setFocusedInventoryId(focusInventoryId);
      requestAnimationFrame(() => {
        rowRefs.current.get(focusInventoryId)?.scrollIntoView({ block: "center" });
      });
      onFocusInventoryConsumed?.();
      return;
    }
    onFocusInventoryConsumed?.();
  }, [
    activeAlerts,
    allOrders,
    focusInventoryId,
    loaded,
    materials,
    onFocusInventoryConsumed,
  ]);

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
        Couldn&apos;t load the inventory view: {error}
      </div>
    );
  }

  if (status && !status.configured) {
    return (
      <div className="mx-auto mt-12 max-w-md">
        <EmptyState
          title="Stockforge isn't connected"
          action={
            status.blocked_reason ? (
              <p className="text-xs text-amber-300">{status.blocked_reason}</p>
            ) : undefined
          }
        >
          Connect the Stockforge inventory system to see stock levels, low-stock
          alerts, and the order board here.
        </EmptyState>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-teal flex items-center justify-between">
        <div className="flex items-center gap-2">
          <div>
            <h2 className="text-lg font-semibold text-zinc-100">Inventory</h2>
            <p className="text-xs text-zinc-500">Stocked report — items you actually stock and monitor.</p>
          </div>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Inventory"
          />
        </div>
        <div className="flex items-center gap-3">
          {status?.inventory_url ? (
            <a
              href={status.inventory_url}
              target="_blank"
              rel="noreferrer"
              className="text-xs text-sky-400 hover:text-sky-300"
            >
              Open in Stockforge →
            </a>
          ) : null}
          <span
            className={`text-xs ${stale ? "text-amber-300" : "text-zinc-400"}`}
            title={`${sync?.material_count ?? 0} materials, ${sync?.order_count ?? 0} orders on file`}
          >
            {syncing
              ? "Updating from Stockforge…"
              : `Updated ${fmtAgo(lastSynced)} from Stockforge`}
          </span>
          <Button
            variant="secondary"
            size="sm"
            busy={syncBusy}
            disabled={syncBusy || syncing}
            onClick={() => void syncNow()}
          >
            {syncBusy ? "Syncing…" : syncing ? "Updating…" : "Refresh"}
          </Button>
        </div>
      </div>

      {notice ? (
        <div className="rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-2 text-sm text-zinc-300">
          {notice}
        </div>
      ) : null}
      {sync?.last_error ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-300">
          <span title={sync.last_error_class ?? undefined}>
            {lastErrorCopy(sync)}
            {sync.last_error_at_ms ? ` As of ${fmtAgo(sync.last_error_at_ms)}.` : ""}
          </span>
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <KpiCard
          hero={attentionRows.length > 0}
          tone={attentionRows.length > 0 ? "warning" : undefined}
          label="Orders needing attention"
          value={String(attentionRows.length)}
          valueCls={attentionRows.length > 0 ? "text-amber-300" : "text-emerald-300"}
          comparison={
            attentionRows.length > 0 ? (
              <span className="text-amber-300/90">
                {orders?.controls.needs_mapping_count
                  ? `${orders.controls.needs_mapping_count} unmapped SKU · `
                  : ""}
                {orders?.controls.deduction_failed_count
                  ? `${orders.controls.deduction_failed_count} deduction failed · `
                  : ""}
                {orders?.controls.stale_count
                  ? `${orders.controls.stale_count} sitting unshipped`
                  : ""}
              </span>
            ) : (
              "Every order is moving — nice."
            )
          }
          footnote="Blocked, unmapped, failed-deduction, or stale orders from the live board"
        />
        <KpiCard
          label="Low stock"
          value={String(lowStockCount)}
          valueCls={
            (kpis?.out_of_stock_count ?? 0) > 0 || (kpis?.critical_count ?? 0) > 0
              ? "text-red-300"
              : lowStockCount > 0
                ? "text-amber-300"
                : "text-zinc-100"
          }
          comparison={
            lowStockCount > 0
              ? `${kpis?.out_of_stock_count ?? 0} out · ${kpis?.critical_count ?? 0} critical · ${
                  kpis?.warning_count ?? 0
                } low`
              : "All monitored materials above their thresholds"
          }
          footnote={`${kpis?.monitored_materials ?? 0} monitored · ${
            kpis?.not_monitored_count ?? 0
          } excluded by Stockforge behavior`}
        />
        <KpiCard
          label="Shopify orders"
          value={String(orders?.controls.shopify_order_count ?? 0)}
          comparison={`${orders?.controls.mapped_count ?? 0} mapped · ${
            orders?.controls.depleted_count ?? 0
          } depleted · ${orders?.controls.awaiting_depletion_count ?? 0} awaiting`}
          footnote={`${openOrderCount} not yet shipped · BOS keeps a ${
            orders?.window_days ?? 30
          }-day window`}
        />
        <KpiCard
          label="Inventory value"
          value={fmtMoneyCompact(kpis?.stock_value_cents ?? 0)}
          comparison={
            openPos.length > 0
              ? `${fmtMoneyCompact(pos?.open_total_cents ?? 0)} inbound on ${openPos.length} PO${
                  openPos.length === 1 ? "" : "s"
                }`
              : "No open purchase orders"
          }
          footnote={`${kpis?.monitored_materials ?? 0} stocked materials at cost`}
        />
      </div>

      {orders ? <PipelineStrip orders={orders} boardUrl={boardUrl} /> : null}

      <div className="grid gap-4 lg:grid-cols-2">
        <div
          ref={alertsSectionRef}
          className={`surface-card surface-flat surface-body-teal rounded-lg border bg-zinc-900/40 p-4 ${
            focusedInventorySection === "alerts:critical"
              ? "border-amber-500/70"
              : "border-zinc-800"
          }`}
        >
          <div className="text-sm font-semibold text-zinc-200">Low-stock alerts</div>
          {(alerts?.alerts.length ?? 0) === 0 ? (
            <p className="mt-3 text-sm text-zinc-400">
              No active alerts — monitored stock is above every threshold.
            </p>
          ) : (
            <div className="mt-2 flex flex-col gap-1.5">
              {visibleAlerts.map((alert) => (
                <div
                  key={alert.alert_id}
                  ref={(el) => {
                    const refKey = `alert:${alert.alert_id}`;
                    if (el) rowRefs.current.set(refKey, el);
                    else rowRefs.current.delete(refKey);
                  }}
                  className={`flex items-center gap-2 rounded px-1.5 py-1 text-sm ${
                    focusedInventoryId === `alert:${alert.alert_id}` ? "bg-zinc-800/70" : ""
                  }`}
                  title={alert.message ?? undefined}
                >
                  <span
                    className={`h-2 w-2 shrink-0 rounded-full ${
                      alert.severity === "CRITICAL" ? "bg-red-500" : "bg-amber-500"
                    }`}
                  />
                  <span className="flex-1 truncate text-zinc-200">
                    <StockforgeName href={alert.external_url}>
                      {alert.material_name ?? alert.material_sku ?? "material"}
                    </StockforgeName>
                  </span>
                  {alert.quantity !== null && alert.quantity !== undefined ? (
                    <span className="tabular-nums text-zinc-400">
                      {fmtQty(alert.quantity, null)} left
                    </span>
                  ) : null}
                  {alert.percentage_remaining !== null &&
                  alert.percentage_remaining !== undefined ? (
                    <span className="w-12 text-right tabular-nums text-zinc-400">
                      {Math.round(alert.percentage_remaining)}%
                    </span>
                  ) : null}
                </div>
              ))}
            </div>
          )}
          {activeAlerts.length > TABLE_PREVIEW_ROWS ? (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowAllAlerts((open) => !open)}
              className="mt-2 w-full"
            >
              {showAllAlerts ? "Show fewer" : `Show all ${activeAlerts.length} alerts`}
            </Button>
          ) : null}
        </div>

        <div
          ref={reorderSectionRef}
          className={`surface-card surface-flat surface-body-teal rounded-lg border bg-zinc-900/40 p-4 ${
            focusedInventorySection === "reorder"
              ? "border-amber-500/70"
              : "border-zinc-800"
          }`}
        >
          <div className="flex items-baseline justify-between">
            <div className="text-sm font-semibold text-zinc-200">Reorder suggestions</div>
            {pendingReorders.length > 0 ? (
              <div className="text-xs text-zinc-400">
                ≈{fmtMoneyCompact(reorderTotal)} to restock
              </div>
            ) : null}
          </div>
          {pendingReorders.length === 0 ? (
            <p className="mt-3 text-sm text-zinc-400">
              Nothing to reorder right now.
            </p>
          ) : (
            <div className="mt-2 flex flex-col gap-1.5">
              {visibleReorders.map((row) => (
                <div
                  key={row.suggestion_id}
                  className="flex items-center gap-2 rounded px-1.5 py-1 text-sm"
                  title={row.reasoning ?? undefined}
                >
                  <StatusBadge
                    tone={
                      row.urgency === "CRITICAL"
                        ? "critical"
                        : row.urgency === "HIGH"
                          ? "warning"
                          : "neutral"
                    }
                  >
                    {row.urgency.toLowerCase()}
                  </StatusBadge>
                  <span className="flex-1 truncate text-zinc-200">
                    <StockforgeName href={row.external_url}>
                      {row.material_name ?? row.material_sku ?? "material"}
                    </StockforgeName>
                  </span>
                  {row.days_until_stockout !== null &&
                  row.days_until_stockout !== undefined ? (
                    <span className="tabular-nums text-zinc-400">
                      {Math.round(row.days_until_stockout)}d left
                    </span>
                  ) : null}
                  <span className="tabular-nums text-zinc-300">
                    {row.suggested_quantity !== null && row.suggested_quantity !== undefined
                      ? fmtQty(row.suggested_quantity, row.unit ?? null)
                      : "—"}
                  </span>
                  <span className="w-16 text-right tabular-nums text-zinc-400">
                    {fmtMoneyCompact(row.estimated_cost_cents)}
                  </span>
                </div>
              ))}
            </div>
          )}
          {pendingReorders.length > TABLE_PREVIEW_ROWS ? (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowAllReorders((open) => !open)}
              className="mt-2 w-full"
            >
              {showAllReorders
                ? "Show fewer"
                : `Show all ${pendingReorders.length} suggestions`}
            </Button>
          ) : null}
        </div>
      </div>

      <div
        ref={ordersSectionRef}
        className={`surface-card surface-flat surface-body-teal rounded-lg border ${
          focusedInventorySection === "orders:blocked"
            ? "border-amber-500/70"
            : "border-zinc-800"
        }`}
      >
        <div className="surface-head-teal flex items-center gap-2 border-b border-zinc-800 px-3 py-2">
          <span className="text-sm font-semibold text-zinc-200">Orders</span>
          <span className="ml-auto text-xs text-zinc-400">
            needs-attention first · last {orders?.window_days ?? 30} days
          </span>
        </div>
        {visibleOrders.length === 0 ? (
          <div className="p-8">
            <EmptyState
              title={
                sync && !sync.backfill_complete
                  ? "Orders are still loading from Stockforge"
                  : "No orders in the window"
              }
            >
              {sync && !sync.backfill_complete
                ? "Check back in a minute or hit Refresh."
                : undefined}
            </EmptyState>
          </div>
        ) : (
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-teal`}>
              <tr>
                <th className={cellCls}>Order</th>
                <th className={cellCls}>Channel</th>
                <th className={cellCls}>Customer</th>
                <th className={cellCls}>Status</th>
                <th className={cellCls}>Depletion</th>
                <th className={cellCls}>Needs</th>
                <th className={numCellCls}>Total</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {visibleOrders.map((row) => {
                const problems = orderProblems(
                  row,
                  orders?.controls.stale_after_days ?? 3,
                );
                const stage = orderStageBadge(row.board_status);
                const depletion = depletionLabel(row);
                return (
                  <tr
                    key={row.order_id}
                    ref={(el) => {
                      const refKey = `order:${row.order_id}`;
                      if (el) rowRefs.current.set(refKey, el);
                      else rowRefs.current.delete(refKey);
                    }}
                    className={`${rowHoverCls} ${
                      focusedInventoryId === `order:${row.order_id}` ? "bg-zinc-900/60" : ""
                    }`}
                    title={`${row.platform ?? ""} · ordered ${row.order_date?.slice(0, 10) ?? "—"} · ${
                      row.item_count
                    } line${row.item_count === 1 ? "" : "s"}${
                      row.tracking_number
                        ? ` · ${row.carrier ?? ""} ${row.tracking_number}`
                        : ""
                    }`}
                  >
                    <td className={`${cellCls} font-mono text-xs text-zinc-300`}>
                      <StockforgeName href={row.external_url}>{row.order_number}</StockforgeName>
                    </td>
                    <td className={cellCls}>
                      <div className="flex flex-col gap-0.5">
                        <span className="text-xs font-semibold uppercase tracking-wide text-zinc-300">
                          {platformLabel(row)}
                        </span>
                        <span className="text-[11px] text-zinc-500">
                          {row.external_order_id ?? "—"}
                        </span>
                      </div>
                    </td>
                    <td className={`${cellCls} text-zinc-200`}>
                      <div className="flex flex-col gap-0.5">
                        <span>{row.customer_name ?? "—"}</span>
                        {row.customer_email ? (
                          <span className="text-[11px] text-zinc-500">
                            {row.customer_email}
                          </span>
                        ) : null}
                      </div>
                    </td>
                    <td className={cellCls}>
                      <span
                        className={`rounded-full px-2 py-0.5 text-xs font-semibold uppercase tracking-wide ring-1 ring-inset ${stage.bg} ${stage.text} ${stage.ring}`}
                      >
                        {row.board_status.toLowerCase()}
                      </span>
                    </td>
                    <td className={cellCls}>
                      <div className="flex flex-col gap-1">
                        <StatusBadge tone={depletion.tone}>
                          {depletion.text}
                        </StatusBadge>
                        <span className="text-[11px] text-zinc-500">
                          {mappingLabel(row)}
                        </span>
                      </div>
                    </td>
                    <td className={cellCls}>
                      {problems.length > 0 ? (
                        <span className="text-xs text-red-300">
                          {problems.join(" · ")}
                        </span>
                      ) : (
                        <span className="text-xs text-zinc-500">—</span>
                      )}
                    </td>
                    <td className={`${numCellCls} text-zinc-200`}>
                      {fmtMoney(row.total_cents)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {allOrders.length > TABLE_PREVIEW_ROWS ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowAllOrders((open) => !open)}
            className="w-full border-t border-zinc-800 rounded-none rounded-b-lg"
          >
            {showAllOrders ? "Show fewer" : `Show all ${allOrders.length} orders`}
          </Button>
        ) : null}
      </div>

      <div
        ref={stockSectionRef}
        className={`surface-card surface-flat surface-body-teal rounded-lg border ${
          focusedInventorySection === "stock:out"
            ? "border-amber-500/70"
            : "border-zinc-800"
        }`}
      >
        <div className="surface-head-teal flex flex-col gap-2 border-b border-zinc-800 px-3 py-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold text-zinc-200">Stock on hand</span>
            <span className="text-xs text-zinc-400">
              {stockScope === "stocked" ? "stocked report" : "full catalog"} · problems first
            </span>
            <div className="ml-auto flex flex-wrap items-center gap-2">
              <div className="inline-flex rounded-md border border-zinc-800 p-0.5 text-xs" role="group" aria-label="Stocked or catalog">
                <button type="button" className={`rounded px-2 py-1 ${stockScope === "stocked" ? "bg-zinc-800 text-zinc-100" : "text-zinc-400"}`} aria-pressed={stockScope === "stocked"} onClick={() => setStockScope("stocked")}>
                  Stocked
                </button>
                <button type="button" className={`rounded px-2 py-1 ${stockScope === "catalog" ? "bg-zinc-800 text-zinc-100" : "text-zinc-400"}`} aria-pressed={stockScope === "catalog"} onClick={() => setStockScope("catalog")}>
                  Catalog
                </button>
              </div>
              <Button variant="ghost" size="sm" onClick={() => exportStockCsv(filteredStock)}>
                Export CSV
              </Button>
              {status?.inventory_url ? (
                <a href={status.inventory_url} target="_blank" rel="noreferrer" className="text-xs text-sky-400 hover:text-sky-300">
                  Full list / export in Stockforge →
                </a>
              ) : null}
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <input className="h-8 min-w-[12rem] flex-1 rounded-md border border-zinc-800 bg-zinc-950/60 px-2 text-sm text-zinc-100 placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-sky-500/70" value={stockQuery} onChange={(event) => setStockQuery(event.target.value)} placeholder="Search name or SKU" aria-label="Search stock by name or SKU" />
            <select className="h-8 rounded-md border border-zinc-800 bg-zinc-950/60 px-2 text-xs text-zinc-200" value={stockStatusFilter} onChange={(event) => setStockStatusFilter(event.target.value as StockStatusFilter)} aria-label="Filter by stock status">
              <option value="all">All statuses</option>
              <option value="attention">Needs attention</option>
              <option value="out">Out</option>
              <option value="critical">Critical</option>
              <option value="warning">Warning</option>
            </select>
            <select className="h-8 rounded-md border border-zinc-800 bg-zinc-950/60 px-2 text-xs text-zinc-200" value={vendorFilter} onChange={(event) => setVendorFilter(event.target.value)} aria-label="Filter by vendor">
              <option value="all">All vendors</option>
              {vendors.map((vendor) => <option key={vendor} value={vendor}>{vendor}</option>)}
            </select>
            <select className="h-8 rounded-md border border-zinc-800 bg-zinc-950/60 px-2 text-xs text-zinc-200" value={categoryFilter} onChange={(event) => setCategoryFilter(event.target.value)} aria-label="Filter by category">
              <option value="all">All categories</option>
              {categories.map((category) => <option key={category} value={category}>{category}</option>)}
            </select>
          </div>
        </div>
        {visibleStock.length === 0 ? (
          <div className="p-8">
            <EmptyState
              title={
                sync && !sync.backfill_complete
                  ? "Materials are still loading from Stockforge"
                  : stockScope === "stocked"
                    ? "No stocked materials match this report"
                    : "No materials on file yet"
              }
            >
              {sync && !sync.backfill_complete
                ? "Check back in a minute or hit Refresh."
                : stockScope === "stocked"
                  ? "This is the stocked report. Switch to Catalog or open the full list in Stockforge."
                  : undefined}
            </EmptyState>
          </div>
        ) : (
          <div className="overflow-x-auto">
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-teal`}>
              <tr>
                <th className={cellCls}>Material</th>
                <th className={cellCls}>SKU</th>
                <th className={numCellCls}>On hand</th>
                <th className={numCellCls}>Reserved</th>
                <th className={numCellCls}>Incoming</th>
                <th className={numCellCls}>Available</th>
                <th className={numCellCls}>Days cover</th>
                <th className={cellCls}>Status</th>
                <th className={cellCls}>Vendor</th>
                <th className={numCellCls}>Value</th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {visibleStock.map((row) => {
                const notMonitoredLabel =
                  row.replenishment_policy === "PRODUCTION" &&
                  row.sale_depletion_policy === "COMPONENTS"
                    ? "built to order"
                    : row.replenishment_policy === "PRODUCTION"
                      ? "production"
                      : row.replenishment_policy === "NONE"
                        ? "not replenished"
                        : "not monitored";
                const tone =
                  row.stock_status === "out" || row.stock_status === "critical"
                    ? "critical"
                    : row.stock_status === "warning"
                      ? "warning"
                      : "neutral";
                const label =
                  row.stock_status === "out"
                    ? "out of stock"
                    : row.stock_status === "critical"
                      ? "critical"
                      : row.stock_status === "warning"
                        ? "low"
                        : row.stock_status === "not_monitored"
                          ? notMonitoredLabel
                          : "ok";
                return (
                  <tr
                    key={row.material_id}
                    ref={(el) => {
                      const refKey = `material:${row.material_id}`;
                      if (el) rowRefs.current.set(refKey, el);
                      else rowRefs.current.delete(refKey);
                    }}
                    className={`${rowHoverCls} ${
                      focusedInventoryId === `material:${row.material_id}` ? "bg-zinc-900/60" : ""
                    }`}
                    title={
                      row.stock_status === "not_monitored"
                        ? `Excluded from low-stock alerts · replenishment ${
                            row.replenishment_policy?.toLowerCase() ?? "not configured"
                          } · ${row.is_purchasable ? "purchasable" : "not purchasable"}`
                        : row.warning_threshold !== null && row.warning_threshold !== undefined
                        ? `Warn at ${fmtQty(row.warning_threshold, row.unit ?? null)}${
                            row.critical_threshold !== null &&
                            row.critical_threshold !== undefined
                              ? ` · critical at ${fmtQty(row.critical_threshold, row.unit ?? null)}`
                              : ""
                          }${
                            row.lead_time_days !== null && row.lead_time_days !== undefined
                              ? ` · ${row.lead_time_days}d lead time`
                              : ""
                          }`
                          : undefined
                    }
                  >
                    <td className={`${cellCls} text-zinc-200`}>
                      <StockforgeName href={row.external_url}>{row.name}</StockforgeName>
                    </td>
                    <td className={`${cellCls} font-mono text-xs text-zinc-400`}>
                      {row.sku ?? "—"}
                    </td>
                    <td className={`${numCellCls} text-zinc-200`}>
                      {fmtQty(row.quantity, row.unit ?? null)}
                    </td>
                    <td className={`${numCellCls} text-zinc-400`}>
                      {fmtKnownQty(row.reserved_qty, row.unit ?? null)}
                    </td>
                    <td className={`${numCellCls} text-zinc-400`}>
                      {fmtKnownQty(row.incoming_qty, row.unit ?? null)}
                    </td>
                    <td className={`${numCellCls} text-zinc-200`}>
                      {fmtKnownQty(row.available_qty, row.unit ?? null)}
                    </td>
                    <td className={`${numCellCls} text-zinc-400`}>
                      {fmtDaysOfCover(row.days_until_stockout)}
                    </td>
                    <td className={cellCls}>
                      <div className="flex flex-wrap items-center gap-1.5">
                        <StatusBadge tone={tone}>{label}</StatusBadge>
                        {row.dead_stock ? (
                          <StatusBadge tone="warning">dead stock</StatusBadge>
                        ) : null}
                      </div>
                    </td>
                    <td className={`${cellCls} text-zinc-400`}>{row.vendor_name ?? "—"}</td>
                    <td className={`${numCellCls} text-zinc-400`}>
                      {fmtMoney(row.stock_value_cents)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          </div>
        )}
        {filteredStock.length > visibleStock.length || showAllStock ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowAllStock((open) => !open)}
            className="w-full border-t border-zinc-800 rounded-none rounded-b-lg"
          >
            {showAllStock ? "Show fewer" : `Show all ${filteredStock.length} ${stockScope === "stocked" ? "stocked " : ""}materials`}
          </Button>
        ) : null}
      </div>

      <div className="surface-card surface-flat surface-body-teal rounded-lg border border-zinc-800">
        <button
          onClick={() => setShowPos((open) => !open)}
          className="surface-head-teal flex w-full items-center justify-between px-3 py-2 text-sm font-semibold text-zinc-200 hover:bg-zinc-900/60"
        >
          <span>
            Inbound purchase orders ({openPos.length}
            {openPos.length > 0 ? ` · ${fmtMoneyCompact(pos?.open_total_cents ?? 0)}` : ""})
          </span>
          <span className="text-xs text-zinc-400">{showPos ? "Hide" : "Show"}</span>
        </button>
        {showPos ? (
          openPos.length === 0 ? (
            <div className="border-t border-zinc-800 p-6">
              <EmptyState title="No open purchase orders." />
            </div>
          ) : (
            <table className={`${tableCls} border-t border-zinc-800`}>
              <thead className={`${theadCls} surface-head-teal`}>
                <tr>
                  <th className={cellCls}>Vendor</th>
                  <th className={cellCls}>Status</th>
                  <th className={cellCls}>Freight</th>
                  <th className={numCellCls}>Lines</th>
                  <th className={numCellCls}>Total</th>
                </tr>
              </thead>
              <tbody className={rowDivideCls}>
                {openPos.map((row) => (
                  <tr key={row.po_id} className={rowHoverCls}>
                    <td className={`${cellCls} text-zinc-200`}>
                      <StockforgeName href={row.external_url}>{row.vendor_name ?? "PO"}</StockforgeName>
                    </td>
                    <td className={cellCls}>
                      <StatusBadge tone="neutral">
                        {row.status.replace(/_/g, " ").toLowerCase()}
                      </StatusBadge>
                    </td>
                    <td className={`${cellCls} text-zinc-400`}>
                      {row.freight_mode?.replace(/_/g, " ").toLowerCase() ?? "—"}
                    </td>
                    <td className={`${numCellCls} text-zinc-400`}>{row.line_count}</td>
                    <td className={`${numCellCls} text-zinc-200`}>
                      {fmtMoney(row.total_cents)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )
        ) : null}
      </div>
    </div>
  );
}
