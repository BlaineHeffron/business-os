//! QuickBooks Online implementation of the accounting READ seam
//! ([`crate::accounting_read::AccountingReadClient`]): the SQL-ish query
//! endpoint with LastUpdatedTime-incremental, STARTPOSITION-paginated walks,
//! plus ProfitAndLoss report totals. GET-only by construction — there is no
//! write path in this module, which is how the read-only posture is enforced
//! (QBO's accounting scope can't be narrowed).
//!
//! Rate-limit care is a first-class concern: 429 and 401 get their own error
//! variants because the sync pump treats them differently (backoff-with-
//! deadline vs refresh-and-retry-once). The caller owns the request budget;
//! this module never loops or retries on its own.

use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::accounting_read::{
    AccountingError, AccountingReadClient, BalanceSheetSummary, BillRecord, CustomerRecord,
    InvoiceRecord, Page, PageRequest, PnlDailySummary, PnlReport, PnlReportRequest,
    PnlSummarizeColumnBy, PnlSummary, TierSource,
};

use crate::qbo_common::QBO_MINOR_VERSION;

/// Transport seam: GET-only on purpose.
pub trait QboHttp: Send + Sync {
    fn get_json(&self, url: &str, access_token: &str) -> Result<QboHttpResponse, AccountingError>;
}

pub struct QboHttpResponse {
    pub status: u16,
    pub body: Value,
    /// Intuit's request trace id — goes into error messages, never logs of
    /// payloads.
    pub intuit_tid: Option<String>,
    /// Parsed Retry-After seconds on 429 responses.
    pub retry_after_secs: Option<u64>,
}

pub struct ReqwestQboHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestQboHttpClient {
    fn default() -> Self {
        // Bound connect + total time so a hung QBO endpoint cannot pin the
        // calling blocking worker thread indefinitely.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl QboHttp for ReqwestQboHttpClient {
    fn get_json(&self, url: &str, access_token: &str) -> Result<QboHttpResponse, AccountingError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(access_token)
            .header("Accept", "application/json")
            .send()
            .map_err(|err| AccountingError::Retryable {
                code: "qbo_request_failed".to_string(),
                message: err.to_string(),
            })?;
        let status = response.status().as_u16();
        let intuit_tid = response
            .headers()
            .get("intuit_tid")
            .and_then(|value| value.to_str().ok())
            .map(sanitize_trace_id);
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().unwrap_or(Value::Null);
        Ok(QboHttpResponse {
            status,
            body,
            intuit_tid,
            retry_after_secs,
        })
    }
}

fn sanitize_trace_id(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
        .take(64)
        .collect()
}

/// Live QBO client, bound to one realm. The access token lives in a cell so
/// the sync pump's mid-cycle 401 recovery can install a fresh token without
/// rebuilding the client (the refresh itself is the AuthRecovery seam's job).
pub struct LiveQboReadClient<C: QboHttp = ReqwestQboHttpClient> {
    http: Arc<C>,
    api_base_url: String,
    realm_id: String,
    access_token: Mutex<String>,
}

impl<C: QboHttp> LiveQboReadClient<C> {
    pub fn new(
        http: Arc<C>,
        api_base_url: impl Into<String>,
        realm_id: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            http,
            api_base_url: api_base_url.into(),
            realm_id: realm_id.into(),
            access_token: Mutex::new(access_token.into()),
        }
    }

    pub fn set_access_token(&self, token: &str) {
        *self.access_token.lock().expect("qbo token cell") = token.to_string();
    }

    fn run_query(&self, query: String) -> Result<Value, AccountingError> {
        let url = query_url(&self.api_base_url, &self.realm_id, &query);
        self.run_get(&url)
    }

    fn run_get(&self, url: &str) -> Result<Value, AccountingError> {
        let token = self.access_token.lock().expect("qbo token cell").clone();
        let response = self.http.get_json(url, &token)?;
        let tid = response.intuit_tid.clone().unwrap_or_default();
        match response.status {
            200..=299 => Ok(response.body),
            429 => Err(AccountingError::RateLimited {
                retry_after_ms: response.retry_after_secs.map(|secs| secs * 1000),
                message: format!("qbo 429 (intuit_tid {tid})"),
            }),
            401 => Err(AccountingError::AuthExpired {
                message: format!("qbo 401 (intuit_tid {tid})"),
            }),
            500..=599 => Err(AccountingError::Retryable {
                code: "qbo_server_error".to_string(),
                message: format!("qbo {} (intuit_tid {tid})", response.status),
            }),
            other => Err(AccountingError::Permanent {
                code: "qbo_query_rejected".to_string(),
                message: format!("qbo {other} (intuit_tid {tid})"),
            }),
        }
    }
}

impl<C: QboHttp> AccountingReadClient for LiveQboReadClient<C> {
    fn fetch_invoices(&self, page: &PageRequest) -> Result<Page<InvoiceRecord>, AccountingError> {
        let body = self.run_query(entity_query("Invoice", page))?;
        Ok(Page {
            records: entity_array(&body, "Invoice")
                .into_iter()
                .filter_map(invoice_record_from_value)
                .collect(),
            requested_page_size: page.effective_page_size(),
        })
    }

    fn fetch_customers(&self, page: &PageRequest) -> Result<Page<CustomerRecord>, AccountingError> {
        let body = self.run_query(customer_query(page))?;
        Ok(Page {
            records: entity_array(&body, "Customer")
                .into_iter()
                .filter_map(customer_record_from_value)
                .collect(),
            requested_page_size: page.effective_page_size(),
        })
    }

    fn supports_bills(&self) -> bool {
        true
    }

    fn fetch_bills(&self, page: &PageRequest) -> Result<Page<BillRecord>, AccountingError> {
        let body = self.run_query(entity_query("Bill", page))?;
        Ok(Page {
            records: entity_array(&body, "Bill")
                .into_iter()
                .filter_map(bill_record_from_value)
                .collect(),
            requested_page_size: page.effective_page_size(),
        })
    }

    fn supports_pnl(&self) -> bool {
        true
    }

    fn fetch_profit_and_loss(
        &self,
        request: &PnlReportRequest<'_>,
    ) -> Result<PnlReport, AccountingError> {
        let url = report_url(
            &self.api_base_url,
            &self.realm_id,
            "ProfitAndLoss",
            Some(request.start_date),
            Some(request.end_date),
            Some(request.summarize_column_by),
        );
        let body = self.run_get(&url)?;
        Ok(pnl_report_from_report(&body, request.summarize_column_by))
    }

    fn supports_balance_sheet(&self) -> bool {
        true
    }

    fn fetch_balance_sheet(
        &self,
        as_of_date: &str,
    ) -> Result<BalanceSheetSummary, AccountingError> {
        let url = report_url(
            &self.api_base_url,
            &self.realm_id,
            "BalanceSheet",
            None,
            Some(as_of_date),
            None,
        );
        let body = self.run_get(&url)?;
        Ok(balance_sheet_summary_from_report(&body))
    }
}

fn report_url(
    api_base_url: &str,
    realm_id: &str,
    report_name: &str,
    start_date: Option<&str>,
    end_date: Option<&str>,
    summarize_column_by: Option<PnlSummarizeColumnBy>,
) -> String {
    use crate::qbo_oauth::encode_query_component;
    let mut params = BTreeMap::new();
    params.insert("minorversion", QBO_MINOR_VERSION.to_string());
    if let Some(start_date) = start_date {
        params.insert("start_date", start_date.to_string());
    }
    if let Some(end_date) = end_date {
        params.insert("end_date", end_date.to_string());
    }
    if summarize_column_by == Some(PnlSummarizeColumnBy::Days) {
        params.insert("summarize_column_by", "Days".to_string());
    }
    let encoded = params
        .iter()
        .map(|(key, value)| format!("{key}={}", encode_query_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!(
        "{}/v3/company/{}/reports/{report_name}?{encoded}",
        api_base_url.trim_end_matches('/'),
        encode_query_component(realm_id),
    )
}

/// Pull the three headline totals out of a ProfitAndLoss report. The report
/// is a row tree; sections carry `group` markers (Income / COGS /
/// GrossProfit) with the total in the second column of their Summary (or
/// ColData for the computed GrossProfit row). Tolerant by design: a missing
/// section reads 0, and a missing GrossProfit row falls back to
/// income - cogs — books with no COGS accounts still report sensibly.
fn pnl_report_from_report(body: &Value, summarize_column_by: PnlSummarizeColumnBy) -> PnlReport {
    let mut summary = PnlSummary::default();
    let mut saw_gross_profit = false;
    let rows = body
        .get("Rows")
        .and_then(|rows| rows.get("Row"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    for row in rows {
        let group = row.get("group").and_then(Value::as_str).unwrap_or("");
        let amount = row_total_amount_cents(row);
        match (group, amount) {
            ("Income", Some(cents)) => summary.total_income_cents = cents,
            ("COGS", Some(cents)) => summary.total_cogs_cents = cents,
            ("GrossProfit", Some(cents)) => {
                summary.gross_profit_cents = cents;
                saw_gross_profit = true;
            }
            _ => {}
        }
    }
    if !saw_gross_profit {
        summary.gross_profit_cents = summary.total_income_cents - summary.total_cogs_cents;
    }
    let daily_income = if summarize_column_by == PnlSummarizeColumnBy::Days {
        pnl_daily_income_from_report(body)
    } else {
        Vec::new()
    };
    PnlReport {
        summary,
        daily_income,
    }
}

fn row_total_amount_cents(row: &Value) -> Option<i64> {
    row.get("Summary")
        .and_then(|summary_row| summary_row.get("ColData"))
        .or_else(|| row.get("ColData"))
        .and_then(Value::as_array)
        .and_then(|cols| {
            cols.iter()
                .skip(1)
                .filter_map(|col| {
                    col.get("value")
                        .and_then(Value::as_str)
                        .and_then(parse_amount_cents)
                })
                .next_back()
        })
}

#[cfg(test)]
fn pnl_summary_from_report(body: &Value) -> PnlSummary {
    pnl_report_from_report(body, PnlSummarizeColumnBy::Total).summary
}

fn pnl_daily_income_from_report(body: &Value) -> Vec<PnlDailySummary> {
    let columns = report_date_columns(body);
    if columns.is_empty() {
        return Vec::new();
    }
    let rows = body
        .get("Rows")
        .and_then(|rows| rows.get("Row"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let Some(income_row) = rows
        .into_iter()
        .find(|row| row.get("group").and_then(Value::as_str) == Some("Income"))
    else {
        return Vec::new();
    };
    let col_data = income_row
        .get("Summary")
        .and_then(|summary_row| summary_row.get("ColData"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    columns
        .into_iter()
        .map(|(index, date)| PnlDailySummary {
            date,
            total_income_cents: col_data
                .get(index)
                .and_then(|col| col.get("value"))
                .and_then(Value::as_str)
                .and_then(parse_amount_cents)
                .unwrap_or(0),
        })
        .collect()
}

fn report_date_columns(body: &Value) -> Vec<(usize, String)> {
    body.get("Columns")
        .and_then(|columns| columns.get("Column"))
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .enumerate()
                .filter_map(|(index, column)| report_column_date(column).map(|date| (index, date)))
                .collect()
        })
        .unwrap_or_default()
}

fn report_column_date(column: &Value) -> Option<String> {
    if let Some(title) =
        string_field(column, "ColTitle").and_then(|title| normalize_report_date(&title))
    {
        return Some(title);
    }
    column
        .get("MetaData")
        .and_then(Value::as_array)
        .and_then(|metadata| {
            metadata.iter().find_map(|entry| {
                let name = string_field(entry, "Name").unwrap_or_default();
                let value = string_field(entry, "Value")?;
                if name.eq_ignore_ascii_case("StartDate")
                    || name.eq_ignore_ascii_case("Date")
                    || value.contains("tx_date=")
                {
                    normalize_report_date(&value)
                } else {
                    None
                }
            })
        })
}

fn normalize_report_date(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if is_yyyy_mm_dd(trimmed) {
        return Some(trimmed[..10].to_string());
    }
    if let Some(pos) = trimmed.find("tx_date=") {
        let candidate = trimmed.get(pos + "tx_date=".len()..)?;
        if candidate.len() >= 10 && is_yyyy_mm_dd(candidate) {
            return Some(candidate[..10].to_string());
        }
    }
    parse_month_day_year(trimmed)
}

fn is_yyyy_mm_dd(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn parse_month_day_year(value: &str) -> Option<String> {
    let cleaned = value.replace(',', "");
    let parts = cleaned.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let month = match parts[0].to_ascii_lowercase().as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    };
    let day: u32 = parts[1].parse().ok()?;
    let year: i32 = parts[2].parse().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn balance_sheet_summary_from_report(body: &Value) -> BalanceSheetSummary {
    BalanceSheetSummary {
        cash_on_hand_cents: balance_sheet_cash_from_rows(
            body.get("Rows")
                .and_then(|rows| rows.get("Row"))
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            false,
        ),
    }
}

fn balance_sheet_cash_from_rows(rows: &[Value], in_bank_group: bool) -> i64 {
    let mut total = 0;
    for row in rows {
        let group = row.get("group").and_then(Value::as_str).unwrap_or("");
        let header = row
            .get("Header")
            .and_then(|header| header.get("ColData"))
            .and_then(Value::as_array)
            .and_then(|cols| cols.first())
            .and_then(|col| col.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let bank_group = in_bank_group
            || group.eq_ignore_ascii_case("BankAccounts")
            || header.eq_ignore_ascii_case("Bank Accounts")
            || header.eq_ignore_ascii_case("Cash and cash equivalents");
        if bank_group {
            if let Some(summary) = row
                .get("Summary")
                .and_then(|summary_row| summary_row.get("ColData"))
                .and_then(Value::as_array)
                .and_then(|cols| cols.get(1))
                .and_then(|col| col.get("value"))
                .and_then(Value::as_str)
                .and_then(parse_amount_cents)
            {
                total += summary;
                continue;
            }
        }
        if let Some(children) = row
            .get("Rows")
            .and_then(|rows| rows.get("Row"))
            .and_then(Value::as_array)
        {
            total += balance_sheet_cash_from_rows(children, bank_group);
        } else if bank_group {
            total += row_amount_cents(row).unwrap_or(0);
        }
    }
    total
}

fn row_amount_cents(row: &Value) -> Option<i64> {
    row.get("ColData")
        .and_then(Value::as_array)
        .and_then(|cols| cols.get(1))
        .and_then(|col| col.get("value"))
        .and_then(Value::as_str)
        .and_then(parse_amount_cents)
}

/// "1234.56" / "-12.5" → integer cents (round half away from zero).
fn parse_amount_cents(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .map(|amount| (amount * 100.0).round() as i64)
}

/// `SELECT * FROM <entity> [WHERE MetaData.LastUpdatedTime >= '<since>']
/// ORDERBY MetaData.LastUpdatedTime STARTPOSITION n MAXRESULTS m`
fn entity_query(entity: &str, page: &PageRequest) -> String {
    let mut query = format!("SELECT * FROM {entity}");
    if let Some(since) = page
        .since_updated_at
        .as_deref()
        .map(str::trim)
        .filter(|since| !since.is_empty())
    {
        // The timestamp goes inside single quotes; strip any quote characters
        // so a stored cursor can never break out of the literal.
        let safe: String = since.chars().filter(|c| *c != '\'' && *c != '\\').collect();
        query.push_str(&format!(" WHERE MetaData.LastUpdatedTime >= '{safe}'"));
    }
    query.push_str(&format!(
        " ORDERBY MetaData.LastUpdatedTime STARTPOSITION {} MAXRESULTS {}",
        page.start_position.max(1),
        page.effective_page_size(),
    ));
    query
}

fn customer_query(page: &PageRequest) -> String {
    let mut query = "SELECT * FROM Customer WHERE Active IN (true,false)".to_string();
    if let Some(since) = page
        .since_updated_at
        .as_deref()
        .map(str::trim)
        .filter(|since| !since.is_empty())
    {
        let safe: String = since.chars().filter(|c| *c != '\'' && *c != '\\').collect();
        query.push_str(&format!(" AND MetaData.LastUpdatedTime >= '{safe}'"));
    }
    query.push_str(&format!(
        " ORDERBY MetaData.LastUpdatedTime STARTPOSITION {} MAXRESULTS {}",
        page.start_position.max(1),
        page.effective_page_size(),
    ));
    query
}

pub(crate) fn query_url(api_base_url: &str, realm_id: &str, query: &str) -> String {
    use crate::qbo_oauth::encode_query_component;
    let mut params = BTreeMap::new();
    params.insert("minorversion", QBO_MINOR_VERSION.to_string());
    params.insert("query", query.to_string());
    let encoded = params
        .iter()
        .map(|(key, value)| format!("{key}={}", encode_query_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!(
        "{}/v3/company/{}/query?{encoded}",
        api_base_url.trim_end_matches('/'),
        encode_query_component(realm_id),
    )
}

pub(crate) fn entity_array<'a>(body: &'a Value, entity: &str) -> Vec<&'a Value> {
    body.get("QueryResponse")
        .and_then(|response| response.get(entity))
        .and_then(Value::as_array)
        .map(|records| records.iter().collect())
        .unwrap_or_default()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

/// QBO sends money as JSON numbers; round-half-away to integer cents at the
/// parse boundary so floats never reach storage or arithmetic.
fn cents_field(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .map(|amount| (amount * 100.0).round() as i64)
        .unwrap_or(0)
}

pub(crate) fn invoice_record_from_value(value: &Value) -> Option<InvoiceRecord> {
    let invoice_id = string_field(value, "Id")?;
    let customer_ref = value.get("CustomerRef");
    let total_amt_cents = cents_field(value, "TotalAmt");
    let balance_cents = cents_field(value, "Balance");
    // QBO voids an invoice by zeroing it and stamping the private note;
    // there is no first-class voided flag on the query payload.
    let voided = total_amt_cents == 0
        && balance_cents == 0
        && string_field(value, "PrivateNote")
            .is_some_and(|note| note.to_ascii_lowercase().contains("voided"));
    Some(InvoiceRecord {
        invoice_id,
        doc_number: string_field(value, "DocNumber"),
        customer_id: customer_ref.and_then(|cref| string_field(cref, "value")),
        customer_name: customer_ref.and_then(|cref| string_field(cref, "name")),
        txn_date: string_field(value, "TxnDate"),
        due_date: string_field(value, "DueDate"),
        total_amt_cents,
        balance_cents,
        voided,
        updated_at: value
            .get("MetaData")
            .and_then(|meta| string_field(meta, "LastUpdatedTime"))
            .unwrap_or_default(),
    })
}

pub(crate) fn bill_record_from_value(value: &Value) -> Option<BillRecord> {
    let bill_id = string_field(value, "Id")?;
    let vendor_ref = value.get("VendorRef");
    let total_amt_cents = cents_field(value, "TotalAmt");
    let balance_cents = cents_field(value, "Balance");
    let voided = total_amt_cents == 0
        && balance_cents == 0
        && string_field(value, "PrivateNote")
            .is_some_and(|note| note.to_ascii_lowercase().contains("voided"));
    Some(BillRecord {
        bill_id,
        vendor_id: vendor_ref.and_then(|vref| string_field(vref, "value")),
        vendor_name: vendor_ref.and_then(|vref| string_field(vref, "name")),
        txn_date: string_field(value, "TxnDate"),
        due_date: string_field(value, "DueDate"),
        total_amt_cents,
        balance_cents,
        voided,
        updated_at: value
            .get("MetaData")
            .and_then(|meta| string_field(meta, "LastUpdatedTime"))
            .unwrap_or_default(),
    })
}

pub(crate) fn customer_record_from_value(value: &Value) -> Option<CustomerRecord> {
    let customer_id = string_field(value, "Id")?;
    let display_name = string_field(value, "DisplayName").unwrap_or_else(|| customer_id.clone());
    let type_ref_name = value
        .get("CustomerTypeRef")
        .and_then(|tref| string_field(tref, "name"));
    let custom_field_tier = tier_custom_field(value);
    let (tier_raw, tier_source) = match (&type_ref_name, &custom_field_tier) {
        (Some(name), _) => (Some(name.clone()), TierSource::CustomerTypeRefName),
        (None, Some(field)) => (Some(field.clone()), TierSource::CustomField),
        (None, None) => (None, TierSource::NotProvided),
    };
    Some(CustomerRecord {
        customer_id,
        display_name,
        company_name: string_field(value, "CompanyName"),
        email: value
            .get("PrimaryEmailAddr")
            .and_then(|addr| string_field(addr, "Address")),
        phone: value
            .get("PrimaryPhone")
            .and_then(|phone| string_field(phone, "FreeFormNumber")),
        active: value.get("Active").and_then(Value::as_bool).unwrap_or(true),
        tier_raw,
        tier_source,
        updated_at: value
            .get("MetaData")
            .and_then(|meta| string_field(meta, "LastUpdatedTime")),
    })
}

fn tier_custom_field(value: &Value) -> Option<String> {
    value
        .get("CustomField")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|field| {
            let name = string_field(field, "Name")?;
            if !name.eq_ignore_ascii_case("tier") && !name.eq_ignore_ascii_case("customer_tier") {
                return None;
            }
            string_field(field, "StringValue")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn page(since: Option<&str>, start: u32, size: u32) -> PageRequest {
        PageRequest {
            since_updated_at: since.map(str::to_string),
            start_position: start,
            page_size: size,
        }
    }

    #[test]
    fn query_builder_renders_filter_order_and_paging() {
        assert_eq!(
            entity_query("Invoice", &page(None, 1, 100)),
            "SELECT * FROM Invoice ORDERBY MetaData.LastUpdatedTime \
             STARTPOSITION 1 MAXRESULTS 100"
        );
        assert_eq!(
            entity_query(
                "Invoice",
                &page(Some("2026-06-01T00:00:00-07:00"), 101, 100)
            ),
            "SELECT * FROM Invoice WHERE MetaData.LastUpdatedTime >= \
             '2026-06-01T00:00:00-07:00' ORDERBY MetaData.LastUpdatedTime \
             STARTPOSITION 101 MAXRESULTS 100"
        );
        // Quote characters can never escape the literal.
        let hostile = entity_query("Invoice", &page(Some("x' OR '1'='1"), 1, 100));
        assert!(!hostile.contains("' OR "));
        // Page size is clamped to the cap.
        assert!(entity_query("Invoice", &page(None, 1, 5000)).contains("MAXRESULTS 100"));

        assert_eq!(
            customer_query(&page(None, 1, 100)),
            "SELECT * FROM Customer WHERE Active IN (true,false) \
             ORDERBY MetaData.LastUpdatedTime STARTPOSITION 1 MAXRESULTS 100"
        );
        assert_eq!(
            customer_query(&page(Some("2026-06-01T00:00:00-07:00"), 101, 100)),
            "SELECT * FROM Customer WHERE Active IN (true,false) AND \
             MetaData.LastUpdatedTime >= '2026-06-01T00:00:00-07:00' \
             ORDERBY MetaData.LastUpdatedTime STARTPOSITION 101 MAXRESULTS 100"
        );
    }

    #[test]
    fn query_url_encodes_realm_and_query() {
        let url = query_url(
            "https://sandbox-quickbooks.api.intuit.com/",
            "123 456",
            "SELECT * FROM Invoice",
        );
        assert!(url
            .starts_with("https://sandbox-quickbooks.api.intuit.com/v3/company/123%20456/query?"));
        assert!(url.contains("minorversion=75"));
        assert!(url.contains("query=SELECT%20%2A%20FROM%20Invoice"));
    }

    #[test]
    fn invoice_parsing_rounds_cents_and_flags_voids() {
        let invoice = serde_json::json!({
            "Id": "201",
            "DocNumber": "1042",
            "CustomerRef": { "value": "55", "name": "Dana's Repair Co" },
            "TxnDate": "2026-06-02",
            "DueDate": "2026-07-02",
            "TotalAmt": 123.455,
            "Balance": 23.45,
            "MetaData": { "LastUpdatedTime": "2026-06-02T10:00:00-07:00" }
        });
        let record = invoice_record_from_value(&invoice).expect("record");
        assert_eq!(record.total_amt_cents, 12346); // round half away from zero
        assert_eq!(record.balance_cents, 2345);
        assert_eq!(record.customer_name.as_deref(), Some("Dana's Repair Co"));
        assert!(!record.voided);

        let voided = serde_json::json!({
            "Id": "202",
            "TotalAmt": 0,
            "Balance": 0,
            "PrivateNote": "Voided - duplicate",
            "MetaData": { "LastUpdatedTime": "2026-06-03T10:00:00-07:00" }
        });
        assert!(invoice_record_from_value(&voided).expect("record").voided);

        let zero_but_not_voided = serde_json::json!({
            "Id": "203", "TotalAmt": 0, "Balance": 0,
            "MetaData": { "LastUpdatedTime": "2026-06-03T10:00:00-07:00" }
        });
        assert!(
            !invoice_record_from_value(&zero_but_not_voided)
                .expect("record")
                .voided
        );
    }

    #[test]
    fn customer_parsing_resolves_tier_sources() {
        let via_type_ref = serde_json::json!({
            "Id": "1", "DisplayName": "Acme",
            "CustomerTypeRef": { "value": "9", "name": "Tier A" },
            "CustomField": [{ "Name": "tier", "StringValue": "ignored" }],
        });
        let record = customer_record_from_value(&via_type_ref).expect("record");
        assert_eq!(record.tier_raw.as_deref(), Some("Tier A"));
        assert_eq!(record.tier_source, TierSource::CustomerTypeRefName);

        let via_custom_field = serde_json::json!({
            "Id": "2", "DisplayName": "Beta",
            "CustomField": [{ "Name": "Customer_Tier", "StringValue": "Tier B" }],
        });
        let record = customer_record_from_value(&via_custom_field).expect("record");
        assert_eq!(record.tier_raw.as_deref(), Some("Tier B"));
        assert_eq!(record.tier_source, TierSource::CustomField);

        let none = serde_json::json!({ "Id": "3", "DisplayName": "Gamma" });
        let record = customer_record_from_value(&none).expect("record");
        assert!(record.tier_raw.is_none());
        assert_eq!(record.tier_source, TierSource::NotProvided);
        assert!(record.active, "Active defaults true when absent");
    }

    #[test]
    fn pnl_report_parsing_extracts_totals_and_tolerates_gaps() {
        let report = serde_json::json!({
            "Rows": { "Row": [
                { "group": "Income",
                  "Summary": { "ColData": [ {"value": "Total Income"}, {"value": "8543.21"} ] } },
                { "group": "COGS",
                  "Summary": { "ColData": [ {"value": "Total Cost of Goods Sold"}, {"value": "3211.10"} ] } },
                { "group": "GrossProfit",
                  "ColData": [ {"value": "Gross Profit"}, {"value": "5332.11"} ] },
                { "group": "Expenses",
                  "Summary": { "ColData": [ {"value": "Total Expenses"}, {"value": "999.99"} ] } }
            ] }
        });
        let summary = pnl_summary_from_report(&report);
        assert_eq!(summary.total_income_cents, 854_321);
        assert_eq!(summary.total_cogs_cents, 321_110);
        assert_eq!(summary.gross_profit_cents, 533_211);

        // No COGS section and no GrossProfit row: gross profit = income.
        let no_cogs = serde_json::json!({
            "Rows": { "Row": [
                { "group": "Income",
                  "Summary": { "ColData": [ {"value": "Total Income"}, {"value": "100.00"} ] } }
            ] }
        });
        let summary = pnl_summary_from_report(&no_cogs);
        assert_eq!(summary.gross_profit_cents, 10_000);

        // Empty/odd report: all zeros, no panic.
        assert_eq!(
            pnl_summary_from_report(&serde_json::json!({})),
            PnlSummary::default()
        );
    }

    struct FakeQboHttp {
        responses: Mutex<VecDeque<QboHttpResponse>>,
    }

    impl QboHttp for FakeQboHttp {
        fn get_json(&self, _url: &str, _token: &str) -> Result<QboHttpResponse, AccountingError> {
            Ok(self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .expect("scripted response"))
        }
    }

    fn live_client(responses: Vec<QboHttpResponse>) -> LiveQboReadClient<FakeQboHttp> {
        LiveQboReadClient::new(
            Arc::new(FakeQboHttp {
                responses: Mutex::new(responses.into()),
            }),
            "https://example.test",
            "realm-1",
            "at",
        )
    }

    #[test]
    fn bill_parsing_rounds_cents_and_vendor_fields() {
        let bill = serde_json::json!({
            "Id": "801",
            "VendorRef": { "value": "44", "name": "Champion Supply" },
            "TxnDate": "2026-06-01",
            "DueDate": "2026-06-20",
            "TotalAmt": 250.125,
            "Balance": 125.12,
            "MetaData": { "LastUpdatedTime": "2026-06-02T10:00:00-07:00" }
        });
        let record = bill_record_from_value(&bill).expect("record");
        assert_eq!(record.bill_id, "801");
        assert_eq!(record.vendor_name.as_deref(), Some("Champion Supply"));
        assert_eq!(record.total_amt_cents, 25_013);
        assert_eq!(record.balance_cents, 12_512);
        assert!(!record.voided);

        let voided = serde_json::json!({
            "Id": "802",
            "TotalAmt": 0,
            "Balance": 0,
            "PrivateNote": "Voided",
            "MetaData": { "LastUpdatedTime": "2026-06-03T10:00:00-07:00" }
        });
        assert!(bill_record_from_value(&voided).expect("record").voided);
    }

    #[test]
    fn status_codes_map_to_the_error_taxonomy() {
        let client = live_client(vec![
            QboHttpResponse {
                status: 429,
                body: Value::Null,
                intuit_tid: Some("tid-1".to_string()),
                retry_after_secs: Some(30),
            },
            QboHttpResponse {
                status: 401,
                body: Value::Null,
                intuit_tid: None,
                retry_after_secs: None,
            },
            QboHttpResponse {
                status: 502,
                body: Value::Null,
                intuit_tid: None,
                retry_after_secs: None,
            },
            QboHttpResponse {
                status: 400,
                body: Value::Null,
                intuit_tid: None,
                retry_after_secs: None,
            },
        ]);
        let page = page(None, 1, 100);
        assert_eq!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::RateLimited {
                retry_after_ms: Some(30_000),
                message: "qbo 429 (intuit_tid tid-1)".to_string(),
            }
        );
        assert!(matches!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::AuthExpired { .. }
        ));
        assert!(matches!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::Retryable { .. }
        ));
        assert!(matches!(
            client.fetch_invoices(&page).unwrap_err(),
            AccountingError::Permanent { .. }
        ));
    }

    #[test]
    fn pnl_report_parsing_extracts_daily_income_columns() {
        let report = serde_json::json!({
            "Columns": { "Column": [
                { "ColTitle": "" },
                { "ColTitle": "2026-06-18" },
                { "ColTitle": "Jun 19, 2026" },
                { "ColTitle": "Total" }
            ] },
            "Rows": { "Row": [
                { "group": "Income",
                  "Summary": { "ColData": [
                      {"value": "Total Income"},
                      {"value": "100.00"},
                      {"value": "250.25"},
                      {"value": "350.25"}
                  ] } },
                { "group": "GrossProfit",
                  "ColData": [ {"value": "Gross Profit"}, {"value": "350.25"} ] }
            ] }
        });
        let report = pnl_report_from_report(&report, PnlSummarizeColumnBy::Days);
        assert_eq!(
            report.daily_income,
            vec![
                PnlDailySummary {
                    date: "2026-06-18".to_string(),
                    total_income_cents: 10_000,
                },
                PnlDailySummary {
                    date: "2026-06-19".to_string(),
                    total_income_cents: 25_025,
                },
            ]
        );
        assert_eq!(report.summary.total_income_cents, 35_025);
    }

    #[test]
    fn balance_sheet_parsing_sums_bank_account_rows_only() {
        let report = serde_json::json!({
            "Rows": { "Row": [
                { "group": "CurrentAssets", "Header": { "ColData": [ {"value": "Current Assets"}, {"value": ""} ] },
                  "Rows": { "Row": [
                    { "group": "BankAccounts", "Header": { "ColData": [ {"value": "Bank Accounts"}, {"value": ""} ] },
                      "Rows": { "Row": [
                        { "ColData": [ {"value": "Checking"}, {"value": "1200.50"} ] },
                        { "ColData": [ {"value": "Savings"}, {"value": "99.25"} ] }
                      ] },
                      "Summary": { "ColData": [ {"value": "Total Bank Accounts"}, {"value": "1299.75"} ] }
                    },
                    { "group": "AccountsReceivable", "Header": { "ColData": [ {"value": "Accounts Receivable"}, {"value": ""} ] },
                      "Rows": { "Row": [
                        { "ColData": [ {"value": "A/R"}, {"value": "5000.00"} ] }
                      ] }
                    }
                  ] }
                }
            ] }
        });
        let summary = balance_sheet_summary_from_report(&report);
        assert_eq!(summary.cash_on_hand_cents, 129_975);
    }
}
