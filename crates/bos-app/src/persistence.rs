//! Sqlite persistence + the single migration registry.
//!
//! Adding a migration: append `M::up(include_str!(...))` with the next sequential
//! number. `migration_registry_is_contiguous` asserts numbering; slice migrations
//! are declared in the slice's `SliceSpec` and surfaced in REPO_MAP.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub const DB_FILE_NAME: &str = "bos.sqlite";

/// Ordered migration sources. Numbering is load-bearing: 0001, 0002, ...
const MIGRATION_SOURCES: &[(&str, &str)] = &[
    (
        "0001_receipt_spine",
        include_str!("persistence/migrations/0001_receipt_spine.sql"),
    ),
    (
        "0002_email_triage_rules",
        include_str!("persistence/migrations/0002_email_triage_rules.sql"),
    ),
    (
        "0003_email_inbound_messages",
        include_str!("persistence/migrations/0003_email_inbound_messages.sql"),
    ),
    (
        "0004_google_oauth_credentials",
        include_str!("persistence/migrations/0004_google_oauth_credentials.sql"),
    ),
    (
        "0005_email_triage_categories",
        include_str!("persistence/migrations/0005_email_triage_categories.sql"),
    ),
    (
        "0006_work_queue",
        include_str!("persistence/migrations/0006_work_queue.sql"),
    ),
    (
        "0007_ai_triage",
        include_str!("persistence/migrations/0007_ai_triage.sql"),
    ),
    (
        "0008_outbox_jobs",
        include_str!("persistence/migrations/0008_outbox_jobs.sql"),
    ),
    (
        "0009_calendar_event_drafts",
        include_str!("persistence/migrations/0009_calendar_event_drafts.sql"),
    ),
    (
        "0010_follow_up_tasks",
        include_str!("persistence/migrations/0010_follow_up_tasks.sql"),
    ),
    (
        "0011_ai_usage_log",
        include_str!("persistence/migrations/0011_ai_usage_log.sql"),
    ),
    (
        "0012_ai_triage_generation",
        include_str!("persistence/migrations/0012_ai_triage_generation.sql"),
    ),
    (
        "0013_crm_note_drafts",
        include_str!("persistence/migrations/0013_crm_note_drafts.sql"),
    ),
    (
        "0014_email_reply_drafts",
        include_str!("persistence/migrations/0014_email_reply_drafts.sql"),
    ),
    (
        "0015_operator_notes",
        include_str!("persistence/migrations/0015_operator_notes.sql"),
    ),
    (
        "0016_work_queue_policy_auto_produce",
        include_str!("persistence/migrations/0016_work_queue_policy_auto_produce.sql"),
    ),
    (
        "0017_calendar_draft_calendar_id",
        include_str!("persistence/migrations/0017_calendar_draft_calendar_id.sql"),
    ),
    (
        "0018_operator_users",
        include_str!("persistence/migrations/0018_operator_users.sql"),
    ),
    (
        "0019_per_user_google_credentials",
        include_str!("persistence/migrations/0019_per_user_google_credentials.sql"),
    ),
    (
        "0020_operator_user_default_calendar",
        include_str!("persistence/migrations/0020_operator_user_default_calendar.sql"),
    ),
    (
        "0021_qbo_views",
        include_str!("persistence/migrations/0021_qbo_views.sql"),
    ),
    (
        "0022_qbo_pnl_snapshots",
        include_str!("persistence/migrations/0022_qbo_pnl_snapshots.sql"),
    ),
    (
        "0023_accounting_rename",
        include_str!("persistence/migrations/0023_accounting_rename.sql"),
    ),
    (
        "0024_ledger_entry_drafts",
        include_str!("persistence/migrations/0024_ledger_entry_drafts.sql"),
    ),
    (
        "0025_inventory_views",
        include_str!("persistence/migrations/0025_inventory_views.sql"),
    ),
    (
        "0026_drive_corpus",
        include_str!("persistence/migrations/0026_drive_corpus.sql"),
    ),
    (
        "0027_content_drafts",
        include_str!("persistence/migrations/0027_content_drafts.sql"),
    ),
    (
        "0028_claim_drafts",
        include_str!("persistence/migrations/0028_claim_drafts.sql"),
    ),
    (
        "0029_owner_reports",
        include_str!("persistence/migrations/0029_owner_reports.sql"),
    ),
    (
        "0030_invoice_drafts",
        include_str!("persistence/migrations/0030_invoice_drafts.sql"),
    ),
    (
        "0031_crm_record_drafts",
        include_str!("persistence/migrations/0031_crm_record_drafts.sql"),
    ),
    (
        "0032_crm_record_enrichment_trace",
        include_str!("persistence/migrations/0032_crm_record_enrichment_trace.sql"),
    ),
    (
        "0033_client_profile",
        include_str!("persistence/migrations/0033_client_profile.sql"),
    ),
    (
        "0034_crm_record_company_description",
        include_str!("persistence/migrations/0034_crm_record_company_description.sql"),
    ),
    (
        "0035_ai_usage_error_message",
        include_str!("persistence/migrations/0035_ai_usage_error_message.sql"),
    ),
    (
        "0036_llm_route_settings",
        include_str!("persistence/migrations/0036_llm_route_settings.sql"),
    ),
    (
        "0037_enrichment_runs",
        include_str!("persistence/migrations/0037_enrichment_runs.sql"),
    ),
    (
        "0038_content_web_facts",
        include_str!("persistence/migrations/0038_content_web_facts.sql"),
    ),
    (
        "0039_work_item_produce_guidance",
        include_str!("persistence/migrations/0039_work_item_produce_guidance.sql"),
    ),
    (
        "0040_email_inbound_body_full",
        include_str!("persistence/migrations/0040_email_inbound_body_full.sql"),
    ),
    (
        "0041_work_queue_policy_ai_suggestible_kinds",
        include_str!("persistence/migrations/0041_work_queue_policy_ai_suggestible_kinds.sql"),
    ),
    (
        "0042_invoice_settings",
        include_str!("persistence/migrations/0042_invoice_settings.sql"),
    ),
    (
        "0043_email_triage_category_agent_defaults",
        include_str!("persistence/migrations/0043_email_triage_category_agent_defaults.sql"),
    ),
    (
        "0044_panic_diagnostics",
        include_str!("persistence/migrations/0044_panic_diagnostics.sql"),
    ),
    (
        "0045_workflow_runs",
        include_str!("persistence/migrations/0045_workflow_runs.sql"),
    ),
    (
        "0046_quote_workflow",
        include_str!("persistence/migrations/0046_quote_workflow.sql"),
    ),
    (
        "0047_correlation_indexes",
        include_str!("persistence/migrations/0047_correlation_indexes.sql"),
    ),
    (
        "0048_workflow_step_trace_values",
        include_str!("persistence/migrations/0048_workflow_step_trace_values.sql"),
    ),
    (
        "0049_workflow_run_profile_id",
        include_str!("persistence/migrations/0049_workflow_run_profile_id.sql"),
    ),
    (
        "0050_draft_source_user_isolation",
        include_str!("persistence/migrations/0050_draft_source_user_isolation.sql"),
    ),
    (
        "0051_task_revision_backfill",
        include_str!("persistence/migrations/0051_task_revision_backfill.sql"),
    ),
    (
        "0052_lead_discovery",
        include_str!("persistence/migrations/0052_lead_discovery.sql"),
    ),
    (
        "0053_customer_tier_sync",
        include_str!("persistence/migrations/0053_customer_tier_sync.sql"),
    ),
    (
        "0054_quote_guardrails",
        include_str!("persistence/migrations/0054_quote_guardrails.sql"),
    ),
    (
        "0055_search_console",
        include_str!("persistence/migrations/0055_search_console.sql"),
    ),
    (
        "0056_lead_finding_evidence_column",
        include_str!("persistence/migrations/0056_lead_finding_evidence_column.sql"),
    ),
    (
        "0057_lead_finding_evidence_payloads",
        include_str!("persistence/migrations/0057_lead_finding_evidence_payloads.sql"),
    ),
    (
        "0058_operator_user_archive",
        include_str!("persistence/migrations/0058_operator_user_archive.sql"),
    ),
    (
        "0059_crm_sales_intent",
        include_str!("persistence/migrations/0059_crm_sales_intent.sql"),
    ),
    (
        "0060_accounting_customer_email_index",
        include_str!("persistence/migrations/0060_accounting_customer_email_index.sql"),
    ),
    (
        "0061_email_triage_fact_cache",
        include_str!("persistence/migrations/0061_email_triage_fact_cache.sql"),
    ),
    (
        "0062_email_triage_legacy_rule_cleanup",
        include_str!("persistence/migrations/0062_email_triage_legacy_rule_cleanup.sql"),
    ),
    (
        "0063_crm_record_multiple_drafts",
        include_str!("persistence/migrations/0063_crm_record_multiple_drafts.sql"),
    ),
    (
        "0064_call_inputs",
        include_str!("persistence/migrations/0064_call_inputs.sql"),
    ),
    (
        "0065_inventory_order_mapping_depletion",
        include_str!("persistence/migrations/0065_inventory_order_mapping_depletion.sql"),
    ),
    (
        "0066_email_follow_up_workflows",
        include_str!("persistence/migrations/0066_email_follow_up_workflows.sql"),
    ),
    (
        "0067_home_dashboard",
        include_str!("persistence/migrations/0067_home_dashboard.sql"),
    ),
    (
        "0068_follow_up_task_source_user",
        include_str!("persistence/migrations/0068_follow_up_task_source_user.sql"),
    ),
    (
        "0069_email_triage_inbox_settings",
        include_str!("persistence/migrations/0069_email_triage_inbox_settings.sql"),
    ),
    (
        "0070_email_attachments_agent_evidence",
        include_str!("persistence/migrations/0070_email_attachments_agent_evidence.sql"),
    ),
    (
        "0071_accounting_bill_cash_snapshots",
        include_str!("persistence/migrations/0071_accounting_bill_cash_snapshots.sql"),
    ),
    (
        "0072_release_notes",
        include_str!("persistence/migrations/0072_release_notes.sql"),
    ),
    (
        "0073_release_note_dismissals",
        include_str!("persistence/migrations/0073_release_note_dismissals.sql"),
    ),
    (
        "0074_work_item_visibility_assignment",
        include_str!("persistence/migrations/0074_work_item_visibility_assignment.sql"),
    ),
    (
        "0075_email_inbound_source_key",
        include_str!("persistence/migrations/0075_email_inbound_source_key.sql"),
    ),
    (
        "0076_google_analytics",
        include_str!("persistence/migrations/0076_google_analytics.sql"),
    ),
    (
        "0077_search_console_properties",
        include_str!("persistence/migrations/0077_search_console_properties.sql"),
    ),
    (
        "0078_claim_drafts_provider_context",
        include_str!("persistence/migrations/0078_claim_drafts_provider_context.sql"),
    ),
    (
        "0079_call_inputs_transcription_meta",
        include_str!("persistence/migrations/0079_call_inputs_transcription_meta.sql"),
    ),
    (
        "0080_home_dashboard_hubspot_deals",
        include_str!("persistence/migrations/0080_home_dashboard_hubspot_deals.sql"),
    ),
    (
        "0081_call_input_drive_settings",
        include_str!("persistence/migrations/0081_call_input_drive_settings.sql"),
    ),
    (
        "0082_packet_proposals",
        include_str!("persistence/migrations/0082_packet_proposals.sql"),
    ),
    (
        "0083_packet_proposal_evidence",
        include_str!("persistence/migrations/0083_packet_proposal_evidence.sql"),
    ),
    (
        "0084_runtime_setting_overrides",
        include_str!("persistence/migrations/0084_runtime_setting_overrides.sql"),
    ),
    (
        "0085_grounding_evidence",
        include_str!("persistence/migrations/0085_grounding_evidence.sql"),
    ),
    (
        "0086_shopify_sales_snapshots",
        include_str!("persistence/migrations/0086_shopify_sales_snapshots.sql"),
    ),
    (
        "0087_crm_cache",
        include_str!("persistence/migrations/0087_crm_cache.sql"),
    ),
    (
        "0088_rename_renamed_cap_override_keys",
        include_str!("persistence/migrations/0088_rename_renamed_cap_override_keys.sql"),
    ),
    (
        "0089_drive_corpus_settings",
        include_str!("persistence/migrations/0089_drive_corpus_settings.sql"),
    ),
    (
        "0090_claim_shipment_refs",
        include_str!("persistence/migrations/0090_claim_shipment_refs.sql"),
    ),
    (
        "0091_email_inbound_sender_email",
        include_str!("persistence/migrations/0091_email_inbound_sender_email.sql"),
    ),
    (
        "0092_work_queue_ai_gmail_scope",
        include_str!("persistence/migrations/0092_work_queue_ai_gmail_scope.sql"),
    ),
    (
        "0093_email_inbound_safe_headers",
        include_str!("persistence/migrations/0093_email_inbound_safe_headers.sql"),
    ),
    (
        "0094_work_queue_ai_gmail_scope_mode",
        include_str!("persistence/migrations/0094_work_queue_ai_gmail_scope_mode.sql"),
    ),
    (
        "0095_email_inbound_enrichments",
        include_str!("persistence/migrations/0095_email_inbound_enrichments.sql"),
    ),
    (
        "0096_email_inbound_sender_identity_facts",
        include_str!("persistence/migrations/0096_email_inbound_sender_identity_facts.sql"),
    ),
    (
        "0097_email_inbound_represented_identity",
        include_str!("persistence/migrations/0097_email_inbound_represented_identity.sql"),
    ),
    (
        "0098_content_plans",
        include_str!("persistence/migrations/0098_content_plans.sql"),
    ),
    (
        "0099_email_reply_draft_reply_all",
        include_str!("persistence/migrations/0099_email_reply_draft_reply_all.sql"),
    ),
    (
        "0100_email_reply_draft_reply_headers",
        include_str!("persistence/migrations/0100_email_reply_draft_reply_headers.sql"),
    ),
    (
        "0101_email_inbound_body_html",
        include_str!("persistence/migrations/0101_email_inbound_body_html.sql"),
    ),
    (
        "0102_gmail_ingest_cursors",
        include_str!("persistence/migrations/0102_gmail_ingest_cursors.sql"),
    ),
    (
        "0103_calendar_draft_attendees",
        include_str!("persistence/migrations/0103_calendar_draft_attendees.sql"),
    ),
    (
        "0104_content_draft_publishing",
        include_str!("persistence/migrations/0104_content_draft_publishing.sql"),
    ),
    (
        "0105_connector_oauth_states",
        include_str!("persistence/migrations/0105_connector_oauth_states.sql"),
    ),
    (
        "0106_social_publishing",
        include_str!("persistence/migrations/0106_social_publishing.sql"),
    ),
    (
        "0107_content_campaign_studio",
        include_str!("persistence/migrations/0107_content_campaign_studio.sql"),
    ),
    (
        "0108_inventory_stock_behavior",
        include_str!("persistence/migrations/0108_inventory_stock_behavior.sql"),
    ),
    (
        "0109_inventory_sync_error_class",
        include_str!("persistence/migrations/0109_inventory_sync_error_class.sql"),
    ),
    (
        "0110_inventory_qty_parity",
        include_str!("persistence/migrations/0110_inventory_qty_parity.sql"),
    ),
    (
        "0111_inventory_line_identity",
        include_str!("persistence/migrations/0111_inventory_line_identity.sql"),
    ),
    (
        "0112_qbo_reconnect_state",
        include_str!("persistence/migrations/0112_qbo_reconnect_state.sql"),
    ),
];

fn migrations() -> Migrations<'static> {
    Migrations::new(
        MIGRATION_SOURCES
            .iter()
            .map(|(_, sql)| M::up(sql))
            .collect(),
    )
}

fn run_post_migration_cleanups(conn: &mut Connection) -> Result<(), PersistenceError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    crate::slices::email_triage::store::cleanup_legacy_rule_json(conn, now_ms)
        .map(|_| ())
        .map_err(|err| PersistenceError::Migration(err.to_string()))
}

pub struct Persistence {
    conn: Connection,
}

/// Pooled SQLite persistence layer. Clone is cheap (Arc-backed internally).
/// This is the swap point for a future Postgres impl: `store_core::mutate`
/// continues to take `&mut Connection` unchanged.
#[derive(Clone)]
pub struct PersistencePool {
    pool: Pool<SqliteConnectionManager>,
}

/// An acquired pooled connection. Returned to the pool on drop.
/// Provides the same `.connection()` / `.connection_ref()` API as `Persistence`.
pub struct PersistenceConn(r2d2::PooledConnection<SqliteConnectionManager>);

impl PersistenceConn {
    pub fn connection(&mut self) -> &mut Connection {
        &mut self.0
    }

    pub fn connection_ref(&self) -> &Connection {
        &self.0
    }
}

static IN_MEMORY_COUNTER: AtomicU64 = AtomicU64::new(0);

impl PersistencePool {
    pub fn open_at(state_dir: &Path) -> Result<Self, PersistenceError> {
        std::fs::create_dir_all(state_dir).map_err(|err| PersistenceError::Io(err.to_string()))?;
        let db_path = state_dir.join(DB_FILE_NAME);

        {
            let mut boot_conn = Connection::open(&db_path)
                .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
            Self::boot_initialize(&mut boot_conn)?;
        }

        let manager = SqliteConnectionManager::file(&db_path).with_init(|conn| {
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            Ok(())
        });
        let pool = Pool::builder()
            .max_size(8)
            .connection_timeout(std::time::Duration::from_secs(5))
            .build(manager)
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
        Ok(Self { pool })
    }

    pub fn open_in_memory() -> Result<Self, PersistenceError> {
        Self::open_in_memory_inner(4, std::time::Duration::from_secs(5))
    }

    pub fn get(&self) -> Result<PersistenceConn, PersistenceError> {
        self.pool
            .get()
            .map(PersistenceConn)
            .map_err(|err| PersistenceError::Pool(err.to_string()))
    }

    /// Borrow a pooled connection. Named `.lock()` deliberately so that existing
    /// `state.persistence.lock()` call sites compile unchanged after the field
    /// type change from a mutex-wrapped `Persistence` to `PersistencePool`.
    pub fn lock(&self) -> PersistenceConn {
        self.get()
            .expect("persistence pool exhausted - this should not happen in normal operation")
    }

    pub fn try_lock(&self) -> Option<PersistenceConn> {
        self.pool.try_get().map(PersistenceConn)
    }

    pub fn schema_version(&self) -> u32 {
        self.get()
            .ok()
            .and_then(|conn| {
                conn.connection_ref()
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .map(|version| version as u32)
                    .ok()
            })
            .unwrap_or(0)
    }

    fn boot_initialize(conn: &mut Connection) -> Result<(), PersistenceError> {
        configure_incremental_auto_vacuum_for_new_db(conn)?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
        migrations()
            .to_latest(conn)
            .map_err(|err| PersistenceError::Migration(err.to_string()))?;
        run_post_migration_cleanups(conn)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory_with_config(
        max_size: u32,
        connection_timeout: std::time::Duration,
    ) -> Result<Self, PersistenceError> {
        Self::open_in_memory_inner(max_size, connection_timeout)
    }

    fn open_in_memory_inner(
        max_size: u32,
        connection_timeout: std::time::Duration,
    ) -> Result<Self, PersistenceError> {
        let id = IN_MEMORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let uri = format!("file:bos_test_{id}?mode=memory&cache=shared");
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
        let manager = SqliteConnectionManager::file(&uri)
            .with_flags(flags)
            .with_init(|conn| {
                conn.pragma_update(None, "foreign_keys", "ON")?;
                conn.busy_timeout(std::time::Duration::from_secs(5))?;
                Ok(())
            });
        let pool = Pool::builder()
            .max_size(max_size)
            .connection_timeout(connection_timeout)
            .build(manager)
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
        {
            let mut conn = pool
                .get()
                .map_err(|err| PersistenceError::Pool(err.to_string()))?;
            Self::boot_initialize(&mut conn)?;
        }
        Ok(Self { pool })
    }
}

impl Persistence {
    pub fn open_at(state_dir: &Path) -> Result<Self, PersistenceError> {
        std::fs::create_dir_all(state_dir).map_err(|err| PersistenceError::Io(err.to_string()))?;
        let mut conn = Connection::open(state_dir.join(DB_FILE_NAME))
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
        Self::initialize(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, PersistenceError> {
        let mut conn = Connection::open_in_memory()
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
        Self::initialize(&mut conn)?;
        Ok(Self { conn })
    }

    fn initialize(conn: &mut Connection) -> Result<(), PersistenceError> {
        configure_incremental_auto_vacuum_for_new_db(conn)?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
        migrations()
            .to_latest(conn)
            .map_err(|err| PersistenceError::Migration(err.to_string()))?;
        run_post_migration_cleanups(conn)
    }

    pub fn connection(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn connection_ref(&self) -> &Connection {
        &self.conn
    }

    pub fn schema_version(&self) -> u32 {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map(|version| version as u32)
            .unwrap_or(0)
    }
}

/// Incremental auto-vacuum must be selected before the first schema objects are
/// created. Existing databases are deliberately left untouched: changing their
/// mode would require a full VACUUM and potentially more free disk than a
/// volume-pressure incident has available.
fn configure_incremental_auto_vacuum_for_new_db(conn: &Connection) -> Result<(), PersistenceError> {
    let user_version = conn
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
    let user_table_count = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
    if user_version == 0 && user_table_count == 0 {
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .map_err(|err| PersistenceError::Sqlite(err.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceError {
    Io(String),
    Sqlite(String),
    Migration(String),
    Pool(String),
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "persistence io error: {msg}"),
            Self::Sqlite(msg) => write!(f, "sqlite error: {msg}"),
            Self::Migration(msg) => write!(f, "migration error: {msg}"),
            Self::Pool(msg) => write!(f, "persistence pool error: {msg}"),
        }
    }
}

impl std::error::Error for PersistenceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_registry_is_contiguous() {
        for (index, (name, _)) in MIGRATION_SOURCES.iter().enumerate() {
            let expected = format!("{:04}_", index + 1);
            assert!(
                name.starts_with(&expected),
                "migration {name} out of order: expected prefix {expected}"
            );
        }
    }

    #[test]
    fn migrations_apply_cleanly() {
        Persistence::open_in_memory().expect("in-memory persistence with migrations");
    }

    #[test]
    fn new_database_enables_incremental_auto_vacuum_before_migrations() {
        let state_dir = test_state_dir("new-incremental");
        let pool = PersistencePool::open_at(&state_dir).expect("new database");
        let conn = pool.lock();
        let mode: i64 = conn
            .connection_ref()
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .expect("auto_vacuum");
        let version: i64 = conn
            .connection_ref()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version");
        assert_eq!(mode, 2, "new databases must use incremental auto-vacuum");
        assert_eq!(version as usize, MIGRATION_SOURCES.len());
        drop(conn);
        drop(pool);
        std::fs::remove_dir_all(state_dir).expect("cleanup");
    }

    #[test]
    fn existing_database_auto_vacuum_mode_is_not_changed() {
        let state_dir = test_state_dir("existing-unchanged");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let db_path = state_dir.join(DB_FILE_NAME);
        let conn = Connection::open(&db_path).expect("legacy database");
        conn.execute("CREATE TABLE legacy_guard (id INTEGER PRIMARY KEY)", [])
            .expect("legacy table");
        let before: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .expect("legacy auto_vacuum");
        assert_eq!(before, 0);
        drop(conn);

        let pool = PersistencePool::open_at(&state_dir).expect("open existing database");
        let conn = pool.lock();
        let after: i64 = conn
            .connection_ref()
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .expect("existing auto_vacuum");
        assert_eq!(
            after, 0,
            "opening an existing database must not change mode"
        );
        drop(conn);
        drop(pool);
        std::fs::remove_dir_all(state_dir).expect("cleanup");
    }

    fn test_state_dir(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bos-persistence-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn migration_0088_rekeys_renamed_runtime_setting_overrides() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0088: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0088_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        Migrations::new(pre_0088)
            .to_latest(&mut conn)
            .expect("apply pre-0088 migrations");

        conn.execute(
            "INSERT INTO runtime_setting_overrides \
             (client_id, var_name, value, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["client", "BOS_PRODUCE_MAX_PER_CYCLE", "7", 1_i64],
        )
        .expect("insert old produce override");
        conn.execute(
            "INSERT INTO entity_revisions \
             (client_id, entity_kind, entity_id, revision, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "client",
                "runtime_setting_override",
                "BOS_PRODUCE_MAX_PER_CYCLE",
                7_i64,
                1_i64
            ],
        )
        .expect("insert old produce revision");
        conn.execute(
            "INSERT INTO runtime_setting_overrides \
             (client_id, var_name, value, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["client", "BOS_AUTO_PRODUCE_MAX_PER_CYCLE", "3", 2_i64],
        )
        .expect("insert conflicting new produce override");
        conn.execute(
            "INSERT INTO entity_revisions \
             (client_id, entity_kind, entity_id, revision, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "client",
                "runtime_setting_override",
                "BOS_AUTO_PRODUCE_MAX_PER_CYCLE",
                3_i64,
                2_i64
            ],
        )
        .expect("insert conflicting new produce revision");
        conn.execute(
            "INSERT INTO runtime_setting_overrides \
             (client_id, var_name, value, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["client", "BOS_AI_TRIAGE_MAX_PER_CYCLE", "9", 3_i64],
        )
        .expect("insert old triage override");
        conn.execute(
            "INSERT INTO entity_revisions \
             (client_id, entity_kind, entity_id, revision, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "client",
                "runtime_setting_override",
                "BOS_AI_TRIAGE_MAX_PER_CYCLE",
                9_i64,
                3_i64
            ],
        )
        .expect("insert old triage revision");

        migrations().to_latest(&mut conn).expect("apply 0088");

        let produce_value: String = conn
            .query_row(
                "SELECT value FROM runtime_setting_overrides \
                 WHERE client_id = 'client' AND var_name = 'BOS_AUTO_PRODUCE_MAX_PER_CYCLE'",
                [],
                |row| row.get(0),
            )
            .expect("renamed produce override");
        let triage_value: String = conn
            .query_row(
                "SELECT value FROM runtime_setting_overrides \
                 WHERE client_id = 'client' \
                 AND var_name = 'BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE'",
                [],
                |row| row.get(0),
            )
            .expect("renamed triage override");
        let produce_revision: i64 = conn
            .query_row(
                "SELECT revision FROM entity_revisions \
                 WHERE client_id = 'client' \
                 AND entity_kind = 'runtime_setting_override' \
                 AND entity_id = 'BOS_AUTO_PRODUCE_MAX_PER_CYCLE'",
                [],
                |row| row.get(0),
            )
            .expect("preserved produce revision");
        let triage_revision: i64 = conn
            .query_row(
                "SELECT revision FROM entity_revisions \
                 WHERE client_id = 'client' \
                 AND entity_kind = 'runtime_setting_override' \
                 AND entity_id = 'BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE'",
                [],
                |row| row.get(0),
            )
            .expect("renamed triage revision");
        let old_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM runtime_setting_overrides \
                 WHERE var_name IN ('BOS_PRODUCE_MAX_PER_CYCLE', \
                 'BOS_AI_TRIAGE_MAX_PER_CYCLE')",
                [],
                |row| row.get(0),
            )
            .expect("old override count");
        let old_revision_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM entity_revisions \
                 WHERE entity_kind = 'runtime_setting_override' \
                 AND entity_id IN ('BOS_PRODUCE_MAX_PER_CYCLE', \
                 'BOS_AI_TRIAGE_MAX_PER_CYCLE')",
                [],
                |row| row.get(0),
            )
            .expect("old revision count");

        assert_eq!(produce_value, "3");
        assert_eq!(triage_value, "9");
        assert_eq!(produce_revision, 3);
        assert_eq!(triage_revision, 9);
        assert_eq!(old_count, 0);
        assert_eq!(old_revision_count, 0);
    }

    /// A credential connected before multi-user logins (one row per client +
    /// service) must survive 0019 keyed to the shared 'operator' identity —
    /// that's what keeps a deployed single-account instance working.
    #[test]
    fn migration_0019_rekeys_the_legacy_credential_to_operator() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0019: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0019_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0019.len(), 18, "0019 must follow 0018");
        Migrations::new(pre_0019)
            .to_latest(&mut conn)
            .expect("apply pre-0019 migrations");
        conn.execute(
            "INSERT INTO google_oauth_credentials \
             (client_id, service, refresh_token, scopes_json, connected_at_ms) \
             VALUES ('demo', 'gmail', 'rt-legacy', '[\"scope-a\"]', 5)",
            [],
        )
        .expect("insert legacy credential");

        migrations().to_latest(&mut conn).expect("apply 0019+");

        let (user_id, token, scopes): (String, String, String) = conn
            .query_row(
                "SELECT user_id, refresh_token, scopes_json \
                 FROM google_oauth_credentials WHERE client_id = 'demo' AND service = 'gmail'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("migrated row");
        assert_eq!(user_id, "operator");
        assert_eq!(token, "rt-legacy");
        assert_eq!(scopes, "[\"scope-a\"]");
    }

    #[test]
    fn migration_0051_backfills_missing_task_revisions_only() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0051: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0051_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0051.len(), 50, "0051 must follow 0050");
        Migrations::new(pre_0051)
            .to_latest(&mut conn)
            .expect("apply pre-0051 migrations");

        conn.execute(
            "INSERT INTO tasks \
             (client_id, task_id, title, due_date, context, source_kind, source_ref, \
              status, created_at_ms, updated_at_ms) \
             VALUES ('demo', 'task_missing_rev', 'missing', NULL, '', 'manual', 'a', \
                     'open', 1, 11)",
            [],
        )
        .expect("insert task without revision");
        conn.execute(
            "INSERT INTO tasks \
             (client_id, task_id, title, due_date, context, source_kind, source_ref, \
              status, created_at_ms, updated_at_ms) \
             VALUES ('demo', 'task_existing_rev', 'existing', NULL, '', 'manual', 'b', \
                     'done', 2, 22)",
            [],
        )
        .expect("insert task with revision");
        conn.execute(
            "INSERT INTO entity_revisions \
             (client_id, entity_kind, entity_id, revision, updated_at_ms) \
             VALUES ('demo', 'task', 'task_existing_rev', 7, 17)",
            [],
        )
        .expect("insert existing revision");

        migrations().to_latest(&mut conn).expect("apply 0051");

        let missing: (i64, i64) = conn
            .query_row(
                "SELECT revision, updated_at_ms FROM entity_revisions \
                 WHERE client_id = 'demo' AND entity_kind = 'task' \
                   AND entity_id = 'task_missing_rev'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("backfilled revision");
        assert_eq!(missing, (1, 11));

        let existing: (i64, i64) = conn
            .query_row(
                "SELECT revision, updated_at_ms FROM entity_revisions \
                 WHERE client_id = 'demo' AND entity_kind = 'task' \
                   AND entity_id = 'task_existing_rev'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("existing revision");
        assert_eq!(existing, (7, 17));
    }

    #[test]
    fn migration_0057_rewrites_legacy_lead_finding_evidence_payloads() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0057: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0057_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0057.len(), 56, "0057 must follow 0056");
        Migrations::new(pre_0057)
            .to_latest(&mut conn)
            .expect("apply pre-0057 migrations");

        conn.execute(
            "INSERT INTO lead_findings \
             (client_id, finding_id, source_id, status, title, summary, contact_hint, \
              company_hint, matched_terms_json, evidence_json, work_item_id, \
              created_at_ms, updated_at_ms) \
             VALUES ('demo', 'lead_stage-1', 'forum_boat_restoration', 'staged', \
                     'Owner needs furniture repair advice', \
                     'A homeowner asked for furniture repair recommendations.', NULL, NULL, '[]', \
                     '{\"source_id\":\"forum_boat_restoration\",\
                       \"source_display_name\":\"Boat Restoration Forum\",\
                       \"source_url\":\"https://example.test/forum\",\
                       \"item_url\":\"https://example.test/forum/thread-1\",\
                       \"captured_at_ms\":1700000,\
                       \"evidence_quote\":\"Looking for a recommendation on furniture repair.\"}', \
                     NULL, 1, 1)",
            [],
        )
        .expect("insert legacy lead finding");

        migrations().to_latest(&mut conn).expect("apply 0057");

        let evidence_json: String = conn
            .query_row(
                "SELECT evidence_json FROM lead_findings \
                 WHERE client_id = 'demo' AND finding_id = 'lead_stage-1'",
                [],
                |row| row.get(0),
            )
            .expect("migrated evidence");
        let evidence: bos_contracts::source::EvidenceRecord =
            serde_json::from_str(&evidence_json).expect("evidence envelope");
        assert_eq!(evidence.evidence_id, "lead_evidence_lead_stage-1");
        assert_eq!(evidence.source.source_id, "forum_boat_restoration");
        assert_eq!(evidence.source.display_name, "Boat Restoration Forum");
        assert_eq!(
            evidence.policy.access_mode,
            bos_contracts::source::EvidenceAccessMode::ApprovedSourceImport
        );
        assert!(!evidence.policy.automated_outreach_allowed);
        assert_eq!(
            evidence.evidence_quote,
            "Looking for a recommendation on furniture repair."
        );
    }

    #[test]
    fn migration_0068_backfills_follow_up_task_source_users() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0066: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0066_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0066.len(), 65, "0066 must follow 0065");
        Migrations::new(pre_0066)
            .to_latest(&mut conn)
            .expect("apply pre-0066 migrations");

        conn.execute(
            "INSERT INTO work_items \
             (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
              packet_kinds_json, status, created_at_ms, updated_at_ms, source_user_id) \
             VALUES ('demo', 'wi_email_m1', 'email', 'm1', 'inquiries', 'Follow up', '', \
                     '[\"follow_up_task\"]', 'accepted', 1, 1, 'user_jordan')",
            [],
        )
        .expect("insert source-scoped work item");
        conn.execute(
            "INSERT INTO follow_up_task_drafts \
             (client_id, draft_id, item_id, source_kind, source_ref, status, title, \
              due_date, context, provenance_json, model, confidence, task_id, \
              created_at_ms, updated_at_ms) \
             VALUES ('demo', 'fud_wi_email_m1_1', 'wi_email_m1', 'email', 'm1', \
                     'approved', 'Reply', NULL, '', '[]', 'test-model', 'high', \
                     'task_fud_wi_email_m1_1', 2, 3)",
            [],
        )
        .expect("insert legacy follow-up draft");
        conn.execute(
            "INSERT INTO tasks \
             (client_id, task_id, title, due_date, context, source_kind, source_ref, \
              status, created_at_ms, updated_at_ms) \
             VALUES ('demo', 'task_fud_wi_email_m1_1', 'Reply', NULL, '', 'email', 'm1', \
                     'open', 4, 5)",
            [],
        )
        .expect("insert legacy task");

        migrations().to_latest(&mut conn).expect("apply 0066");

        let draft_user: Option<String> = conn
            .query_row(
                "SELECT source_user_id FROM follow_up_task_drafts \
                 WHERE client_id = 'demo' AND draft_id = 'fud_wi_email_m1_1'",
                [],
                |row| row.get(0),
            )
            .expect("draft source user");
        assert_eq!(draft_user.as_deref(), Some("user_jordan"));

        let task_user: Option<String> = conn
            .query_row(
                "SELECT source_user_id FROM tasks \
                 WHERE client_id = 'demo' AND task_id = 'task_fud_wi_email_m1_1'",
                [],
                |row| row.get(0),
            )
            .expect("task source user");
        assert_eq!(task_user.as_deref(), Some("user_jordan"));
    }

    #[test]
    fn migration_0074_backfills_work_item_visibility_from_source_users_only() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0074: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0074_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0074.len(), 73, "0074 must follow 0073");
        Migrations::new(pre_0074)
            .to_latest(&mut conn)
            .expect("apply pre-0074 migrations");

        conn.execute(
            "INSERT INTO work_items \
             (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
              packet_kinds_json, status, created_at_ms, updated_at_ms, source_user_id) \
             VALUES ('demo', 'wi_email_jordan', 'email', 'm1', 'inquiries', 'Jordan item', '', \
                     '[\"follow_up_task\"]', 'open', 10, 11, 'user_jordan')",
            [],
        )
        .expect("insert source-scoped work item");
        conn.execute(
            "INSERT INTO work_items \
             (client_id, item_id, source_kind, source_ref, category_id, title, summary, \
              packet_kinds_json, status, created_at_ms, updated_at_ms, source_user_id) \
             VALUES ('demo', 'wi_legacy_null', 'operator_note', 'note1', 'operator_note', \
                     'Legacy', '', '[\"crm_activity\"]', 'open', 20, 21, NULL)",
            [],
        )
        .expect("insert null-source work item");

        migrations().to_latest(&mut conn).expect("apply 0074");

        let jordan_row: (String, i64) = conn
            .query_row(
                "SELECT user_id, created_at_ms FROM work_item_visibility \
                 WHERE client_id = 'demo' AND item_id = 'wi_email_jordan'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("visibility backfill");
        assert_eq!(jordan_row, ("user_jordan".to_string(), 10));

        let null_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM work_item_visibility \
                 WHERE client_id = 'demo' AND item_id = 'wi_legacy_null'",
                [],
                |row| row.get(0),
            )
            .expect("null row count");
        assert_eq!(null_rows, 0);

        let assignee_column: Option<String> = conn
            .query_row(
                "SELECT assignee_user_id FROM work_items \
                 WHERE client_id = 'demo' AND item_id = 'wi_email_jordan'",
                [],
                |row| row.get(0),
            )
            .expect("assignee column");
        assert_eq!(assignee_column, None);
    }

    #[test]
    fn migration_0075_preserves_legacy_inbound_rows_with_source_key() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0075: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0075_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0075.len(), 74, "0075 must follow 0074");
        Migrations::new(pre_0075)
            .to_latest(&mut conn)
            .expect("apply pre-0075 migrations");

        conn.execute(
            "INSERT INTO email_inbound_messages \
             (client_id, message_id, thread_id, internal_date_ms, from_addr, to_addr, subject, \
              body_excerpt, labels_json, resolved_category, matched_rule_id, ingested_at_ms, \
              source_user_id, body_full, attachments_json) \
             VALUES ('demo', 'gmail-msg-1', 'thread-1', 1000, 'a@example.com', 'ops@example.com', \
                     'Subject', 'Body', '[]', 'inquiries', NULL, 2000, 'user_jordan', 'Body', '[]')",
            [],
        )
        .expect("insert legacy inbound message");

        migrations().to_latest(&mut conn).expect("apply 0075");

        let row: (String, String, Option<String>) = conn
            .query_row(
                "SELECT source_key, message_id, source_user_id FROM email_inbound_messages \
                 WHERE client_id = 'demo'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("migrated inbound row");
        assert_eq!(
            row,
            (
                "gmail-msg-1".to_string(),
                "gmail-msg-1".to_string(),
                Some("user_jordan".to_string())
            )
        );
    }

    #[test]
    fn migration_0096_backfills_sender_identity_header_reasons() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0096: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0096_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0096.len(), 95, "0096 must follow 0095");
        Migrations::new(pre_0096)
            .to_latest(&mut conn)
            .expect("apply pre-0096 migrations");

        conn.execute(
            "INSERT INTO email_inbound_messages \
             (client_id, source_key, message_id, body_excerpt, labels_json, resolved_category, \
              ingested_at_ms, body_full, attachments_json, sender_email, headers_json) \
             VALUES \
             ('demo', 'auto', 'auto', '', '[]', 'inquiries', 1, '', '[]', 'ada@example.com', \
              '[[\"Auto-Submitted\", \"auto-generated\"]]'), \
             ('demo', 'list', 'list', '', '[]', 'inquiries', 1, '', '[]', 'list@example.com', \
              '[[\"List-Id\", \"Customers <customers.example.com>\"]]'), \
             ('demo', 'bulk', 'bulk', '', '[]', 'inquiries', 1, '', '[]', 'bulk@example.com', \
              '[[\"Precedence\", \"bulk\"]]'), \
             ('demo', 'normal', 'normal', '', '[]', 'inquiries', 1, '', '[]', 'dana@example.com', \
              '[]')",
            [],
        )
        .expect("insert legacy inbound messages");

        migrations().to_latest(&mut conn).expect("apply 0096");

        let mut stmt = conn
            .prepare(
                "SELECT source_key, sender_header_identity_blocked, sender_identity_block_reason \
                 FROM email_inbound_messages WHERE client_id = 'demo' ORDER BY source_key",
            )
            .expect("prepare sender identity query");
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .expect("query sender identity rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("sender identity rows");

        assert_eq!(
            rows,
            vec![
                (
                    "auto".to_string(),
                    true,
                    Some("automated_email_headers".to_string())
                ),
                (
                    "bulk".to_string(),
                    true,
                    Some("bulk_email_headers".to_string())
                ),
                (
                    "list".to_string(),
                    true,
                    Some("mailing_list_headers".to_string())
                ),
                ("normal".to_string(), false, None),
            ]
        );
    }

    #[test]
    fn migration_0097_adds_represented_identity_columns() {
        let mut conn = Connection::open_in_memory().expect("db");
        let pre_0097: Vec<M> = MIGRATION_SOURCES
            .iter()
            .take_while(|(name, _)| !name.starts_with("0097_"))
            .map(|(_, sql)| M::up(sql))
            .collect();
        assert_eq!(pre_0097.len(), 96, "0097 must follow 0096");
        Migrations::new(pre_0097)
            .to_latest(&mut conn)
            .expect("apply pre-0097 migrations");

        conn.execute(
            "INSERT INTO email_inbound_messages \
             (client_id, source_key, message_id, body_excerpt, labels_json, resolved_category, \
              ingested_at_ms, body_full, attachments_json, sender_email, headers_json) \
             VALUES \
             ('demo', 'legacy', 'legacy', '', '[]', 'inquiries', 1, '', '[]', \
              'ada@example.com', '[]')",
            [],
        )
        .expect("insert legacy inbound message");

        migrations().to_latest(&mut conn).expect("apply 0097");

        let represented: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT represented_email, represented_domain \
                 FROM email_inbound_messages WHERE client_id = 'demo' AND source_key = 'legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("represented identity columns");
        assert_eq!(represented, (None, None));
    }
}
