---
title: "Inventory"
keywords: ["inventory", "stock", "orders", "reorder", "cover", "dead stock", "refresh"]
order: 40
---

# Inventory

## What this does

Inventory shows stock levels, order status, low-stock alerts, and reorder suggestions from your connected inventory system. It's a read-only view for spotting what needs attention.

## How to operate it

- Start with Orders needing attention to find blocked, unmapped, or stuck orders.
- Check Low stock and Reorder suggestions for materials running low.
- Open Orders for status, customer, what each one needs, and total.
- Open Stock on hand for the **stocked report** — quantity, SKU, days of cover, status, vendor, and value for items you actually stock.
- Switch to Catalog only when you need built-to-order or non-replenished items.
- Names stay in BusinessOS. Use ↗ to open the matching page in Stockforge.
- Refresh to pull the latest numbers right away. A "recently refreshed" notice means the numbers are already current, not stale.
- Export CSV for the rows you are looking at, or open the full list in Stockforge for their native export.

## Common tasks

- Check fulfillment: scan the order board counts, then work the Orders needing attention list.
- Plan purchasing: review Low stock and Reorder suggestions, and check Inbound purchase orders before you order.
- Check a material: find it in the stocked report and compare On hand against its status. Use ↗ if you need to edit it in Stockforge.

## Stocked vs catalog

The default Stock on hand table is the stocked report: active items Stockforge depletes from stock and replenishes (`STOCK` plus `AUTO`, `PURCHASE`, or `PRODUCTION`). Built-to-order kits and items with missing policies stay in Catalog so they cannot pollute out-of-stock or low-stock counts. Filter **Needs attention** to match the Low stock KPI (out + critical + warning).

Days cover comes from Stockforge's cached stockout prediction. A dash means no burn prediction was supplied; it does not mean zero days. **Dead stock** is labeled only when the item is stocked and on hand, no stockout prediction is present, the cached 30-day order lines show no demand, and open purchase-order lines show no inbound stock. Missing, empty, or incomplete line history is not treated as "no demand" — BusinessOS does not apply the label.
