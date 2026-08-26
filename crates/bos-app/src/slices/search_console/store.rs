use bos_contracts::receipt::ActorKindDto;
use bos_contracts::search_console::{
    AnalyticsBreakdownRow, AnalyticsMetricTotals, SearchConsoleBreakdownRow,
    SearchConsoleMetricTotals, SearchConsoleProperty,
};
use bos_integrations::google_search_console::SearchConsoleSite;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::store_core::{self, MutationOutcome, MutationRequest, StoreError};

pub const CURSOR_ENTITY_KIND: &str = "search_console_cursor";
pub const PROPERTY_DISCOVERY_ENTITY_KIND: &str = "search_console_property_discovery";
pub const PROPERTY_SELECTION_ENTITY_KIND: &str = "search_console_property_selection";
pub const PROPERTY_SELECTION_ENTITY_ID: &str = "active";
pub const DISCOVERY_CURSOR_KEY: &str = "search_console_discovery";
pub const SNAPSHOT_ENTITY_KIND: &str = "search_console_snapshot";
pub const ANALYTICS_CURSOR_ENTITY_KIND: &str = "google_analytics_cursor";
pub const ANALYTICS_SNAPSHOT_ENTITY_KIND: &str = "google_analytics_snapshot";
pub const SYNC_ACTOR: &str = "search_console_sync_pump";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchConsoleCursor {
    pub config_hash: String,
    pub synced_start_date: Option<String>,
    pub synced_end_date: Option<String>,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_synced_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyticsCursor {
    pub config_hash: String,
    pub synced_start_date: Option<String>,
    pub synced_end_date: Option<String>,
    pub rate_limited_until_ms: u64,
    pub last_error: Option<String>,
    pub last_synced_at_ms: Option<u64>,
}

pub fn get_cursor(
    conn: &Connection,
    client_id: &str,
    property_url: &str,
) -> Result<SearchConsoleCursor, StoreError> {
    Ok(conn
        .query_row(
            "SELECT config_hash, synced_start_date, synced_end_date, \
             rate_limited_until_ms, last_error, last_synced_at_ms \
             FROM search_console_sync_cursors WHERE client_id = ?1 AND property_url = ?2",
            params![client_id, property_url],
            |row| {
                Ok(SearchConsoleCursor {
                    config_hash: row.get(0)?,
                    synced_start_date: row.get(1)?,
                    synced_end_date: row.get(2)?,
                    rate_limited_until_ms: row.get::<_, i64>(3)? as u64,
                    last_error: row.get(4)?,
                    last_synced_at_ms: row.get::<_, Option<i64>>(5)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub fn get_analytics_cursor(
    conn: &Connection,
    client_id: &str,
    property_id: &str,
) -> Result<AnalyticsCursor, StoreError> {
    Ok(conn
        .query_row(
            "SELECT config_hash, synced_start_date, synced_end_date, \
             rate_limited_until_ms, last_error, last_synced_at_ms \
             FROM google_analytics_sync_cursors WHERE client_id = ?1 AND property_id = ?2",
            params![client_id, property_id],
            |row| {
                Ok(AnalyticsCursor {
                    config_hash: row.get(0)?,
                    synced_start_date: row.get(1)?,
                    synced_end_date: row.get(2)?,
                    rate_limited_until_ms: row.get::<_, i64>(3)? as u64,
                    last_error: row.get(4)?,
                    last_synced_at_ms: row.get::<_, Option<i64>>(5)?.map(|ms| ms as u64),
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub fn put_analytics_cursor(
    conn: &mut Connection,
    client_id: &str,
    property_id: &str,
    cursor: &AnalyticsCursor,
    now_ms: u64,
) -> Result<(), StoreError> {
    let current = get_analytics_cursor(conn, client_id, property_id)?;
    if current == *cursor {
        return Ok(());
    }
    let idempotency_key = format!(
        "google_analytics_cursor:{property_id}:{}:{}:{}",
        cursor.synced_start_date.as_deref().unwrap_or(""),
        cursor.synced_end_date.as_deref().unwrap_or(""),
        cursor.last_error.as_deref().unwrap_or("")
    );
    let after = serde_json::json!({
        "property_id": property_id,
        "synced_start_date": cursor.synced_start_date,
        "synced_end_date": cursor.synced_end_date,
        "rate_limited_until_ms": cursor.rate_limited_until_ms,
        "last_error": cursor.last_error,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_property = property_id.to_string();
    let owned = cursor.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ANALYTICS_CURSOR_ENTITY_KIND,
            entity_id: property_id,
            change_kind: "advance",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO google_analytics_sync_cursors \
                 (client_id, property_id, config_hash, synced_start_date, synced_end_date, \
                  rate_limited_until_ms, last_error, last_synced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT (client_id, property_id) DO UPDATE SET \
                   config_hash = excluded.config_hash, \
                   synced_start_date = excluded.synced_start_date, \
                   synced_end_date = excluded.synced_end_date, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_synced_at_ms = excluded.last_synced_at_ms",
                params![
                    owned_client,
                    owned_property,
                    owned.config_hash,
                    owned.synced_start_date,
                    owned.synced_end_date,
                    owned.rate_limited_until_ms as i64,
                    owned.last_error,
                    owned.last_synced_at_ms.map(|ms| ms as i64),
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(())
}

pub fn latest_property(conn: &Connection, client_id: &str) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT property_url FROM search_console_sync_cursors \
             WHERE client_id = ?1 ORDER BY last_synced_at_ms DESC NULLS LAST, property_url LIMIT 1",
            params![client_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub fn list_properties(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<SearchConsoleProperty>, StoreError> {
    let selected = selected_property(conn, client_id)?.map(|selection| selection.site_url);
    let mut stmt = conn.prepare(
        "SELECT site_url, permission_level, discovered_at_ms, last_seen_at_ms \
         FROM search_console_properties \
         WHERE client_id = ?1 ORDER BY site_url",
    )?;
    let rows = stmt.query_map(params![client_id], |row| {
        let site_url: String = row.get(0)?;
        Ok(SearchConsoleProperty {
            selected: selected.as_deref() == Some(site_url.as_str()),
            site_url,
            permission_level: row.get(1)?,
            discovered_at_ms: row.get::<_, i64>(2).ok().map(|ms| ms as u64),
            last_seen_at_ms: row.get::<_, i64>(3).ok().map(|ms| ms as u64),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedProperty {
    pub site_url: String,
    pub revision: Option<u64>,
}

pub fn selected_property(
    conn: &Connection,
    client_id: &str,
) -> Result<Option<SelectedProperty>, StoreError> {
    let site_url: Option<String> = conn
        .query_row(
            "SELECT site_url FROM search_console_property_selection WHERE client_id = ?1",
            params![client_id],
            |row| row.get(0),
        )
        .optional()?;
    let revision = selection_revision(conn, client_id)?;
    Ok(site_url.map(|site_url| SelectedProperty { site_url, revision }))
}

pub fn selection_revision(conn: &Connection, client_id: &str) -> Result<Option<u64>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT revision FROM entity_revisions \
             WHERE client_id = ?1 AND entity_kind = ?2 AND entity_id = ?3",
            params![
                client_id,
                PROPERTY_SELECTION_ENTITY_KIND,
                PROPERTY_SELECTION_ENTITY_ID
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|revision| revision as u64))
}

pub fn replace_discovered_properties(
    conn: &mut Connection,
    client_id: &str,
    sites: &[SearchConsoleSite],
    now_ms: u64,
) -> Result<(), StoreError> {
    let mut deduped = Vec::<SearchConsoleSite>::new();
    for site in sites {
        if !deduped
            .iter()
            .any(|existing| existing.site_url == site.site_url)
        {
            deduped.push(site.clone());
        }
    }
    let after = serde_json::json!({
        "properties": deduped.iter().map(|site| &site.site_url).collect::<Vec<_>>(),
        "count": deduped.len(),
    })
    .to_string();
    let idempotency_key = format!("search_console_properties:{now_ms}:{}", deduped.len());
    let owned_client = client_id.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: PROPERTY_DISCOVERY_ENTITY_KIND,
            entity_id: "properties",
            change_kind: "replace",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            for site in &deduped {
                tx.execute(
                    "INSERT INTO search_console_properties \
                     (client_id, site_url, permission_level, discovered_at_ms, last_seen_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (client_id, site_url) DO UPDATE SET \
                       permission_level = excluded.permission_level, \
                       last_seen_at_ms = excluded.last_seen_at_ms",
                    params![
                        owned_client,
                        site.site_url,
                        site.permission_level,
                        now_ms as i64,
                        now_ms as i64,
                    ],
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

pub struct PropertySelectionContext<'a> {
    pub client_id: &'a str,
    pub actor_id: &'a str,
    pub expected_revision: Option<u64>,
    pub idempotency_key: &'a str,
    pub now_ms: u64,
}

pub fn select_property(
    conn: &mut Connection,
    ctx: PropertySelectionContext<'_>,
    site_url: &str,
) -> Result<MutationOutcome, StoreError> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM search_console_properties WHERE client_id = ?1 AND site_url = ?2",
            params![ctx.client_id, site_url],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Err(StoreError::Domain(
            "search_console_property_not_discovered".to_string(),
        ));
    }
    let before = selected_property(conn, ctx.client_id)?.map(|selection| selection.site_url);
    let after_json = serde_json::json!({ "site_url": site_url }).to_string();
    let owned_client = ctx.client_id.to_string();
    let owned_site_url = site_url.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id: ctx.client_id,
            entity_kind: PROPERTY_SELECTION_ENTITY_KIND,
            entity_id: PROPERTY_SELECTION_ENTITY_ID,
            change_kind: "select",
            actor_id: ctx.actor_id,
            actor_kind: ActorKindDto::Operator,
            expected_revision: ctx.expected_revision,
            idempotency_key: ctx.idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: before
                .map(|site_url| serde_json::json!({ "site_url": site_url }).to_string()),
            after_json: Some(after_json),
            now_ms: ctx.now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO search_console_property_selection (client_id, site_url, updated_at_ms) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT (client_id) DO UPDATE SET \
                   site_url = excluded.site_url, \
                   updated_at_ms = excluded.updated_at_ms",
                params![owned_client, owned_site_url, ctx.now_ms as i64],
            )?;
            Ok(())
        },
    )
}

pub fn put_cursor(
    conn: &mut Connection,
    client_id: &str,
    property_url: &str,
    cursor: &SearchConsoleCursor,
    now_ms: u64,
) -> Result<(), StoreError> {
    let current = get_cursor(conn, client_id, property_url)?;
    if current == *cursor {
        return Ok(());
    }
    let idempotency_key = format!(
        "search_console_cursor:{property_url}:{}:{}:{}",
        cursor.synced_start_date.as_deref().unwrap_or(""),
        cursor.synced_end_date.as_deref().unwrap_or(""),
        cursor.last_error.as_deref().unwrap_or("")
    );
    let after = serde_json::json!({
        "property_url": property_url,
        "synced_start_date": cursor.synced_start_date,
        "synced_end_date": cursor.synced_end_date,
        "rate_limited_until_ms": cursor.rate_limited_until_ms,
        "last_error": cursor.last_error,
    })
    .to_string();
    let owned_client = client_id.to_string();
    let owned_property = property_url.to_string();
    let owned = cursor.clone();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: CURSOR_ENTITY_KIND,
            entity_id: property_url,
            change_kind: "advance",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "INSERT INTO search_console_sync_cursors \
                 (client_id, property_url, config_hash, synced_start_date, synced_end_date, \
                  rate_limited_until_ms, last_error, last_synced_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT (client_id, property_url) DO UPDATE SET \
                   config_hash = excluded.config_hash, \
                   synced_start_date = excluded.synced_start_date, \
                   synced_end_date = excluded.synced_end_date, \
                   rate_limited_until_ms = excluded.rate_limited_until_ms, \
                   last_error = excluded.last_error, \
                   last_synced_at_ms = excluded.last_synced_at_ms",
                params![
                    owned_client,
                    owned_property,
                    owned.config_hash,
                    owned.synced_start_date,
                    owned.synced_end_date,
                    owned.rate_limited_until_ms as i64,
                    owned.last_error,
                    owned.last_synced_at_ms.map(|ms| ms as i64),
                ],
            )?;
            Ok(())
        },
    )?;
    Ok(())
}

fn totals_from_row(row: &Row<'_>) -> rusqlite::Result<SearchConsoleMetricTotals> {
    Ok(SearchConsoleMetricTotals {
        clicks: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
        impressions: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
        ctr_micros: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
        position_micros: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
    })
}

pub fn sum_daily(
    conn: &Connection,
    client_id: &str,
    property_url: &str,
    start_date: &str,
    end_date: &str,
) -> Result<SearchConsoleMetricTotals, StoreError> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(clicks), 0), COALESCE(SUM(impressions), 0), \
         CASE WHEN SUM(impressions) > 0 THEN SUM(ctr_micros * impressions) / SUM(impressions) ELSE 0 END, \
         CASE WHEN SUM(impressions) > 0 THEN SUM(position_micros * impressions) / SUM(impressions) ELSE 0 END \
         FROM search_console_daily_metrics \
         WHERE client_id = ?1 AND property_url = ?2 AND date >= ?3 AND date <= ?4",
        params![client_id, property_url, start_date, end_date],
        totals_from_row,
    )?)
}

pub fn sum_dimension(
    conn: &Connection,
    client_id: &str,
    property_url: &str,
    dimension_type: &str,
    is_branded: Option<bool>,
    start_date: &str,
    end_date: &str,
) -> Result<SearchConsoleMetricTotals, StoreError> {
    let branded_sql = match is_branded {
        Some(true) => "AND is_branded = 1",
        Some(false) => "AND is_branded = 0",
        None => "",
    };
    Ok(conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(clicks), 0), COALESCE(SUM(impressions), 0), \
             CASE WHEN SUM(impressions) > 0 THEN SUM(ctr_micros * impressions) / SUM(impressions) ELSE 0 END, \
             CASE WHEN SUM(impressions) > 0 THEN SUM(position_micros * impressions) / SUM(impressions) ELSE 0 END \
             FROM search_console_dimension_metrics \
             WHERE client_id = ?1 AND property_url = ?2 AND dimension_type = ?3 \
               AND date >= ?4 AND date <= ?5 {branded_sql}"
        ),
        params![client_id, property_url, dimension_type, start_date, end_date],
        totals_from_row,
    )?)
}

pub fn top_dimensions(
    conn: &Connection,
    client_id: &str,
    property_url: &str,
    dimension_type: &str,
    start_date: &str,
    end_date: &str,
    limit: usize,
) -> Result<Vec<SearchConsoleBreakdownRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT dimension_value, COALESCE(SUM(clicks), 0), COALESCE(SUM(impressions), 0), \
         CASE WHEN SUM(impressions) > 0 THEN SUM(ctr_micros * impressions) / SUM(impressions) ELSE 0 END, \
         CASE WHEN SUM(impressions) > 0 THEN SUM(position_micros * impressions) / SUM(impressions) ELSE 0 END \
         FROM search_console_dimension_metrics \
         WHERE client_id = ?1 AND property_url = ?2 AND dimension_type = ?3 \
           AND date >= ?4 AND date <= ?5 \
         GROUP BY dimension_value ORDER BY SUM(clicks) DESC, SUM(impressions) DESC LIMIT ?6",
    )?;
    let rows = stmt.query_map(
        params![
            client_id,
            property_url,
            dimension_type,
            start_date,
            end_date,
            limit as i64
        ],
        |row| {
            Ok(SearchConsoleBreakdownRow {
                value: row.get(0)?,
                metrics: SearchConsoleMetricTotals {
                    clicks: row.get(1)?,
                    impressions: row.get(2)?,
                    ctr_micros: row.get(3)?,
                    position_micros: row.get(4)?,
                },
            })
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn analytics_totals_from_row(row: &Row<'_>) -> rusqlite::Result<AnalyticsMetricTotals> {
    Ok(AnalyticsMetricTotals {
        sessions: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
        total_users: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
        event_count: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
        conversions: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
    })
}

pub fn sum_analytics_daily(
    conn: &Connection,
    client_id: &str,
    property_id: &str,
    start_date: &str,
    end_date: &str,
) -> Result<AnalyticsMetricTotals, StoreError> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(sessions), 0), COALESCE(SUM(total_users), 0), \
         COALESCE(SUM(event_count), 0), COALESCE(SUM(conversions), 0) \
         FROM google_analytics_daily_metrics \
         WHERE client_id = ?1 AND property_id = ?2 AND date >= ?3 AND date <= ?4",
        params![client_id, property_id, start_date, end_date],
        analytics_totals_from_row,
    )?)
}

pub fn top_analytics_dimensions(
    conn: &Connection,
    client_id: &str,
    property_id: &str,
    dimension_type: &str,
    start_date: &str,
    end_date: &str,
    limit: usize,
) -> Result<Vec<AnalyticsBreakdownRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT dimension_value, COALESCE(SUM(sessions), 0), COALESCE(SUM(total_users), 0), \
         COALESCE(SUM(event_count), 0), COALESCE(SUM(conversions), 0) \
         FROM google_analytics_dimension_metrics \
         WHERE client_id = ?1 AND property_id = ?2 AND dimension_type = ?3 \
           AND date >= ?4 AND date <= ?5 \
         GROUP BY dimension_value ORDER BY SUM(sessions) DESC, SUM(total_users) DESC LIMIT ?6",
    )?;
    let rows = stmt.query_map(
        params![
            client_id,
            property_id,
            dimension_type,
            start_date,
            end_date,
            limit as i64
        ],
        |row| {
            Ok(AnalyticsBreakdownRow {
                value: row.get(0)?,
                metrics: AnalyticsMetricTotals {
                    sessions: row.get(1)?,
                    total_users: row.get(2)?,
                    event_count: row.get(3)?,
                    conversions: row.get(4)?,
                },
            })
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn analytics_source_rows(
    conn: &Connection,
    client_id: &str,
    property_id: &str,
    start_date: &str,
    end_date: &str,
) -> Result<Vec<AnalyticsBreakdownRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT dimension_value, COALESCE(SUM(sessions), 0), COALESCE(SUM(total_users), 0), \
         COALESCE(SUM(event_count), 0), COALESCE(SUM(conversions), 0) \
         FROM google_analytics_dimension_metrics \
         WHERE client_id = ?1 AND property_id = ?2 AND dimension_type = 'source_medium' \
           AND date >= ?3 AND date <= ?4 \
         GROUP BY dimension_value ORDER BY SUM(sessions) DESC, SUM(total_users) DESC",
    )?;
    let rows = stmt.query_map(
        params![client_id, property_id, start_date, end_date],
        |row| {
            Ok(AnalyticsBreakdownRow {
                value: row.get(0)?,
                metrics: AnalyticsMetricTotals {
                    sessions: row.get(1)?,
                    total_users: row.get(2)?,
                    event_count: row.get(3)?,
                    conversions: row.get(4)?,
                },
            })
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyMetricRow {
    pub date: String,
    pub metrics: SearchConsoleMetricTotals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionMetricRow {
    pub date: String,
    pub dimension_type: String,
    pub dimension_value: String,
    pub is_branded: bool,
    pub metrics: SearchConsoleMetricTotals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsDailyMetricRow {
    pub date: String,
    pub metrics: AnalyticsMetricTotals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsDimensionMetricRow {
    pub date: String,
    pub dimension_type: String,
    pub dimension_value: String,
    pub metrics: AnalyticsMetricTotals,
}

pub struct AnalyticsSnapshotWindow<'a> {
    pub start_date: &'a str,
    pub end_date: &'a str,
    pub daily: &'a [AnalyticsDailyMetricRow],
    pub dimensions: &'a [AnalyticsDimensionMetricRow],
}

pub struct SnapshotWindow<'a> {
    pub start_date: &'a str,
    pub end_date: &'a str,
    pub daily: &'a [DailyMetricRow],
    pub dimensions: &'a [DimensionMetricRow],
}

pub fn replace_analytics_window(
    conn: &mut Connection,
    client_id: &str,
    property_id: &str,
    window: AnalyticsSnapshotWindow<'_>,
    now_ms: u64,
) -> Result<(), StoreError> {
    let after = serde_json::json!({
        "property_id": property_id,
        "start_date": window.start_date,
        "end_date": window.end_date,
        "daily_rows": window.daily.len(),
        "dimension_rows": window.dimensions.len(),
    })
    .to_string();
    let content_hash = analytics_window_hash(&window);
    let idempotency_key = format!(
        "google_analytics_snapshot:{property_id}:{start_date}:{end_date}:{content_hash}",
        start_date = window.start_date,
        end_date = window.end_date,
    );
    let owned_client = client_id.to_string();
    let owned_property = property_id.to_string();
    let owned_daily = window.daily.to_vec();
    let owned_dimensions = window.dimensions.to_vec();
    let owned_start = window.start_date.to_string();
    let owned_end = window.end_date.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: ANALYTICS_SNAPSHOT_ENTITY_KIND,
            entity_id: property_id,
            change_kind: "sync_window",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM google_analytics_daily_metrics \
                 WHERE client_id = ?1 AND property_id = ?2 AND date >= ?3 AND date <= ?4",
                params![owned_client, owned_property, owned_start, owned_end],
            )?;
            tx.execute(
                "DELETE FROM google_analytics_dimension_metrics \
                 WHERE client_id = ?1 AND property_id = ?2 AND date >= ?3 AND date <= ?4",
                params![owned_client, owned_property, owned_start, owned_end],
            )?;
            for row in &owned_daily {
                tx.execute(
                    "INSERT INTO google_analytics_daily_metrics \
                     (client_id, property_id, date, sessions, total_users, event_count, conversions, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        owned_client,
                        owned_property,
                        row.date,
                        row.metrics.sessions,
                        row.metrics.total_users,
                        row.metrics.event_count,
                        row.metrics.conversions,
                        now_ms as i64,
                    ],
                )?;
            }
            for row in &owned_dimensions {
                tx.execute(
                    "INSERT INTO google_analytics_dimension_metrics \
                     (client_id, property_id, date, dimension_type, dimension_value, \
                      sessions, total_users, event_count, conversions, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        owned_client,
                        owned_property,
                        row.date,
                        row.dimension_type,
                        row.dimension_value,
                        row.metrics.sessions,
                        row.metrics.total_users,
                        row.metrics.event_count,
                        row.metrics.conversions,
                        now_ms as i64,
                    ],
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

fn analytics_window_hash(window: &AnalyticsSnapshotWindow<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(window.start_date.as_bytes());
    hasher.update([0]);
    hasher.update(window.end_date.as_bytes());
    hasher.update([0]);
    for row in window.daily {
        hasher.update(row.date.as_bytes());
        hasher.update([0]);
        hash_analytics_totals(&mut hasher, &row.metrics);
    }
    hasher.update([0xff]);
    for row in window.dimensions {
        hasher.update(row.date.as_bytes());
        hasher.update([0]);
        hasher.update(row.dimension_type.as_bytes());
        hasher.update([0]);
        hasher.update(row.dimension_value.as_bytes());
        hasher.update([0]);
        hash_analytics_totals(&mut hasher, &row.metrics);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hash_analytics_totals(hasher: &mut Sha256, metrics: &AnalyticsMetricTotals) {
    for value in [
        metrics.sessions,
        metrics.total_users,
        metrics.event_count,
        metrics.conversions,
    ] {
        hasher.update(value.to_be_bytes());
    }
}

pub fn replace_window(
    conn: &mut Connection,
    client_id: &str,
    property_url: &str,
    window: SnapshotWindow<'_>,
    now_ms: u64,
) -> Result<(), StoreError> {
    let after = serde_json::json!({
        "property_url": property_url,
        "start_date": window.start_date,
        "end_date": window.end_date,
        "daily_rows": window.daily.len(),
        "dimension_rows": window.dimensions.len(),
    })
    .to_string();
    let idempotency_key = format!(
        "search_console_snapshot:{property_url}:{start_date}:{end_date}:{}:{}",
        window.daily.len(),
        window.dimensions.len(),
        start_date = window.start_date,
        end_date = window.end_date,
    );
    let owned_client = client_id.to_string();
    let owned_property = property_url.to_string();
    let owned_daily = window.daily.to_vec();
    let owned_dimensions = window.dimensions.to_vec();
    let owned_start = window.start_date.to_string();
    let owned_end = window.end_date.to_string();
    store_core::mutate(
        conn,
        MutationRequest {
            client_id,
            entity_kind: SNAPSHOT_ENTITY_KIND,
            entity_id: property_url,
            change_kind: "sync_window",
            actor_id: SYNC_ACTOR,
            actor_kind: ActorKindDto::System,
            expected_revision: None,
            idempotency_key: &idempotency_key,
            correlation_id: None,
            causation_id: None,
            before_json: None,
            after_json: Some(after),
            now_ms,
        },
        move |tx| {
            tx.execute(
                "DELETE FROM search_console_daily_metrics \
                 WHERE client_id = ?1 AND property_url = ?2 AND date >= ?3 AND date <= ?4",
                params![owned_client, owned_property, owned_start, owned_end],
            )?;
            tx.execute(
                "DELETE FROM search_console_dimension_metrics \
                 WHERE client_id = ?1 AND property_url = ?2 AND date >= ?3 AND date <= ?4",
                params![owned_client, owned_property, owned_start, owned_end],
            )?;
            for row in &owned_daily {
                tx.execute(
                    "INSERT INTO search_console_daily_metrics \
                     (client_id, property_url, date, clicks, impressions, ctr_micros, position_micros, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        owned_client,
                        owned_property,
                        row.date,
                        row.metrics.clicks,
                        row.metrics.impressions,
                        row.metrics.ctr_micros,
                        row.metrics.position_micros,
                        now_ms as i64,
                    ],
                )?;
            }
            for row in &owned_dimensions {
                tx.execute(
                    "INSERT INTO search_console_dimension_metrics \
                     (client_id, property_url, date, dimension_type, dimension_value, is_branded, \
                      clicks, impressions, ctr_micros, position_micros, updated_at_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        owned_client,
                        owned_property,
                        row.date,
                        row.dimension_type,
                        row.dimension_value,
                        row.is_branded,
                        row.metrics.clicks,
                        row.metrics.impressions,
                        row.metrics.ctr_micros,
                        row.metrics.position_micros,
                        now_ms as i64,
                    ],
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}
