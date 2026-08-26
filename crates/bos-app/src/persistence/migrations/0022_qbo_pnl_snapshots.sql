-- qbo_views: cached ProfitAndLoss period totals (provider cache, not source
-- of truth). One row per (kind, period start): months feed the gross-margin
-- baseline (avg monthly margin of the previous four completed quarters — the
-- pilot's payment metric) and the MTD card; weeks feed the WTD card.
-- is_complete = the period is fully in the past; the sync never re-fetches
-- complete periods, which is what keeps the request budget flat.
CREATE TABLE qbo_pnl_snapshots (
    client_id TEXT NOT NULL,
    period_kind TEXT NOT NULL,           -- 'month' | 'week'
    period_start TEXT NOT NULL,          -- YYYY-MM-DD
    period_end TEXT NOT NULL,
    total_income_cents INTEGER NOT NULL DEFAULT 0,
    total_cogs_cents INTEGER NOT NULL DEFAULT 0,
    gross_profit_cents INTEGER NOT NULL DEFAULT 0,
    is_complete INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT NOT NULL,
    first_seen_at_ms INTEGER NOT NULL,
    last_written_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, period_kind, period_start)
);
