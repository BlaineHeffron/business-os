//! Note → work item emission and the produce-source view over notes.

use bos_contracts::email_triage::InboundMessageRecord;
use bos_contracts::operator_notes::OperatorNote;
use bos_contracts::work_queue::WorkItemStatus;
use rusqlite::Connection;

use crate::store_core::StoreError;

pub const DEFAULT_CATEGORY: &str = "operator_note";
const TITLE_MAX_CHARS: usize = 80;

/// The note action selected by default when the form sends none — log it in
/// the CRM (D2: CRM pre-checked, the others off).
pub fn default_actions() -> Vec<String> {
    vec![crate::slices::crm_drafts::service::PACKET_KIND.to_string()]
}

/// Resolve the form's selected actions: trim, drop blanks, default to the CRM
/// note when empty, order-preserving dedup, and validate every kind against the
/// packet-kind catalog. `Err` carries the wire code for an unknown kind (→ 400
/// at the route).
pub fn resolve_actions(requested: &[String]) -> Result<Vec<String>, &'static str> {
    let mut actions: Vec<String> = Vec::new();
    for raw in requested {
        let action = raw.trim();
        if !action.is_empty() && !actions.iter().any(|a| a == action) {
            actions.push(action.to_string());
        }
    }
    if actions.is_empty() {
        actions = default_actions();
    }
    if actions
        .iter()
        .any(|action| !crate::slices::work_queue::packet_kind_exists(action))
    {
        return Err("operator_note_action_invalid");
    }
    Ok(actions)
}

/// First line (clamped) as the work-item title.
pub fn note_title(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return "Operator note".to_string();
    }
    first_line.chars().take(TITLE_MAX_CHARS).collect()
}

/// Emit the note's work item already ACCEPTED with the operator-selected
/// action kinds (unconditional — the operator logged the note because they
/// want work from it, and accepting your own note is implicit). The selected
/// actions ride as the item's kinds; category policy rows do not override
/// this per-item selection. The caller kicks produce for the item's resulting
/// kinds. Returns false on idempotent replay.
pub fn emit_item_for_note(
    conn: &mut Connection,
    client_id: &str,
    note: &OperatorNote,
    actions: &[String],
    now_ms: u64,
) -> Result<bool, StoreError> {
    crate::slices::work_queue::service::emit_unconditional(
        conn,
        client_id,
        crate::slices::work_queue::service::UnconditionalEmit {
            source_kind: crate::slices::work_queue::SOURCE_KIND_OPERATOR_NOTE,
            source_ref: &note.note_id,
            category_id: &note.category_id,
            title: &note_title(&note.body),
            summary: &note.body,
            default_kinds: actions.to_vec(),
            allow_policy_kinds: false,
            source_user_id: personal_user(&note.created_by),
            status: WorkItemStatus::Accepted,
        },
        now_ms,
    )
}

/// The note author as a source user: a personal identity tags the item;
/// the shared/anonymous "operator" doesn't.
fn personal_user(created_by: &str) -> Option<String> {
    let actor = created_by.strip_prefix("mcp:").unwrap_or(created_by);
    (actor != crate::http::SHARED_OPERATOR_ACTOR).then(|| actor.to_string())
}

/// The produce-source view: a note rendered as the message record the
/// produce kinds consume. No sender address — an email reply draft over a
/// note correctly fails its recipient guard; CRM/follow-up/calendar kinds
/// work from the text alone.
pub fn produce_source_view(note: &OperatorNote) -> InboundMessageRecord {
    InboundMessageRecord {
        source_key: note.note_id.clone(),
        message_id: note.note_id.clone(),
        thread_id: None,
        internal_date_ms: Some(note.created_at_ms as i64),
        from_addr: None,
        to_addr: None,
        subject: Some(note_title(&note.body)),
        body_excerpt: note.body.clone(),
        body_full: note.body.clone(),
        headers: Vec::new(),
        labels: Vec::new(),
        resolved_category: note.category_id.clone(),
        matched_rule_id: None,
        ingested_at_ms: note.created_at_ms,
        ai_triage_status: None,
        ai_triage_rationale: None,
        attachments: Vec::new(),
        source_user_id: personal_user(&note.created_by),
    }
}
