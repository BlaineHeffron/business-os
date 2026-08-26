//! Work queue slice: THE operator feed. Classified inputs become work items
//! per category policy; the operator accepts (→ packet production, future
//! slice) or dismisses. Surfaces are views over this one feed — no per-page
//! read models (predecessor invariant, enforced from day one here).

pub mod agent_launch;
pub mod routes;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use crate::{
    env_registry,
    slices::{RouteSpec, SliceSpec},
};

pub const SLICE: SliceSpec = SliceSpec {
    id: "work_queue",
    title: "Operator work queue",
    summary: "Per-category packet policy decides which classified inputs become work items; operator accepts or dismisses. Accepted items are the input to packet production (future slice).",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/work-queue",
            summary: "Work items, newest first (?status=open|accepted|dismissed); rows carry the packet kinds with staged drafts awaiting decision",
        },
        RouteSpec {
            method: "POST",
            path: "/api/work-queue/{item_id}/action",
            summary: "Accept, dismiss, reopen, or explicitly move a source email to Gmail Trash",
        },
        RouteSpec {
            method: "POST",
            path: "/api/work-queue/{item_id}/assignment",
            summary: "Assign or unassign a visible work item",
        },
        RouteSpec {
            method: "GET",
            path: "/api/work-queue/{item_id}/source",
            summary: "Full source behind a work item (email or note), for inline review",
        },
        RouteSpec {
            method: "POST",
            path: "/api/work-queue/{item_id}/packet-kinds",
            summary: "Replace the item's suggested packet kinds (operator tunes what gets produced)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/work-queue/{item_id}/produce-guidance",
            summary: "Replace the operator guidance injected into this item's produce-stage LLM requests",
        },
        RouteSpec {
            method: "GET",
            path: "/api/work-queue/packet-kinds",
            summary: "Platform catalog of packet kinds (typed transforms)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/work-queue/{item_id}/launch-agent",
            summary: "Launch a Agent Monitor agent session seeded with this work item's context (operator power tool; gated by BOS_AGENT_LAUNCH_ENABLED)",
        },
        RouteSpec {
            method: "GET",
            path: "/api/work-queue/policies",
            summary: "Per-category work-item policies",
        },
        RouteSpec {
            method: "POST",
            path: "/api/work-queue/policies",
            summary: "Create or update a category's policy",
        },
    ],
    tables: &["work_items", "work_item_visibility", "work_queue_policies"],
    env_vars: &[
        &env_registry::BOS_AGENT_LAUNCH_ENABLED,
        &env_registry::BOS_DEBUG_ENABLED,
        &env_registry::BOS_DEBUG_AGENT_MONITOR_URL,
        &env_registry::BOS_DEBUG_AGENT_MONITOR_TOKEN,
        &env_registry::BOS_AUTO_PRODUCE_ENABLED,
        &env_registry::BOS_AUTO_PRODUCE_INTERVAL_SECS,
        &env_registry::BOS_AUTO_PRODUCE_MAX_PER_CYCLE,
    ],
    read_models: &["work_queue_feed", "work_queue_policies"],
};

pub const SOURCE_KIND_EMAIL: &str = "email";
pub const SOURCE_KIND_OPERATOR_NOTE: &str = "operator_note";
pub const SOURCE_KIND_STOCKFORGE_DAMAGE: &str = "stockforge_damage";

/// The packet-kind catalog: every typed transform the platform can produce.
/// Adding a kind here is a CODE change because each kind must be backed by an
/// output schema + produce implementation + write binding (produce slice).
/// `produce_available` flips to true as each kind is wired end-to-end.
pub fn packet_kind_catalog() -> &'static [bos_contracts::work_queue::PacketKindRecord] {
    use std::sync::OnceLock;
    static CATALOG: OnceLock<Vec<bos_contracts::work_queue::PacketKindRecord>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let kind = |kind_id: &str, title: &str, description: &str, produce_available: bool| {
            bos_contracts::work_queue::PacketKindRecord {
                kind_id: kind_id.to_string(),
                title: title.to_string(),
                description: description.to_string(),
                produce_available,
            }
        };
        vec![
            kind(
                "follow_up_task",
                "Follow-up task",
                "A follow-up task with a due date and context, drafted from the source message. Saved locally — nothing is sent to an external service.",
                true, // follow_up_tasks slice: produce → approve → local task
            ),
            kind(
                "email_draft_reply",
                "Email draft reply",
                "A drafted reply to the source message, staged for your review before a Gmail draft is created. Sending always stays with you.",
                true, // email_drafts slice: produce → approve → gated draft-create
            ),
            kind(
                "calendar_event_draft",
                "Calendar event draft",
                "A calendar event drafted from the source message, staged for your review before anything is added to your calendar.",
                true, // calendar_drafts slice: produce → approve → gated write
            ),
            kind(
                "crm_activity",
                "Log CRM note",
                "A note logging the call or email (who contacted, what they wanted, next steps), staged for your review before anything is added to your CRM. If the contact doesn't yet exist, a record-creation draft is proposed first.",
                true, // crm_drafts slice: produce → approve → gated write
            ),
            kind(
                "crm_record_create",
                "Create CRM records",
                "Creates the company and/or contact records referenced in a note, if they don't already exist in your CRM. Review and approve before any records are added. Currently supports EspoCRM.",
                true, // crm_record_drafts slice: produce → approve → gated ensure-chain write
            ),
            kind(
                "crm_sales_intent",
                "Create CRM lead",
                "Stages pipeline intent separately from CRM contacts and companies. A lead means sales interest or an unqualified opportunity; approval creates an EspoCRM Lead when supported.",
                true, // crm_sales_intent slice: produce → approve → gated lead write
            ),
            kind(
                "ledger_entry",
                "Record received payment",
                "Records a received payment in your accounting system — payer, amount, and date are taken directly from the source email. Staged for your review before anything is recorded.",
                true, // ledger_drafts slice: produce → approve → gated write
            ),
            kind(
                "content_draft",
                "Content draft",
                "A blog post or web page drafted from the brief using your Drive documents as the only source. Every statement must be supported by your documents — unsupported content is blocked before approval. Publishing always stays with you.",
                true, // content_drafts slice: produce → citation gate → approve (no provider write)
            ),
            {
                // The catalog initializes once per process and the provider
                // is process-lifetime config, so the title can name the
                // invoicing system approvals actually write to.
                let invoicing =
                    match crate::slices::accounting::service::configured_accounting_provider()
                        .as_deref()
                    {
                        Ok("invoice_ninja") => Some("Invoice Ninja"),
                        Ok("stripe") => Some("Stripe"),
                        _ => None,
                    };
                kind(
                    "invoice_draft",
                    &invoicing
                        .map(|name| format!("Invoice draft ({name})"))
                        .unwrap_or_else(|| "Invoice draft".to_string()),
                    &format!(
                        "An invoice drafted from a note or email describing billable work. Customer and line items are taken directly from the source — amounts are never invented by AI. Approve to create a draft in {}; sending always stays with you.",
                        invoicing.unwrap_or("your invoicing system"),
                    ),
                    true, // invoice_drafts slice: produce → approve → gated provider draft-invoice
                )
            },
            kind(
                "claim_draft",
                "Shipping damage claim packet",
                "A shipping-damage claim packet assembled from your order evidence (order reference, pack photos, tracking, damage photos). Completeness is checked before approval. Approving creates a Gmail draft for manual carrier or shipping-platform filing, plus a follow-up task.",
                true, // claim_drafts slice: produce → completeness gate → approve → gated Gmail draft
            ),
        ]
    })
}

pub fn packet_kind_exists(kind_id: &str) -> bool {
    packet_kind_catalog().iter().any(|k| k.kind_id == kind_id)
}

/// The slice that backs each catalog kind end-to-end (produce + write). A kind
/// is only meaningful — offerable in the Categories "Produce" picker, emittable
/// by policy, runnable by the produce spine — when its owning slice is enabled
/// for the client. This is the single ownership map for the full catalog;
/// narrower allow-lists should reference this rather than duplicating owners.
const PACKET_KIND_SLICE_OWNERS: &[(&str, &str)] = &[
    ("follow_up_task", crate::slices::follow_up_tasks::SLICE.id),
    ("email_draft_reply", crate::slices::email_drafts::SLICE.id),
    (
        "calendar_event_draft",
        crate::slices::calendar_drafts::SLICE.id,
    ),
    ("crm_activity", crate::slices::crm_drafts::SLICE.id),
    (
        "crm_record_create",
        crate::slices::crm_record_drafts::SLICE.id,
    ),
    (
        "crm_sales_intent",
        crate::slices::crm_sales_intent::SLICE.id,
    ),
    ("ledger_entry", crate::slices::ledger_drafts::SLICE.id),
    ("content_draft", crate::slices::content_drafts::SLICE.id),
    ("invoice_draft", crate::slices::invoice_drafts::SLICE.id),
    ("claim_draft", crate::slices::claim_drafts::SLICE.id),
];

/// Owning slice id for a catalog kind, or `None` if the kind is unknown.
pub fn packet_kind_slice(kind_id: &str) -> Option<&'static str> {
    PACKET_KIND_SLICE_OWNERS
        .iter()
        .find_map(|(candidate, slice_id)| (*candidate == kind_id).then_some(*slice_id))
}

/// The catalog filtered to kinds whose owning slice is enabled for the client.
/// This is what the operator-facing picker is served — a client never sees a
/// produce option (e.g. invoice drafts) for a slice the instance doesn't run.
pub fn packet_kind_catalog_for_enabled<F>(
    slice_enabled: F,
) -> Vec<bos_contracts::work_queue::PacketKindRecord>
where
    F: Fn(&str) -> bool,
{
    packet_kind_catalog()
        .iter()
        .filter(|kind| packet_kind_slice(&kind.kind_id).is_some_and(&slice_enabled))
        .cloned()
        .collect()
}

#[cfg(test)]
mod slice_owner_tests {
    use super::*;

    #[test]
    fn every_catalog_kind_has_a_slice_owner() {
        for kind in packet_kind_catalog() {
            assert!(
                packet_kind_slice(&kind.kind_id).is_some(),
                "catalog kind {} has no slice owner — add it to PACKET_KIND_SLICE_OWNERS",
                kind.kind_id
            );
        }
    }

    #[test]
    fn catalog_is_gated_by_enabled_slices() {
        // Only the email_drafts slice enabled: the picker offers exactly its kind.
        let only_email =
            packet_kind_catalog_for_enabled(|slice| slice == crate::slices::email_drafts::SLICE.id);
        assert_eq!(only_email.len(), 1);
        assert_eq!(only_email[0].kind_id, "email_draft_reply");

        // invoice_drafts disabled (Demo's case): invoice_draft is not offered.
        let no_invoicing = packet_kind_catalog_for_enabled(|slice| {
            slice != crate::slices::invoice_drafts::SLICE.id
        });
        assert!(no_invoicing.iter().all(|k| k.kind_id != "invoice_draft"));
    }
}
