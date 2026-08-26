//! The accounting-provider READ seam: provider-neutral records, the paged
//! incremental-walk request shape, the shared error taxonomy, and the
//! [`AccountingReadClient`] trait every provider implements (QuickBooks in
//! `qbo_read`, Invoice Ninja in `invoice_ninja`). The sync pump in bos-app
//! drives this trait and never sees provider specifics.

/// Walk page cap shared by all providers (QBO's query endpoint allows up to
/// 1000; 100 keeps payloads small and the per-cycle budget meaningful).
pub const ACCOUNTING_MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountingError {
    /// 429 — caller must back off (Retry-After honored when present).
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    /// Expired/invalid short-lived credential; the caller may recover once
    /// (only OAuth providers like QBO ever emit this).
    AuthExpired { message: String },
    /// 5xx / network / timeout — safe to retry next cycle.
    Retryable { code: String, message: String },
    /// Other 4xx, parse failures — retrying won't help.
    Permanent { code: String, message: String },
}

impl std::fmt::Display for AccountingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::AuthExpired { message } => write!(formatter, "auth_expired: {message}"),
            Self::Retryable { code, message } => write!(formatter, "{code}: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

/// One page of an entity walk. `start_position` is a 1-based RECORD offset;
/// the sync pump only ever requests page-aligned positions
/// (`1, 1+page_size, 1+2*page_size, …`), which page-number APIs rely on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Inclusive updated-at filter (RFC3339). `None` = full walk (initial
    /// backfill). Inclusive on purpose: boundary ties re-fetch and the
    /// caller's content-hash upsert keeps them quiet.
    pub since_updated_at: Option<String>,
    pub start_position: u32,
    /// Capped at [`ACCOUNTING_MAX_PAGE_SIZE`].
    pub page_size: u32,
}

impl PageRequest {
    pub fn effective_page_size(&self) -> u32 {
        self.page_size.clamp(1, ACCOUNTING_MAX_PAGE_SIZE)
    }
}

#[derive(Debug)]
pub struct Page<T> {
    pub records: Vec<T>,
    /// The page size actually requested; `records.len() < requested_page_size`
    /// means this was the last page of the walk.
    pub requested_page_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceRecord {
    pub invoice_id: String,
    pub doc_number: Option<String>,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    /// YYYY-MM-DD.
    pub txn_date: Option<String>,
    pub due_date: Option<String>,
    /// Money as integer cents — floats never survive parsing.
    pub total_amt_cents: i64,
    pub balance_cents: i64,
    pub voided: bool,
    /// Provider updated-at (RFC3339-ish, lexically ordered) — cursor input.
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillRecord {
    pub bill_id: String,
    pub vendor_id: Option<String>,
    pub vendor_name: Option<String>,
    /// YYYY-MM-DD.
    pub txn_date: Option<String>,
    pub due_date: Option<String>,
    /// Money as integer cents — floats never survive parsing.
    pub total_amt_cents: i64,
    pub balance_cents: i64,
    pub voided: bool,
    /// Provider updated-at (RFC3339-ish, lexically ordered) — cursor input.
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierSource {
    CustomerTypeRefName,
    CustomField,
    NotProvided,
}

impl TierSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CustomerTypeRefName => "customer_type_ref",
            Self::CustomField => "custom_field",
            Self::NotProvided => "not_provided",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerRecord {
    pub customer_id: String,
    pub display_name: String,
    pub company_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub active: bool,
    pub tier_raw: Option<String>,
    pub tier_source: TierSource,
    pub updated_at: Option<String>,
}

/// Totals extracted from one profit-and-loss style report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PnlSummary {
    pub total_income_cents: i64,
    pub total_cogs_cents: i64,
    pub gross_profit_cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnlDailySummary {
    /// YYYY-MM-DD.
    pub date: String,
    pub total_income_cents: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PnlSummarizeColumnBy {
    Total,
    Days,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PnlReportRequest<'a> {
    pub start_date: &'a str,
    pub end_date: &'a str,
    pub summarize_column_by: PnlSummarizeColumnBy,
}

impl<'a> PnlReportRequest<'a> {
    pub fn total(start_date: &'a str, end_date: &'a str) -> Self {
        Self {
            start_date,
            end_date,
            summarize_column_by: PnlSummarizeColumnBy::Total,
        }
    }

    pub fn days(start_date: &'a str, end_date: &'a str) -> Self {
        Self {
            start_date,
            end_date,
            summarize_column_by: PnlSummarizeColumnBy::Days,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PnlReport {
    pub summary: PnlSummary,
    pub daily_income: Vec<PnlDailySummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BalanceSheetSummary {
    pub cash_on_hand_cents: i64,
}

/// What the sync pump drives. Providers carry their own credentials
/// (constructed per cycle by the caller); auth recovery is a separate seam.
pub trait AccountingReadClient: Send + Sync {
    fn fetch_invoices(&self, page: &PageRequest) -> Result<Page<InvoiceRecord>, AccountingError>;
    fn fetch_customers(&self, page: &PageRequest) -> Result<Page<CustomerRecord>, AccountingError>;
    fn supports_bills(&self) -> bool {
        false
    }
    fn fetch_bills(&self, _page: &PageRequest) -> Result<Page<BillRecord>, AccountingError> {
        Err(AccountingError::Permanent {
            code: "bills_unsupported".to_string(),
            message: "provider does not expose bills".to_string(),
        })
    }
    /// Capability flag: providers without P&L reporting (e.g. Invoice Ninja)
    /// return false and the pump skips the P&L step entirely.
    fn supports_pnl(&self) -> bool;
    /// Only called when [`supports_pnl`](Self::supports_pnl) is true.
    fn fetch_profit_and_loss(
        &self,
        request: &PnlReportRequest<'_>,
    ) -> Result<PnlReport, AccountingError>;
    fn supports_balance_sheet(&self) -> bool {
        false
    }
    fn fetch_balance_sheet(
        &self,
        _as_of_date: &str,
    ) -> Result<BalanceSheetSummary, AccountingError> {
        Err(AccountingError::Permanent {
            code: "balance_sheet_unsupported".to_string(),
            message: "provider does not expose balance sheet reports".to_string(),
        })
    }
}

/// Deterministic in-memory client with the SAME walk semantics the live
/// clients rely on (inclusive since-filter, ordered by updated_at, then
/// position/size paging) — the sync pump's cursor math is tested against it.
#[derive(Default, Clone)]
pub struct FixtureAccountingReadClient {
    pub invoices: Vec<InvoiceRecord>,
    pub bills: Vec<BillRecord>,
    pub customers: Vec<CustomerRecord>,
    /// (start_date, end_date) → totals. Periods not present report zeros,
    /// like a quiet stretch of real books.
    pub pnl: std::collections::HashMap<(String, String), PnlSummary>,
    pub daily_pnl: std::collections::HashMap<(String, String), Vec<PnlDailySummary>>,
    pub balance_sheet: Option<BalanceSheetSummary>,
    pub bills_supported: bool,
    pub balance_sheet_supported: bool,
    /// Mirrors the provider capability (default true so P&L tests work).
    pub pnl_supported: bool,
}

impl FixtureAccountingReadClient {
    pub fn with_pnl_support() -> Self {
        Self {
            pnl_supported: true,
            ..Self::default()
        }
    }

    fn walk<T: Clone>(
        records: &[T],
        page: &PageRequest,
        updated_at: impl Fn(&T) -> String,
    ) -> Page<T> {
        let mut matching: Vec<T> = records
            .iter()
            .filter(|record| match page.since_updated_at.as_deref() {
                Some(since) => updated_at(record).as_str() >= since,
                None => true,
            })
            .cloned()
            .collect();
        matching.sort_by_key(|record| updated_at(record));
        let page_size = page.effective_page_size() as usize;
        let start = (page.start_position.max(1) as usize) - 1;
        let records = matching.into_iter().skip(start).take(page_size).collect();
        Page {
            records,
            requested_page_size: page.effective_page_size(),
        }
    }
}

impl AccountingReadClient for FixtureAccountingReadClient {
    fn fetch_invoices(&self, page: &PageRequest) -> Result<Page<InvoiceRecord>, AccountingError> {
        Ok(Self::walk(&self.invoices, page, |record| {
            record.updated_at.clone()
        }))
    }

    fn fetch_customers(&self, page: &PageRequest) -> Result<Page<CustomerRecord>, AccountingError> {
        Ok(Self::walk(&self.customers, page, |record| {
            record.updated_at.clone().unwrap_or_default()
        }))
    }

    fn supports_bills(&self) -> bool {
        self.bills_supported
    }

    fn fetch_bills(&self, page: &PageRequest) -> Result<Page<BillRecord>, AccountingError> {
        Ok(Self::walk(&self.bills, page, |record| {
            record.updated_at.clone()
        }))
    }

    fn supports_pnl(&self) -> bool {
        self.pnl_supported
    }

    fn fetch_profit_and_loss(
        &self,
        request: &PnlReportRequest<'_>,
    ) -> Result<PnlReport, AccountingError> {
        let key = (request.start_date.to_string(), request.end_date.to_string());
        let summary = self.pnl.get(&key).copied().unwrap_or_default();
        let daily_income = if request.summarize_column_by == PnlSummarizeColumnBy::Days {
            self.daily_pnl.get(&key).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(PnlReport {
            summary,
            daily_income,
        })
    }

    fn supports_balance_sheet(&self) -> bool {
        self.balance_sheet_supported
    }

    fn fetch_balance_sheet(
        &self,
        _as_of_date: &str,
    ) -> Result<BalanceSheetSummary, AccountingError> {
        Ok(self.balance_sheet.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoice(id: &str, updated_at: &str) -> InvoiceRecord {
        InvoiceRecord {
            invoice_id: id.to_string(),
            doc_number: None,
            customer_id: None,
            customer_name: None,
            txn_date: None,
            due_date: None,
            total_amt_cents: 100,
            balance_cents: 100,
            voided: false,
            updated_at: updated_at.to_string(),
        }
    }

    fn page(since: Option<&str>, start: u32, size: u32) -> PageRequest {
        PageRequest {
            since_updated_at: since.map(str::to_string),
            start_position: start,
            page_size: size,
        }
    }

    #[test]
    fn fixture_client_pages_deterministically() {
        let fixture = FixtureAccountingReadClient {
            invoices: vec![
                invoice("c", "2026-06-03T00:00:00-07:00"),
                invoice("a", "2026-06-01T00:00:00-07:00"),
                invoice("b", "2026-06-02T00:00:00-07:00"),
            ],
            ..Default::default()
        };
        let first = fixture.fetch_invoices(&page(None, 1, 2)).expect("page");
        assert_eq!(
            first
                .records
                .iter()
                .map(|record| record.invoice_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"],
            "ordered by updated_at"
        );
        let second = fixture.fetch_invoices(&page(None, 3, 2)).expect("page");
        assert_eq!(second.records.len(), 1, "last short page");

        // Inclusive since-filter: the boundary row is re-fetched.
        let incremental = fixture
            .fetch_invoices(&page(Some("2026-06-02T00:00:00-07:00"), 1, 10))
            .expect("page");
        assert_eq!(incremental.records.len(), 2);
    }
}
