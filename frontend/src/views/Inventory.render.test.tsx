import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InventoryAlertsResponse } from "../types/generated/InventoryAlertsResponse";
import type { InventoryOrdersResponse } from "../types/generated/InventoryOrdersResponse";
import type { InventoryPurchaseOrdersResponse } from "../types/generated/InventoryPurchaseOrdersResponse";
import type { InventoryStockResponse } from "../types/generated/InventoryStockResponse";
import type { InventorySyncInfo } from "../types/generated/InventorySyncInfo";
import type { StockforgeConnectorStatus } from "../types/generated/StockforgeConnectorStatus";

/**
 * Inventory.tsx useState order (keep in lockstep with the component):
 * status, stock, alerts, orders, pos, showAllStock, stockScope,
 * stockStatusFilter, stockQuery, vendorFilter, categoryFilter,
 * showAllAlerts, showAllReorders, showAllOrders, showPos, loaded,
 * error, notice, syncBusy, focusedInventoryId, focusedInventorySection.
 */
const INVENTORY_USE_STATE_COUNT = 21;

const hookState = vi.hoisted(() => ({
  index: 0,
  values: [] as unknown[],
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: vi.fn((callback: unknown) => callback),
    useEffect: vi.fn(),
    useMemo: vi.fn((factory: () => unknown) => factory()),
    useRef: vi.fn((initial: unknown) => ({ current: initial })),
    useState: vi.fn((initial: unknown) => {
      const index = hookState.index;
      hookState.index += 1;
      const value = index < hookState.values.length ? hookState.values[index] : initial;
      return [value, vi.fn()];
    }),
  };
});

import Inventory from "./Inventory";

const sync: InventorySyncInfo = {
  sync_enabled: true,
  in_flight: false,
  backfill_complete: true,
  last_synced_at_ms: 1_700_000_000_000,
  material_count: 5,
  order_count: 4,
  last_requests_used: 8,
  next_sync_allowed_at_ms: 1_700_000_060_000,
};

const connector: StockforgeConnectorStatus = {
  configured: true,
  base_url: "https://stockforge.example.test",
  order_board_url: "https://stockforge.example.test/orders",
  inventory_url: "https://stockforge.example.test/materials",
  has_synced: true,
};

const stock: InventoryStockResponse = {
  sync,
  kpis: {
    active_materials: 5,
    monitored_materials: 4,
    not_monitored_count: 1,
    warning_count: 1,
    critical_count: 1,
    out_of_stock_count: 1,
    stock_value_cents: 5_250_000,
    catalog_value_cents: 5_300_000,
  },
  materials: [
    {
      material_id: "out",
      name: "Out of stock mugs",
      sku: "OUT-1",
      category: "LIQUID",
      quantity: 0,
      reserved_qty: 0,
      incoming_qty: 12,
      available_qty: 0,
      days_until_stockout: 0,
      unit: "gal",
      stock_status: "out",
      is_purchasable: true,
      replenishment_policy: "PURCHASE",
      sale_depletion_policy: "STOCK",
      warning_threshold: 20,
      critical_threshold: 10,
      stock_value_cents: 0,
      vendor_name: "North Mill",
      lead_time_days: 5,
      is_stocked: true,
      dead_stock: false,
      external_url: "https://stockforge.example.test/materials/out",
    },
    {
      material_id: "critical",
      name: "Critical resin",
      sku: "CRIT-1",
      quantity: 4.5,
      stock_status: "critical",
      stock_value_cents: 20_000,
      is_stocked: true,
      dead_stock: false,
    },
    {
      material_id: "warning",
      name: "Low pigment",
      quantity: 9,
      stock_status: "warning",
      stock_value_cents: 30_000,
      is_stocked: true,
      dead_stock: true,
    },
    {
      material_id: "production",
      name: "Built-to-order kit",
      quantity: 1,
      stock_status: "not_monitored",
      is_purchasable: false,
      replenishment_policy: "PRODUCTION",
      sale_depletion_policy: "COMPONENTS",
      stock_value_cents: 100,
      is_stocked: false,
      dead_stock: false,
    },
    {
      material_id: "ok",
      name: "Healthy stock",
      quantity: 125,
      stock_status: "ok",
      stock_value_cents: 5_200_000,
      is_stocked: true,
      dead_stock: false,
    },
  ],
};

const alerts: InventoryAlertsResponse = {
  sync,
  alerts: [
    {
      alert_id: "critical-alert",
      material_name: "Out of stock mugs",
      severity: "CRITICAL",
      quantity: 0,
      percentage_remaining: 0,
      message: "Restock now",
      external_url: "https://stockforge.example.test/alerts/critical",
    },
    {
      alert_id: "warning-alert",
      material_sku: "WARN-1",
      severity: "WARNING",
    },
  ],
  reorder_suggestions: [
    {
      suggestion_id: "critical-reorder",
      material_name: "Out of stock mugs",
      urgency: "CRITICAL",
      days_until_stockout: 0,
      suggested_quantity: 20,
      unit: "gal",
      estimated_cost_cents: 45_000,
      reasoning: "Below the critical threshold",
    },
    {
      suggestion_id: "high-reorder",
      material_sku: "CRIT-1",
      urgency: "HIGH",
      estimated_cost_cents: 25_000,
    },
    {
      suggestion_id: "medium-reorder",
      urgency: "MEDIUM",
      estimated_cost_cents: 10_000,
    },
  ],
};

function order(
  orderId: string,
  boardStatus: string,
  overrides: Partial<InventoryOrdersResponse["orders"][number]> = {},
): InventoryOrdersResponse["orders"][number] {
  return {
    order_id: orderId,
    order_number: `#${orderId}`,
    platform: "SHOPIFY",
    board_status: boardStatus,
    total_cents: 12_500,
    item_count: 2,
    unit_count: 3,
    mapped_line_count: 2,
    age_days: 0,
    needs_mapping: false,
    blocked: false,
    deducted: true,
    deduction_failed: false,
    exception: false,
    depletion_total: 2,
    depletion_applied: 2,
    depletion_failed: 0,
    depletion_reversed: 0,
    blocked_reasons: [],
    ...overrides,
  };
}

const orders: InventoryOrdersResponse = {
  sync,
  window_days: 30,
  pipeline: {
    new_count: 1,
    picking_count: 1,
    packed_count: 1,
    shipped_count: 1,
    delivered_count: 1,
    exception_count: 1,
  },
  controls: {
    shopify_order_count: 6,
    mapped_count: 4,
    depleted_count: 2,
    awaiting_depletion_count: 1,
    needs_mapping_count: 1,
    deduction_failed_count: 1,
    blocked_count: 1,
    stale_count: 1,
    stale_after_days: 3,
  },
  orders: [
    order("new", "NEW", {
      customer_name: "Ada Lovelace",
      customer_email: "ada@example.test",
      age_days: 7,
      needs_mapping: true,
      mapped_line_count: 0,
      deducted: false,
    }),
    order("picking", "PICKING", {
      blocked: true,
      blocked_reasons: ["awaiting label"],
      deducted: false,
      depletion_applied: 0,
    }),
    order("packed", "PACKED", {
      deduction_failed: true,
      depletion_failed: 1,
      deducted: false,
    }),
    order("shipped", "SHIPPED", {
      depletion_reversed: 1,
      tracking_number: "TRACK-1",
      carrier: "UPS",
    }),
    order("delivered", "DELIVERED"),
    order("exception", "EXCEPTION", { exception: true, blocked: true }),
  ],
};

const purchaseOrders: InventoryPurchaseOrdersResponse = {
  sync,
  open_total_cents: 120_000,
  purchase_orders: [
    {
      po_id: "po-1",
      vendor_name: "North Mill",
      status: "PENDING_APPROVAL",
      total_cents: 120_000,
      freight_mode: "LESS_THAN_TRUCKLOAD",
      line_count: 3,
      external_url: "https://stockforge.example.test/purchase-orders/po-1",
    },
  ],
};

type StateOptions = {
  status?: StockforgeConnectorStatus | null;
  stock?: InventoryStockResponse | null;
  alerts?: InventoryAlertsResponse | null;
  orders?: InventoryOrdersResponse | null;
  purchaseOrders?: InventoryPurchaseOrdersResponse | null;
  loaded?: boolean;
  error?: string | null;
  notice?: string | null;
  showAll?: boolean;
  showPurchaseOrders?: boolean;
  stockScope?: "stocked" | "catalog";
};

function arrangeState(options: StateOptions = {}): void {
  const showAll = options.showAll ?? true;
  hookState.index = 0;
  hookState.values = [
    options.status === undefined ? connector : options.status,
    options.stock === undefined ? stock : options.stock,
    options.alerts === undefined ? alerts : options.alerts,
    options.orders === undefined ? orders : options.orders,
    options.purchaseOrders === undefined ? purchaseOrders : options.purchaseOrders,
    showAll,
    options.stockScope ?? "catalog",
    "all",
    "",
    "all",
    "all",
    showAll,
    showAll,
    showAll,
    options.showPurchaseOrders ?? true,
    options.loaded ?? true,
    options.error ?? null,
    options.notice ?? "Refresh accepted",
    false,
    null,
    null,
  ];
  if (hookState.values.length !== INVENTORY_USE_STATE_COUNT) {
    throw new Error(
      `arrangeState must supply ${INVENTORY_USE_STATE_COUNT} useState values (Inventory.tsx hook order)`,
    );
  }
}

function renderInventory(): string {
  hookState.index = 0;
  const html = renderToStaticMarkup(
    createElement(Inventory, {
      onUnauthorized: vi.fn(),
      helpTopicId: "inventory",
      onOpenHelpTopic: vi.fn(),
    }),
  );
  expect(hookState.index).toBe(INVENTORY_USE_STATE_COUNT);
  return html;
}

describe("Inventory render states", () => {
  beforeEach(() => {
    hookState.index = 0;
    hookState.values = [];
  });

  it("renders its loading, error, and disconnected states", () => {
    arrangeState({ loaded: false });
    expect(renderInventory()).toContain("animate-pulse");

    arrangeState({ error: "Stockforge is unavailable" });
    const failed = renderInventory();
    expect(failed).toContain("Couldn&#x27;t load the inventory view");
    expect(failed).toContain("Stockforge is unavailable");

    arrangeState({
      status: {
        configured: false,
        has_synced: false,
        blocked_reason: "Add the connector key",
      },
    });
    const disconnected = renderInventory();
    expect(disconnected).toContain("Stockforge isn&#x27;t connected");
    expect(disconnected).toContain("Add the connector key");
  });

  it("renders the populated operations dashboard with every table", () => {
    arrangeState();
    const html = renderInventory();

    expect(html).toContain("Orders needing attention");
    expect(html).toContain("Out of stock mugs");
    expect(html).toContain("Ada Lovelace");
    expect(html).toContain("deduction failed");
    expect(html).toContain("dead stock");
    expect(html).toContain("Built-to-order kit");
    expect(html).toContain("Inbound purchase orders (1");
    expect(html).toContain("North Mill");
    expect(html).toContain("Refresh accepted");
    expect(html).toContain("Restock now");
    expect(html).toContain("TRACK-1");
    expect(html).toContain("$1,200.00");
  });

  it("renders honest empty and incomplete-backfill copy", () => {
    const incompleteSync = { ...sync, backfill_complete: false };
    arrangeState({
      stock: {
        ...stock,
        materials: [],
        sync: incompleteSync,
      },
      alerts: {
        alerts: [],
        reorder_suggestions: [],
        sync: incompleteSync,
      },
      orders: {
        ...orders,
        orders: [],
        sync: incompleteSync,
      },
      purchaseOrders: {
        purchase_orders: [],
        open_total_cents: 0,
        sync: incompleteSync,
      },
      showPurchaseOrders: true,
    });
    const html = renderInventory();

    expect(html).toContain("No active alerts");
    expect(html).toContain("Nothing to reorder right now");
    expect(html).toContain("Orders are still loading from Stockforge");
    expect(html).toContain("Materials are still loading from Stockforge");
    expect(html).toContain("No open purchase orders");
  });
});
