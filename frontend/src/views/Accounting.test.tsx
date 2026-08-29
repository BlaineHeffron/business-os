import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AccountingFinancialsResponse } from "../types/generated/AccountingFinancialsResponse";
import type { AccountingConnectorStatus } from "../types/generated/AccountingConnectorStatus";
import type { AccountingInvoiceRow } from "../types/generated/AccountingInvoiceRow";
import type { AccountingSyncInfo } from "../types/generated/AccountingSyncInfo";
import type { CustomerTierSyncRun } from "../types/generated/CustomerTierSyncRun";
import Accounting, {
  AccountingMarginTrend,
  AccountingReconnectNotice,
  AccountingRefreshMessages,
  CustomerTierSyncPanel,
  accountingDueLabel,
  accountingInvoiceMatchesBucket,
  accountingSyncButtonLabel,
} from "./Accounting";

const sync: AccountingSyncInfo = {
  sync_enabled: true,
  in_flight: false,
  backfill_complete: true,
  last_synced_at_ms: 1_700_000_000_000,
  invoice_count: 7,
  customer_count: 4,
  last_requests_used: 5,
  next_sync_allowed_at_ms: 1_700_000_060_000,
};

function invoice(
  overrides: Partial<AccountingInvoiceRow> = {},
): AccountingInvoiceRow {
  return {
    invoice_id: "invoice-1",
    doc_number: "INV-1",
    customer_name: "Ada Lovelace",
    txn_date: "2026-08-01",
    due_date: "2026-08-20",
    total_cents: 25_000,
    balance_cents: 10_000,
    status: "open",
    days_overdue: 0,
    ...overrides,
  };
}

function financials(
  overrides: Partial<AccountingFinancialsResponse> = {},
): AccountingFinancialsResponse {
  return {
    basis: "quickbooks_pnl",
    metric_basis: "gross_margin",
    metric_basis_label: "Gross margin",
    week_to_date_cents: 120_000,
    prior_week_to_date_cents: 90_000,
    month_to_date_cents: 460_000,
    prior_month_to_date_cents: 410_000,
    mtd_gross_profit_cents: 180_000,
    mtd_cogs_cents: 280_000,
    baseline_monthly_margin_cents: 150_000,
    baseline_months_cached: 12,
    baseline_window_start: "2025-08-01",
    baseline_window_end: "2026-07-31",
    margin_above_baseline_cents: 30_000,
    metric_value_cents: 180_000,
    metric_baseline_cents: 150_000,
    metric_above_baseline_cents: 30_000,
    metric_pending_reason: null,
    months: [
      {
        month_start: "2026-07-01",
        total_income_cents: 500_000,
        total_cogs_cents: 300_000,
        gross_profit_cents: 200_000,
        is_complete: true,
      },
      {
        month_start: "2026-08-01",
        total_income_cents: 460_000,
        total_cogs_cents: 280_000,
        gross_profit_cents: 180_000,
        is_complete: false,
      },
    ],
    sync,
    ...overrides,
  };
}

function tierRun(
  overrides: Partial<CustomerTierSyncRun> = {},
): CustomerTierSyncRun {
  return {
    run_id: "tier-run-1",
    status: "staged",
    revision: 3,
    plan: {
      source_provider: "qbo",
      target_provider: "shopify",
      mapping_version: "tiers-v2",
      actions: [
        {
          qbo_customer_id: "qbo-1",
          display_name: "Ada Lovelace",
          email: "ada@example.test",
          qbo_tier: "Wholesale",
          shopify: { tag: "tier:Wholesale" },
        },
      ],
      skipped: [
        {
          qbo_customer_id: "qbo-2",
          display_name: "Grace Hopper",
          reason: "missing email",
          qbo_tier: "Retail",
        },
      ],
    },
    outbox_job_id: null,
    outbox_job: null,
    created_at_ms: 1_700_000_000_000,
    updated_at_ms: 1_700_000_000_000,
    ...overrides,
  };
}

describe("Accounting invoice behavior", () => {
  it("matches every aging bucket without including paid invoices", () => {
    const cases: Array<[string, AccountingInvoiceRow]> = [
      ["current", invoice({ days_overdue: 0 })],
      ["days_1_30", invoice({ status: "overdue", days_overdue: 1 })],
      ["days_31_60", invoice({ status: "overdue", days_overdue: 45 })],
      ["days_61_90", invoice({ status: "overdue", days_overdue: 75 })],
      ["days_90_plus", invoice({ status: "overdue", days_overdue: 91 })],
      ["no_due_date", invoice({ due_date: null })],
    ];

    for (const [bucket, row] of cases) {
      expect(accountingInvoiceMatchesBucket(row, bucket)).toBe(true);
    }
    expect(accountingInvoiceMatchesBucket(invoice(), "unknown")).toBe(false);
    expect(
      accountingInvoiceMatchesBucket(invoice({ status: "paid" }), "current"),
    ).toBe(false);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 30 }),
        "days_1_30",
      ),
    ).toBe(true);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 31 }),
        "days_1_30",
      ),
    ).toBe(false);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 60 }),
        "days_31_60",
      ),
    ).toBe(true);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 61 }),
        "days_31_60",
      ),
    ).toBe(false);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 90 }),
        "days_61_90",
      ),
    ).toBe(true);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 90 }),
        "days_90_plus",
      ),
    ).toBe(false);
    expect(
      accountingInvoiceMatchesBucket(
        invoice({ status: "overdue", days_overdue: 91 }),
        "days_90_plus",
      ),
    ).toBe(true);
  });

  it("uses relational due wording for the table", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-20T15:00:00"));

    expect(accountingDueLabel(invoice({ due_date: null }))).toEqual({
      text: "no due date",
      cls: "text-zinc-400",
    });
    expect(
      accountingDueLabel(
        invoice({ status: "overdue", days_overdue: 1 }),
      ),
    ).toEqual({ text: "1 day overdue", cls: "text-red-400" });
    expect(
      accountingDueLabel(
        invoice({ status: "overdue", days_overdue: 12 }),
      ).text,
    ).toBe("12 days overdue");
    expect(accountingDueLabel(invoice({ due_date: "2026-08-20" })).text).toBe(
      "due today",
    );
    expect(accountingDueLabel(invoice({ due_date: "2026-08-21" })).text).toBe(
      "due in 1 day",
    );
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("Accounting rendered states", () => {
  it("uses clear accounting sync action labels", () => {
    expect(accountingSyncButtonLabel(true, false, false)).toBe("Reconnect required");
    expect(accountingSyncButtonLabel(false, true, false)).toBe("Syncing…");
    expect(accountingSyncButtonLabel(false, false, true)).toBe("Updating…");
    expect(accountingSyncButtonLabel(false, false, false)).toBe("Refresh");
  });

  it("replaces the generic refresh error with reconnect guidance", () => {
    const reconnectHtml = renderToStaticMarkup(
      createElement(AccountingRefreshMessages, {
        notice: null,
        lastError: "token refresh failed",
        reconnectRequired: true,
      }),
    );
    expect(reconnectHtml).not.toContain("Try Refresh");

    const retryHtml = renderToStaticMarkup(
      createElement(AccountingRefreshMessages, {
        notice: "A refresh is already running.",
        lastError: "temporary error",
        reconnectRequired: false,
      }),
    );
    expect(retryHtml).toContain("A refresh is already running");
    expect(retryHtml).toContain("Try Refresh");
  });

  it("directs the operator to reconnect after OAuth authorization fails", () => {
    const status: AccountingConnectorStatus = {
      provider: "qbo",
      connected: true,
      reconnect_required: true,
      connection_error_code: "qbo_token_rejected",
      connect_url: "/api/connectors/qbo/connect",
    };
    const html = renderToStaticMarkup(
      createElement(AccountingReconnectNotice, { status }),
    );

    expect(html).toContain("QuickBooks needs to be reconnected");
    expect(html).toContain("Cached numbers remain available");
    expect(html).toContain('href="/api/connectors/qbo/connect"');
    expect(html).toContain("Reconnect QuickBooks");
  });

  it("renders the real loading state without starting browser effects", () => {
    const html = renderToStaticMarkup(
      createElement(Accounting, {
        onUnauthorized: vi.fn(),
        onOpenHelpTopic: vi.fn(),
        tierSyncEnabled: false,
      }),
    );

    expect(html).toContain("animate-pulse");
    expect(html).toContain("<table");
  });

  it("renders margin history and honestly distinguishes partial months", () => {
    const html = renderToStaticMarkup(
      createElement(AccountingMarginTrend, { financials: financials() }),
    );

    expect(html).toContain("Gross margin by month");
    expect(html).toContain("baseline $1,500/mo");
    expect(html).toContain("July 2026 · Sales $5,000.00 · Margin $2,000.00 (40%)");
    expect(html).toContain("August 2026 (so far)");
  });

  it("renders invoice-total trends and the missing-history explanation", () => {
    const salesHtml = renderToStaticMarkup(
      createElement(AccountingMarginTrend, {
        financials: financials({
          basis: "invoice_totals",
          metric_basis: "invoice_totals",
          months: financials().months.map((month) => ({
            ...month,
            total_cogs_cents: null,
            gross_profit_cents: null,
          })),
        }),
      }),
    );
    expect(salesHtml).toContain("Sales by month");
    expect(salesHtml).not.toContain("Margin $");

    const pendingHtml = renderToStaticMarkup(
      createElement(AccountingMarginTrend, {
        financials: financials({
          metric_baseline_cents: null,
          baseline_months_cached: 9,
        }),
      }),
    );
    expect(pendingHtml).toContain("Baseline appears once 3 more months");

    expect(
      renderToStaticMarkup(
        createElement(AccountingMarginTrend, {
          financials: financials({ months: [] }),
        }),
      ),
    ).toBe("");
  });

  it("renders tier-sync review, dry-run, and failure outcomes", () => {
    const handlers = {
      onPreview: vi.fn(),
      onApprove: vi.fn(),
      onReject: vi.fn(),
    };
    const staged = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun(),
        busy: false,
        ...handlers,
      }),
    );
    expect(staged).toContain("1 mapped customer · 1 skipped · tiers-v2");
    expect(staged).toContain("staged");

    const delivered = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun({
          status: "approved",
          outbox_job_id: "job-1",
          outbox_job: {
            job_id: "job-1",
            status: "delivered",
            attempts: 1,
            dry_run: true,
            provider_object_id: null,
          },
        }),
        busy: false,
        ...handlers,
      }),
    );
    expect(delivered).toContain("approved");
    expect(delivered).toContain("dry-run delivered");

    const queued = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun({ status: "approved" }),
        busy: false,
        ...handlers,
      }),
    );
    expect(queued).toContain("queued");

    const live = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun({
          status: "approved",
          outbox_job_id: "job-live",
          outbox_job: {
            job_id: "job-live",
            status: "delivered",
            attempts: 1,
            dry_run: false,
            provider_object_id: "shopify-1",
          },
        }),
        busy: false,
        ...handlers,
      }),
    );
    expect(live).toContain(">delivered<");

    const pending = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun({
          status: "approved",
          outbox_job_id: "job-pending",
          outbox_job: {
            job_id: "job-pending",
            status: "pending",
            attempts: 0,
          },
        }),
        busy: false,
        ...handlers,
      }),
    );
    expect(pending).toContain(">pending<");

    const failed = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun({
          status: "rejected",
          outbox_job_id: "job-2",
          outbox_job: {
            job_id: "job-2",
            status: "failed_terminal",
            attempts: 3,
            last_error: "Shopify rejected the tier",
            dry_run: false,
            provider_object_id: null,
          },
        }),
        busy: false,
        ...handlers,
      }),
    );
    expect(failed).toContain("rejected");
    expect(failed).toContain("failed");
    expect(failed).toContain("Shopify rejected the tier");

    const empty = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: null,
        busy: true,
        ...handlers,
      }),
    );
    expect(empty).toContain("Preview builds a reviewed plan");
    expect(empty).toMatch(/disabled="" class="[^"]+">Approve<\/button>/);
    expect(empty).toMatch(/disabled="" class="[^"]+">Reject<\/button>/);

    const noActions = renderToStaticMarkup(
      createElement(CustomerTierSyncPanel, {
        run: tierRun({
          plan: {
            source_provider: "qbo",
            target_provider: "shopify",
            mapping_version: "tiers-v2",
            actions: [],
            skipped: [],
          },
        }),
        busy: false,
        ...handlers,
      }),
    );
    expect(noActions).toMatch(/disabled="" class="[^"]+">Approve<\/button>/);
    expect(noActions).not.toMatch(/disabled="" class="[^"]+">Reject<\/button>/);
  });

});
