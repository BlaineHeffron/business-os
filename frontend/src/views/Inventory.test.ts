import { describe, expect, it } from "vitest";
import type { InventoryStockRow } from "../types/generated/InventoryStockRow";
import {
  buildStockCsv,
  filterStockRows,
  fmtDaysOfCover,
  fmtKnownQty,
  refreshConflictNotice,
  scopedStockRows,
  uniqueSorted,
} from "./Inventory";

function row(partial: Partial<InventoryStockRow> & Pick<InventoryStockRow, "material_id">): InventoryStockRow {
  return {
    name: partial.material_id,
    quantity: 1,
    reserved_qty: 0,
    incoming_qty: 0,
    available_qty: 1,
    days_until_stockout: null,
    stock_status: "ok",
    stock_value_cents: 100,
    is_stocked: true,
    dead_stock: false,
    ...partial,
  };
}

describe("inventory stocked report filters", () => {
  const stocked = row({
    material_id: "mug",
    name: "example Blue",
    sku: "QB-1",
    vendor_name: "Mill",
    category: "LIQUID",
    stock_status: "out",
    is_stocked: true,
  });
  const catalog = row({
    material_id: "kit",
    name: "Cabinet kit",
    sku: "KIT-9",
    vendor_name: "Shop",
    category: "DISCRETE",
    stock_status: "not_monitored",
    is_stocked: false,
  });

  it("defaults to stocked rows and hides catalog kits", () => {
    expect(scopedStockRows([stocked, catalog], "stocked").map((item) => item.material_id)).toEqual([
      "mug",
    ]);
    expect(filterStockRows([stocked, catalog], {
      scope: "stocked",
      status: "all",
      vendor: "all",
      category: "all",
      query: "",
    }).map((item) => item.material_id)).toEqual(["mug"]);
  });

  it("shows catalog rows only when the catalog scope is selected", () => {
    expect(
      filterStockRows([stocked, catalog], {
        scope: "catalog",
        status: "all",
        vendor: "all",
        category: "all",
        query: "",
      }).map((item) => item.material_id),
    ).toEqual(["mug", "kit"]);
  });

  it("needs-attention matches the low-stock KPI, warning is warning-only", () => {
    const warning = row({ material_id: "warn", stock_status: "warning" });
    const critical = row({ material_id: "crit", stock_status: "critical" });
    const ok = row({ material_id: "ok", stock_status: "ok" });
    const options = {
      scope: "stocked" as const,
      vendor: "all",
      category: "all",
      query: "",
    };
    expect(
      filterStockRows([stocked, warning, critical, ok], { ...options, status: "attention" }).map(
        (item) => item.material_id,
      ),
    ).toEqual(["mug", "warn", "crit"]);
    expect(
      filterStockRows([stocked, warning, critical, ok], { ...options, status: "warning" }).map(
        (item) => item.material_id,
      ),
    ).toEqual(["warn"]);
  });

  it("builds vendor options from the current scope", () => {
    expect(uniqueSorted(scopedStockRows([stocked, catalog], "stocked").map((item) => item.vendor_name))).toEqual([
      "Mill",
    ]);
    expect(uniqueSorted(scopedStockRows([stocked, catalog], "catalog").map((item) => item.vendor_name))).toEqual([
      "Mill",
      "Shop",
    ]);
  });
});

describe("inventory qty display and csv", () => {
  it("renders unknown as dash and known zero as zero", () => {
    expect(fmtKnownQty(null, "gal")).toBe("—");
    expect(fmtKnownQty(undefined, "gal")).toBe("—");
    expect(fmtKnownQty(0, "gal")).toBe("0 gal");
    expect(fmtDaysOfCover(null)).toBe("—");
    expect(fmtDaysOfCover(undefined)).toBe("—");
    expect(fmtDaysOfCover(0)).toBe("0d");
    expect(fmtDaysOfCover(0.4)).toBe("0.4d");
  });

  it("writes known zero reserved as 0 and unknown reserved as an empty cell", () => {
    const csv = buildStockCsv([
      row({
        material_id: "zero",
        name: "Zero reserved",
        reserved_qty: 0,
        incoming_qty: 0,
        available_qty: 1,
      }),
      row({
        material_id: "unknown",
        name: "Unknown reserved",
        reserved_qty: null,
        incoming_qty: null,
        available_qty: null,
      }),
    ]);
    const [header, zeroRow, unknownRow] = csv.trimEnd().split("\n");
    const headerFields = header.split(",");
    const zeroFields = zeroRow.split(",");
    const unknownFields = unknownRow.split(",");
    expect(headerFields).toHaveLength(14);
    expect(zeroFields).toHaveLength(headerFields.length);
    expect(unknownFields).toHaveLength(headerFields.length);
    expect(headerFields[4]).toBe("reserved");
    expect(zeroFields[4]).toBe("0");
    expect(unknownFields[4]).toBe("");
    expect(unknownFields[5]).toBe("");
    expect(unknownFields[6]).toBe("");
    expect(headerFields[7]).toBe("days_of_cover");
    expect(unknownFields[7]).toBe("");
    expect(headerFields[8]).toBe("dead_stock");
    expect(zeroFields[8]).toBe("no");
  });
});

describe("inventory refresh conflict notice", () => {
  it("treats cooldown as current, not stale", () => {
    expect(refreshConflictNotice("sync_cooldown", null)).toBe(
      "Recently refreshed. Your numbers are up to date.",
    );
    expect(refreshConflictNotice("sync_cooldown", "in about 2 minutes")).toBe(
      "Recently refreshed. Next refresh available in about 2 minutes. Your numbers are up to date.",
    );
    expect(refreshConflictNotice("sync_in_flight", null)).toBe("A refresh is already running.");
  });
});
