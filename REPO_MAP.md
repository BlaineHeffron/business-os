# REPO_MAP (generated — do not edit; run `just repo-map`)

## Slices

### `accounting` — Accounting cached views

Accounting provider connector (QuickBooks OAuth or a self-hosted provider) feeding request-budgeted incremental sync into local snapshot caches; invoice/aging/financials/customer views serve from sqlite only — the UI never hits the provider API.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/accounting/status` | Provider + connection status; connect_url when QBO is disconnected |
| GET | `/api/connectors/qbo/connect` | Redirect to the Intuit consent screen |
| GET | `/api/connectors/qbo/callback` | OAuth redirect target; stores the realm-bound credential (realmId arrives here) |
| POST | `/api/connectors/qbo/disconnect` | Remove the stored QBO credential; body {purge:true} also deletes every cached snapshot/cursor row |
| POST | `/api/accounting/sync` | Kick one budgeted sync cycle (202; 409 while one runs or during the cooldown) |
| GET | `/api/accounting/invoices` | Cached invoice table (?filter=open|overdue|all) — local cache only, never QBO |
| GET | `/api/accounting/aging` | AR aging buckets over cached open invoices |
| GET | `/api/accounting/financials` | Owner financials from cached P&L reports: weekly/MTD sales and gross margin vs the four-quarter baseline (the pilot payment metric) |
| GET | `/api/accounting/customers` | Cached customer list with tiers (QBO is the tier source of truth) |

Tables: qbo_credentials, accounting_sync_cursors, accounting_invoice_snapshots, accounting_bill_snapshots, accounting_customer_snapshots, accounting_pnl_snapshots, accounting_balance_sheet_snapshots

Read models: accounting_status, accounting_invoices, accounting_aging, accounting_financials, accounting_customers

### `admin_settings` — Admin settings

Full runtime configuration visibility plus curated per-client overrides for resolver-wired behavior switches.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/admin/settings` | Read grouped runtime configuration with secrets redacted |
| POST | `/api/admin/settings/{var_name}` | Set a resolver-wired runtime setting override |
| DELETE | `/api/admin/settings/{var_name}` | Clear a resolver-wired runtime setting override |

Tables: runtime_setting_overrides

Read models: admin_settings

### `agent_mcp` — Agent MCP

Optional MCP endpoint for AgentMonitor/Fleet agents explicitly launched with BusinessOS context. Tools are operator-authenticated and limited to safe reads, note/work-queue artifact creation, and staged draft production; no tool sends email or writes to providers.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/agent-mcp` | Discover the optional BusinessOS MCP server and its safe tool posture |
| POST | `/api/agent-mcp` | Stateless streamable-HTTP MCP endpoint for explicitly BOS-contexted agents |

### `ai_usage` — AI usage log

Per-call usage accounting for typed LLM executions (tokens, latency, cost, outcome) across both the API and harness routes. All LLM call sites flow through this slice's recording seam.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/ai-usage` | Recent usage rows plus all-time and last-24h totals |
| GET | `/api/llm-settings` | Effective typed-LLM routing settings and known task purposes |
| POST | `/api/llm-settings` | Replace global typed-LLM defaults and per-purpose route overrides |
| GET | `/api/llm-settings/claude-subscription` | Read Claude Code subscription availability and connection status |
| POST | `/api/llm-settings/claude-subscription/start` | Start an attended Claude subscription OAuth authorization |
| POST | `/api/llm-settings/claude-subscription/complete` | Submit the one-time Claude authorization code to the waiting CLI |

Tables: ai_usage_log, llm_route_settings, llm_route_overrides

Read models: ai_usage_log, llm_route_settings

### `calendar_drafts` — Calendar event drafts

Produce + approval vertical for calendar_event_draft work items: typed Extract stages a provenance'd event draft; approval enqueues an outbox job delivered through the write-gated Google Calendar client (dry-run while the gate is closed). Owns the outbox delivery pump.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/calendar-drafts` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/calendar-drafts/produce` | Produce a draft from an accepted work item (typed Extract; returns the existing active draft when one exists) |
| POST | `/api/calendar-drafts/{draft_id}/action` | Approve (stages the provider write as an outbox job) or reject a staged draft |
| GET | `/api/calendar-drafts/calendars` | Writable calendars of the connected account (the event-draft calendar picker) |
| POST | `/api/calendar-drafts/{draft_id}/update` | Edit a staged draft's event fields, attendees, invitation choice, and calendar before approval |

Tables: calendar_event_drafts, outbox_jobs

Read models: calendar_drafts

### `call_inputs` — Call inputs

Consent-gated call-log, transcript, and selected recording source inputs. Configured sources require an enabled source with a recorded consent basis before operators can stage inputs; accepted inputs become normal queue items for existing CRM, follow-up, calendar, and email draft flows.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/call-inputs/status` | Configured call input sources and pending consent/fit state |
| GET | `/api/call-inputs/drive-settings` | Configured Google Drive audio intake folder |
| POST | `/api/call-inputs/drive-settings` | Replace the Google Drive audio intake folder setting |
| GET | `/api/call-inputs` | Call inputs newest-first (?status=staged|accepted|rejected) |
| POST | `/api/call-inputs` | Stage one selected call log/transcript/recording reference from an enabled source with a recorded consent basis |
| POST | `/api/call-inputs/{call_input_id}/action` | Accept a staged call input into the work queue or reject it |

Tables: call_inputs, call_input_drive_settings

Read models: call_inputs_status, call_inputs, call_input_drive_settings

### `claim_drafts` — Shipping damage claims

Shipping damage events become queue items (claims pump, request-budgeted, env-gated OFF); produce assembles a deterministic provider-neutral claim packet from local caches (order ref, packing proof, tracking ref, damage photos — completeness gates approval) with one grounded narrative transform; approval stages a gated Gmail draft for manual provider filing plus a claim-tracking follow-up task.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/claim-drafts` | Claim drafts, newest first (?item_id= filters) |
| POST | `/api/claim-drafts/produce` | Produce a claim packet for an accepted damage item (202, panel polls) |
| POST | `/api/claim-drafts/{draft_id}/action` | Approve (packet must be complete; stages the Gmail draft + tracking task) or reject |
| POST | `/api/claim-drafts/{draft_id}/update` | Edit a staged draft's narrative/item/amount (shipment + evidence immutable) |
| POST | `/api/claim-drafts/sync` | Kick one claims sync cycle (202; 409 while syncing/cooling down) |

Tables: stockforge_damage_snapshots, claims_sync_cursors, claim_drafts

Read models: claim_drafts

### `client_profile` — Client Profile

Per-client company background seeded from the overlay and read by outward-facing LLM tasks.

Tables: client_profile

### `content_drafts` — Content drafts

Grounded content drafting over the drive_corpus index: brief → BM25 top-k → evidence budget → one typed drafting transform with mandatory snippet citations → deterministic citation gate → operator approval → separately authorized, gated publishing through a client-specific outbox adapter.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/content-drafts` | Content drafts, newest first (?item_id= filters) |
| POST | `/api/content-drafts/produce` | Produce a grounded draft for an accepted work item (202, panel polls) |
| POST | `/api/content-drafts/{draft_id}/action` | Approve (citation gate must pass; draft-only, no provider write) or reject |
| POST | `/api/content-drafts/{draft_id}/update` | Edit a staged draft's title/body/SEO fields (claims and gate are immutable) |
| POST | `/api/content-drafts/{draft_id}/publish` | Publish an approved draft through the configured client adapter |

Tables: content_drafts, content_web_facts, outbox_jobs

Read models: content_drafts

### `content_plans` — Content plans

Local content plan items with deterministic duplicate/cannibalization warnings, manual published inventory, and a one-transaction handoff into the normal work_queue/content_draft produce spine.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/content-plans/items` | Content plan items with derived draft state |
| POST | `/api/content-plans/items` | Create a planned content item and run advisory overlap checks |
| POST | `/api/content-plans/items/{plan_item_id}/update` | Update a planned content item and rerun advisory overlap checks |
| POST | `/api/content-plans/items/{plan_item_id}/queue` | Queue a planned item as a normal content_draft work item |
| GET | `/api/content-plans/items/{plan_item_id}/campaign` | Unified campaign workspace over the plan, exact article/social revisions, destinations, and publication dependency |
| POST | `/api/content-plans/items/{plan_item_id}/generate` | Operator-accept and generate the grounded article through the normal content_drafts producer |
| POST | `/api/content-plans/items/{plan_item_id}/publish-campaign` | Approve the exact article/social/destination snapshot and enqueue blog-first publication |
| POST | `/api/content-plans/items/{plan_item_id}/check` | Rerun advisory duplicate/cannibalization checks |
| POST | `/api/content-plans/items/{plan_item_id}/mark-published` | Mark a planned or queued item as manually published and add it to inventory |
| GET | `/api/content-plans/inventory` | List local content inventory rows |
| GET | `/api/content-plans/draft-overlap/{draft_id}` | Advisory overlap warnings for a staged content draft |
| POST | `/api/content-plans/inventory` | Add a manual published inventory row |
| POST | `/api/content-plans/inventory/refresh` | Refresh local content inventory from cached local sources |
| POST | `/api/content-plans/inventory/{inventory_id}/archive` | Archive a local content inventory row |

Tables: content_plan_items, content_inventory_items, content_inventory_fts, content_campaign_publications

Read models: content_plan_items, content_inventory_items, content_campaign_workspace

### `crm_cache` — CRM cache

Local cached CRM contacts and HubSpot deals for offline grounding; sync is request-budgeted and the browser reads local data only.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/crm-cache/status` | CRM cache counts and sync freshness |
| GET | `/api/crm-cache/contacts` | Cached CRM contacts by ?email= or ?company= |
| GET | `/api/crm-cache/deals` | Cached CRM deals by ?contact_email= with amount visibility applied |
| GET | `/api/crm-cache/context` | Source-aware cached CRM context for an inbound message |
| POST | `/api/crm-cache/sync` | Kick one CRM cache sync cycle |

Tables: crm_contact_snapshots, crm_deal_snapshots, crm_cache_sync_cursors

Read models: crm_cache_status, crm_cache_contacts, crm_cache_deals, crm_cache_context

### `crm_drafts` — CRM note drafts

Produce + approval vertical for crm_activity work items: typed fill stages a provenance'd CRM note (occurred-at grounded from the source email's date); approval enqueues an outbox job delivered through the write-gated CRM client — BOS_CRM_PROVIDER selects hubspot or espocrm (dry-run while the gate is closed).

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/crm-drafts` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/crm-drafts/produce` | Produce a CRM note draft from an accepted work item (typed fill; returns the existing active draft when one exists) |
| POST | `/api/crm-drafts/{draft_id}/action` | Approve (stages the CRM write as an outbox job for the configured provider) or reject a staged draft |
| POST | `/api/crm-drafts/{draft_id}/update` | Edit a staged draft's AI-filled note fields (body/contact) before approval |

Tables: crm_note_drafts

Read models: crm_drafts

### `crm_record_drafts` — CRM record-create drafts

Produce + approval vertical for crm_record_create work items: a typed fill extracts the company/contacts a note references (names grounded — an invented name is dropped), a bounded LIVE CRM search decides which already exist, and one draft per missing contact proposes ONLY the missing records. Each approval enqueues the create-records outbox job for the configured CRM provider: EspoCRM creates Account then Contact, HubSpot creates Company then Contact with the default association. Writes are idempotent on redelivery and dry-run until their provider write gate is enabled.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/crm-record-drafts` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/crm-record-drafts/produce` | Produce record-create draft(s) from an accepted work item (typed fill + live CRM search; returns one existing active draft when any exists) |
| POST | `/api/crm-record-drafts/{draft_id}/action` | Approve (≥1 record proposed with a name; stages the configured CRM ensure-chain write) or reject |
| POST | `/api/crm-record-drafts/{draft_id}/update` | Edit a staged draft (which records to create + their fields; names re-validated) |
| POST | `/api/crm-record-drafts/{draft_id}/enrich` | Kick off operator-directed web enrichment or gated research mode for a staged record-create draft; returns the enrichment run id immediately |

Tables: crm_record_drafts

Read models: crm_record_drafts

### `crm_sales_intent` — CRM sales-intent drafts

Produce + approval vertical for crm_sales_intent work items: a typed fill stages pipeline intent (lead title, rationale, qualification, next step, optional follow-up date) separately from address-book CRM records. Approval writes an EspoCRM Lead behind the CRM write gate; unsupported providers or targets fail gracefully before mutation.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/crm-sales-intent` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/crm-sales-intent/produce` | Produce a sales-intent draft from an accepted work item |
| POST | `/api/crm-sales-intent/{draft_id}/action` | Approve (stages a provider lead write when supported) or reject |
| POST | `/api/crm-sales-intent/{draft_id}/update` | Edit a staged sales-intent draft before approval |

Tables: crm_sales_intent_drafts

Read models: crm_sales_intent

### `customer_tier_sync` — Customer tier sync

Generic gated/dry-run QBO-to-Shopify customer tier sync: previews read cached QBO customer tiers, operator approval enqueues a Shopify outbox job, and live writes require BOS_SHOPIFY_WRITE_ENABLED. Shopify targets can copy the QBO tier value into a customer metafield/tag, with explicit mapping overrides from config/env.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/customer-tier-sync/runs` | Recent staged/approved customer-tier sync runs with outbox delivery state |
| POST | `/api/customer-tier-sync/preview` | Build a dry-run sync plan from cached QBO customer tiers and configured Shopify targets |
| POST | `/api/customer-tier-sync/runs/{run_id}/approve` | Approve a staged run, enqueueing the gated Shopify customer-tier write outbox job |
| POST | `/api/customer-tier-sync/runs/{run_id}/reject` | Reject a staged sync run without enqueueing provider writes |

Tables: customer_tier_sync_runs, outbox_jobs

Read models: customer_tier_sync_runs

### `data_retention` — Data retention

Bounded, receipted compaction of old email bodies and explicitly allowlisted applied receipt payloads, plus safe SQLite checkpoint/optimize/incremental-vacuum maintenance. Receipt rows and idempotency history are permanent; full VACUUM is never automatic.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/data-retention/status` | Retention policy, eligible rows, SQLite allocation, reusable pages, WAL size, and last-run state |
| POST | `/api/data-retention/run` | Start one idempotent, overlap-guarded retention cycle |

Read models: data_retention_status

### `debug` — Debug

Opt-in operator diagnostics over backend-surfaced errors: panics, failed/conflict receipts, outbox delivery failures, and failed LLM calls.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/debug` | Recent backend diagnostics (404 unless BOS_DEBUG_ENABLED is set) |
| POST | `/api/debug/spawn-agent` | Debug-only local monitor proxy: spawn a Codex agent with diagnostic context |

Tables: panic_diagnostics

Read models: debug_diagnostics

### `drive_corpus` — Drive RAG corpus

Incremental Google Drive readonly sync (request-budgeted, env-gated, changes-API cursor, content-hash skip) into a local FTS5 (BM25) chunk index with deterministic heading-aware chunking. Corpus folders resolve from env pin > operator settings > overlay defaults. Serves corpus status and lexical search; the content_drafts vertical retrieves evidence here.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/drive-corpus/status` | Corpus config, credential/scope state, sync freshness, and index counts |
| POST | `/api/drive-corpus/settings` | Replace the operator-selected Drive folder feeding the RAG corpus |
| POST | `/api/drive-corpus/sync` | Kick one sync cycle (202; 409 while syncing/cooling down/unconfigured) |
| GET | `/api/drive-corpus/search` | BM25 search over the local chunk index (?q=&limit=) |

Tables: drive_doc_snapshots, drive_chunks, drive_chunks_fts, drive_sync_cursors, drive_corpus_settings

Read models: drive_corpus_status, drive_corpus_search

### `email_drafts` — Email reply drafts

Produce/manual-stage + approval vertical for email_draft_reply work items: typed fields remain operator-editable and an optional bounded AI rewrite changes only the body. Approval enqueues an outbox job that creates a Gmail DRAFT (never sends) through the write-gated client (dry-run while the gate is closed).

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/email-drafts` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/email-drafts/manual` | Stage an operator-authored typed email draft for an accepted work item without a model call |
| POST | `/api/email-drafts/produce` | Produce a reply draft from an accepted work item (typed fill; returns the existing active draft when one exists) |
| POST | `/api/email-drafts/{draft_id}/action` | Approve (stages the Gmail draft-create as an outbox job) or reject a staged draft |
| POST | `/api/email-drafts/{draft_id}/update` | Edit a staged draft's recipients, subject, and body before approval |
| POST | `/api/email-drafts/{draft_id}/rewrite` | Rewrite a staged exact-revision email body with the configured bounded typed LLM route |
| GET | `/api/email-drafts/follow-ups` | List outbound email follow-up workflow summaries for task decoration and debug |
| POST | `/api/email-drafts/follow-ups/{follow_up_id}/check` | Manually reconcile a Gmail thread for an outbound follow-up workflow |
| POST | `/api/email-drafts/follow-ups/{follow_up_id}/draft` | Create an accepted email_draft_reply work item for an overdue follow-up |

Tables: email_reply_drafts, email_outbound_follow_ups

Read models: email_drafts

### `email_triage` — Email triage rules

Operator-managed deterministic rules that classify inbound email into input categories; dry-run endpoint for testing rules against sample or live messages.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/email-triage/rules` | List active rules |
| POST | `/api/email-triage/rules` | Create or update a rule (idempotent, revision-checked) |
| POST | `/api/email-triage/rules/{rule_id}/action` | Enable, disable, or delete a rule |
| POST | `/api/email-triage/dry-run` | Classify sample messages against current + proposed rules |
| GET | `/api/email-triage/condition-catalog` | Catalog of supported email triage rule conditions |
| GET | `/api/email-triage/inbox` | Recent ingested + classified inbound messages, optionally filtered by Gmail category, label, mailbox, and limit |
| GET | `/api/email-triage/inbox/options` | Available inbox Gmail categories, labels, mailboxes, and configured dashboard defaults |
| GET | `/api/email-triage/inbox/settings` | Operator-configurable inbox Gmail tab visibility settings |
| POST | `/api/email-triage/inbox/settings` | Replace inbox Gmail tab visibility settings |
| POST | `/api/email-triage/inbox/{message_id}/follow-up` | Manually add a follow-up task packet kind for one inbound email |
| POST | `/api/email-triage/inbox/{message_id}/trash` | Explicitly dismiss local work and enqueue a gated Gmail Move to Trash effect |
| POST | `/api/email-triage/inbox/{message_id}/attachments/{attachment_id}/evidence` | Stage one inbound email attachment into a per-session agent evidence directory |
| POST | `/api/email-triage/reclassify` | Re-run rules over all stored mail + backfill work items |
| POST | `/api/email-triage/ai-retriage-reset` | Clear AI-triage verdicts (per message, stale, or all) so the pump re-examines old mail |
| GET | `/api/email-triage/categories` | Operator-defined input categories (lazy-seeds defaults) |
| POST | `/api/email-triage/categories` | Create or update a category |
| POST | `/api/email-triage/categories/{category_id}/delete` | Delete a category (refused while rules pin it) |

Tables: email_triage_rules, email_inbound_messages, email_inbound_enrichments, email_triage_categories, email_triage_fact_cache, email_triage_inbox_settings, agent_evidence_files, gmail_ingest_cursors

Read models: email_triage_rules_list, email_triage_inbox, email_triage_inbox_options, email_triage_inbox_settings, email_triage_categories

### `enrichment` — Enrichment diagnostics

Shared field-scoped enrichment waterfall diagnostics: draft slices write durable tier events and proposals through store_core; operators can inspect recent runs by draft or item.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/enrichment/runs` | Recent enrichment runs (?slice_id=&draft_id= or ?item_id= filters) |

Tables: enrichment_run

Read models: enrichment_runs

### `follow_up_tasks` — Follow-up tasks

Produce/manual-stage + approval vertical for follow_up_task work items: typed fields are validated at one chokepoint; optional AI produce stages a provenance'd draft. Approval writes the local tasks row in the same receipted transaction. Serves the operator task list (complete/reopen).

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/follow-up-drafts` | Drafts newest-first (?item_id= scopes to one work item) |
| POST | `/api/follow-up-drafts/manual` | Stage an operator-authored typed follow-up draft for an accepted work item without a model call |
| POST | `/api/follow-up-drafts/produce` | Produce a draft from an accepted work item (typed fill; returns the existing active draft when one exists) |
| POST | `/api/follow-up-drafts/{draft_id}/action` | Approve (creates the local task in the same transaction) or reject a staged draft |
| POST | `/api/follow-up-drafts/{draft_id}/update` | Edit a staged draft's AI-filled task fields (title/due date/context) before approval |
| GET | `/api/tasks` | Operator task list, open-first by due date (?status=open|done; ?today=YYYY-MM-DD decorates open tasks with watchdog escalation lanes: overdue/due-today/upcoming, missed->escalated->critical) |
| POST | `/api/tasks/{task_id}/action` | Complete or reopen a task |

Tables: follow_up_task_drafts, tasks

Read models: follow_up_drafts, tasks

### `google_connector` — Google account connector

OAuth connect flow for Google services, per operator user: consent URL bound to the connecting user, code-exchange callback, stored refresh tokens (audited, never in receipts), per-user status + disconnect.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/connectors/google/status` | Connection status; includes connect_url when disconnected |
| GET | `/api/connectors/google/connect` | Redirect to the Google consent screen |
| GET | `/api/connectors/google/callback` | OAuth redirect target; exchanges the code and stores the refresh token |
| POST | `/api/connectors/google/disconnect` | Remove the stored credential |
| GET | `/api/connectors/google/drive/folders` | Search Google Drive folders available to the connected credential |

Tables: google_oauth_credentials, connector_oauth_states

Read models: google_connector_status

### `home_dashboard` — Home dashboard

Configurable per-operator landing dashboard assembled from existing authorized read models: tasks, inbox/work queue, inventory, and financial widgets.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/home-dashboard` | Current operator's dashboard preferences and authorized widget data |
| POST | `/api/home-dashboard/preferences` | Replace the current operator's dashboard widget order and visibility |
| GET | `/api/home-dashboard/hubspot-deals/discovery` | Read-only HubSpot deal pipelines, stages, and date properties for dashboard setup |
| GET | `/api/home-dashboard/hubspot-deals/mapping` | Current saved HubSpot deal pipeline mapping for the Home sales widget |
| POST | `/api/home-dashboard/hubspot-deals/mapping` | Save the HubSpot deal pipeline mapping used by the Home sales widget |

Tables: home_dashboard_preferences, home_dashboard_hubspot_deal_mapping

Read models: home_dashboard

### `instance_diagnostics` — Instance diagnostics

Structured health for cross-instance support monitoring: identity, pump guard states, and error rollups computed from receipts, outbox_jobs, and ai_usage_log. Read-only; the support hub (agent-monitor) polls these endpoints.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/readyz` | Unauthenticated structured liveness (mounted core, serves even when the slice is disabled) |
| GET | `/api/diagnostics/health` | Operator-gated health: identity, pump statuses, outbox backlog, windowed error rollups, enabled slices |

### `inventory` — Inventory cached views (Stockforge)

Read-only Stockforge connector: env-configured org API key (VIEWER role, sfk_live_…), a request-budgeted sync pump into local snapshot caches (webhook events kick it early), and stock/alert/order-board/PO views served from sqlite only — the UI never hits the Stockforge API.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/connectors/stockforge/status` | Connector status (configured, base URL, has synced); blocked_reason when env is missing |
| POST | `/api/inventory/sync` | Kick one budgeted sync cycle (202; 409 while one runs or during the cooldown) |
| GET | `/api/inventory/stock` | Cached stock on hand with low-stock classification + KPI rollup — local cache only, never Stockforge |
| GET | `/api/inventory/alerts` | Active low-stock alerts + pending reorder suggestions (burn-rate / lead-time aware) |
| GET | `/api/inventory/orders` | Order-board summary over the cached 30-day window: pipeline counts, exceptions, order-control gaps, attention-first cards |
| GET | `/api/inventory/purchase-orders` | Open purchase orders (inbound stock) from the cached snapshot |
| POST | `/api/webhooks/stockforge` | Inbound Stockforge webhook (HMAC-verified, replay-bounded); a verified event kicks one guarded sync cycle — payloads are never trusted as data |

Tables: stockforge_sync_cursors, stockforge_material_snapshots, stockforge_alert_snapshots, stockforge_reorder_snapshots, stockforge_order_snapshots, stockforge_po_snapshots

Read models: stockforge_connector_status, inventory_stock, inventory_alerts, inventory_orders, inventory_purchase_orders

### `invoice_drafts` — Invoice drafts

Produce + approval vertical for invoice_draft work items (suggested from notes/emails describing billable work): typed fill stages an invoice draft — customer, line items with provenance-grounded amounts (totals recomputed server-side, never model math); approval (requires a customer email) enqueues the create-invoice-draft outbox job for the configured provider — Invoice Ninja (find-or-create client, DRAFT invoice by unique number, dry-run until BOS_INVOICE_NINJA_WRITE_ENABLED) or Stripe (find-or-create customer, invoice with auto_advance=false, dry-run until BOS_STRIPE_WRITE_ENABLED). Either way the invoice stays a provider DRAFT — review and send stay human in the provider's UI.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/invoice-drafts` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/invoice-drafts/produce` | Produce an invoice draft from an accepted work item (typed fill; returns the existing active draft when one exists) |
| POST | `/api/invoice-drafts/{draft_id}/action` | Approve (requires customer email + non-zero total; stages the provider draft-invoice write) or reject |
| POST | `/api/invoice-drafts/{draft_id}/update` | Edit a staged draft (customer/email/due date/memo/line items; totals recomputed server-side) |
| POST | `/api/invoice-drafts/{draft_id}/enrich` | Kick off operator-directed customer web enrichment for a staged invoice draft; returns the enrichment run id immediately |
| GET | `/api/invoice-drafts/settings` | Invoicing defaults (default due-date term) |
| POST | `/api/invoice-drafts/settings` | Update invoicing defaults — default due-date Net N, applied at produce when the source states no explicit date or term (1..=365 days; revision-checked) |

Tables: invoice_drafts, invoice_settings

Read models: invoice_drafts, invoice_settings

### `lead_discovery` — Lead discovery

Approved-source lead discovery workflow: sources are client-overlay configured, findings are staged for human review with provenance, and accepted findings become normal queue work. No broad scraping or outreach.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/lead-discovery/status` | Configured approved sources, criteria, and pending/disabled state |
| GET | `/api/lead-discovery/findings` | Lead findings newest-first (?status=staged|accepted|rejected) |
| POST | `/api/lead-discovery/findings` | Stage one finding from an approved configured source; provenance is required |
| POST | `/api/lead-discovery/findings/{finding_id}/action` | Accept a finding into the work queue or reject it |

Tables: lead_findings

Read models: lead_discovery_status, lead_findings

### `ledger_drafts` — Ledger entry drafts

Produce + approval vertical for ledger_entry work items: typed fill stages a received-payment draft (payer/amount/date grounded with literal provenance — money is never invented); approval enqueues the provider write as an outbox job — Invoice Ninja record_receipt (client + invoice + applied payment) or QBO record_payment (applied to the snapshot invoice whose open balance matches the amount) — dry-run while the provider's write gate is closed.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/ledger-drafts` | Drafts newest-first (?item_id= scopes to one work item); includes outbox delivery state |
| POST | `/api/ledger-drafts/produce` | Produce a receipt draft from an accepted work item (typed fill; returns the existing active draft when one exists) |
| POST | `/api/ledger-drafts/{draft_id}/action` | Approve (stages the accounting write as an outbox job; requires a writable provider) or reject a staged draft |
| POST | `/api/ledger-drafts/{draft_id}/update` | Edit a staged draft's AI-filled receipt fields (payer/amount/date/description) before approval |

Tables: ledger_entry_drafts

Read models: ledger_drafts

### `operator_notes` — Operator notes

Manually logged notes as a work-item source: creating a note emits a work item (category operator_note; policy supplies packet kinds, defaulting to CRM note + follow-up task), and produce kinds run over the note text.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/operator-notes` | Recent notes, newest first |
| POST | `/api/operator-notes` | Log a note; emits its work item in the same request (idempotent on the key) |

Tables: operator_notes

Read models: operator_notes

### `operator_users` — Operator users

Named operators with personal bearer tokens: authentication resolves WHO acts (receipts stamp the user id), enabling per-user approvals and, next, per-user provider credentials.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/me` | Who the presented token authenticates as |
| GET | `/api/users` | List operator users |
| POST | `/api/users` | Create an operator user (returns the personal token ONCE) |
| POST | `/api/users/{user_id}/action` | Enable, disable, or archive a user (disable/archive invalidate the token immediately) |
| POST | `/api/users/{user_id}/rotate-token` | Replace the user's token (returned ONCE; the old token stops working) |
| POST | `/api/users/{user_id}/default-calendar` | Set or clear the calendar the user's approved event drafts default to |

Tables: operator_users

Read models: operator_users

### `owner_reports` — Owner reporting digest

Deterministic weekly + month-to-date owner digest assembled from cached operational data plus read-only HubSpot deal reporting when configured; generation is env-gated, scheduled delivery is separately gated/configured by overlay/env (recipients, weekly weekday, MTD day, metric ordering), and optional email delivery stages a gated Gmail draft. Calls are configurable email-derived metrics; site traffic remains a pending data-source decision.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/owner-reports` | Digest reports, newest period first (?period=weekly|mtd filters) |
| POST | `/api/owner-reports/generate` | Regenerate the current weekly + MTD digests now (202; 409 while generating/cooling down) |
| POST | `/api/owner-reports/{report_id}/email` | Stage the digest as a gated Gmail draft to configured owner-report recipients (422 when unset) |

Tables: owner_reports

Read models: owner_reports

### `packet_proposals` — Packet proposals

Smart draft runs one bounded typed AI proposal over a normalized source, records the run, accepts the queue item, and stages existing packet-kind drafts through their normal gates.

| Method | Path | Summary |
| --- | --- | --- |
| POST | `/api/packet-proposals/smart-draft` | Create or accept a work item for a source and stage Smart draft packet proposals |
| POST | `/api/packet-proposals/smart-draft/source-state` | Read Smart draft source state and current queue item revision |

Tables: packet_proposal_runs, packet_proposal_run_evidence

Read models: packet_proposal_runs

### `quote_workflows` — Quote workflows

Bounded quote-builder workflow with slice-local Trace persistence: start a run, inspect its causal trace, and approve or reject the staged quote draft. Approval enqueues the provider-write outbox job with the workflow run id as correlation id.

| Method | Path | Summary |
| --- | --- | --- |
| POST | `/api/quote-workflows/run` | Start the quote_builder.v1 workflow and stage a quote draft when grounding and policy checks pass |
| GET | `/api/quote-workflows/{run_id}` | Inspect a workflow run with steps, by-correlation receipts, outbox jobs, and staged quote draft |
| POST | `/api/quote-drafts/{draft_id}/action` | Approve (enqueue quote provider draft) or reject a staged quote draft |

Tables: workflow_runs, workflow_steps, quote_drafts, outbox_jobs

Read models: quote_workflow_inspection

### `release_notes` — Release notes

Operator-facing deployment notes created by the fleet and dismissed per operator.

| Method | Path | Summary |
| --- | --- | --- |
| POST | `/api/webhooks/release-notes` | Create a release note from the fleet; webhook-token gated and idempotent by release note id |
| GET | `/api/release-notes/latest` | Latest release note not dismissed by this operator |
| GET | `/api/release-notes` | Recent release notes |
| POST | `/api/release-notes/{id}/dismiss` | Dismiss a release note for this operator |

Tables: release_notes, release_note_dismissals

Read models: release_notes

### `search_console` — Search Console traffic

Read-only Google Search Console and GA4 sync for the configured properties. Stores local traffic snapshots and serves status, sync-now, and cached traffic overview with branded/non-branded, top query, top landing-page, and source cuts.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/search-console/status` | Configured property, credential/scope state, sync freshness, and cached weekly/MTD traffic |
| POST | `/api/search-console/sync` | Kick one budgeted Search Console discovery/sync cycle (202; 409 while syncing/cooling down) |
| POST | `/api/google-analytics/sync` | Kick one budgeted GA4 sync cycle (202; 409 while syncing/cooling down/unconfigured) |
| POST | `/api/search-console/property` | Select one discovered Search Console property for cached reporting when no env/overlay property overrides it |

Tables: search_console_sync_cursors, search_console_properties, search_console_property_selection, search_console_daily_metrics, search_console_dimension_metrics, google_analytics_sync_cursors, google_analytics_daily_metrics, google_analytics_dimension_metrics

Read models: search_console_status

### `shopify_sales` — Shopify sales cached views

Read-only Shopify connector: env-configured Admin API token, a request-budgeted sync pump into local order/customer snapshot caches, and sales views served from sqlite only — the UI and grounding tools never hit Shopify directly.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/shopify-sales/status` | Connector status (configured, shop domain, has synced); blocked_reason when env is missing |
| POST | `/api/shopify-sales/sync` | Kick one budgeted sync cycle (202; 409 while one runs or during the cooldown) |
| GET | `/api/shopify-sales/orders` | Cached recent orders or ?email= customer order history; dollar fields redact for limited operators |
| GET | `/api/shopify-sales/customers` | Cached customer lookup by email; dollar fields redact for limited operators |

Tables: shopify_order_snapshots, shopify_customer_snapshots, shopify_sales_sync_state

Read models: shopify_sales_status, shopify_sales_orders, shopify_sales_customers

### `social_publishing` — Social publishing

Published-content ingress and a bounded typed transform produce editable platform-specific proposals. Operator approval snapshots the exact current revision and atomically enqueues one independently retryable Buffer job per channel; live writes default off.

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/social-publishing/proposals` | Published-source generation state, recent proposals, configured Buffer channels, and per-channel delivery state |
| POST | `/api/social-publishing/proposals` | Stage one editable proposal covering every configured Buffer channel |
| POST | `/api/social-publishing/proposals/{proposal_id}/update` | Replace a staged proposal's exact channel text, image, UTM, and schedule snapshot |
| POST | `/api/social-publishing/proposals/{proposal_id}/action` | Approve the exact current revision and fan out channel jobs atomically, or reject |
| POST | `/api/social-publishing/sources/{source_id}/generate` | Kick off one bounded typed transform that drafts grounded per-channel proposals from published content |
| POST | `/api/social-publishing/drafts/{draft_id}/generate-preview` | Draft grounded social variants from an exact editable article revision and operator-previewed canonical URL |

Tables: social_published_sources, social_post_proposals, outbox_jobs

Read models: social_published_sources, social_post_proposals

### `work_queue` — Operator work queue

Per-category packet policy decides which classified inputs become work items; operator accepts or dismisses. Accepted items are the input to packet production (future slice).

| Method | Path | Summary |
| --- | --- | --- |
| GET | `/api/work-queue` | Work items, newest first (?status=open|accepted|dismissed); rows carry the packet kinds with staged drafts awaiting decision |
| POST | `/api/work-queue/{item_id}/action` | Accept, dismiss, reopen, or explicitly move a source email to Gmail Trash |
| POST | `/api/work-queue/{item_id}/assignment` | Assign or unassign a visible work item |
| GET | `/api/work-queue/{item_id}/source` | Full source behind a work item (email or note), for inline review |
| POST | `/api/work-queue/{item_id}/packet-kinds` | Replace the item's suggested packet kinds (operator tunes what gets produced) |
| POST | `/api/work-queue/{item_id}/produce-guidance` | Replace the operator guidance injected into this item's produce-stage LLM requests |
| GET | `/api/work-queue/packet-kinds` | Platform catalog of packet kinds (typed transforms) |
| POST | `/api/work-queue/{item_id}/launch-agent` | Launch a Agent Monitor agent session seeded with this work item's context (operator power tool; gated by BOS_AGENT_LAUNCH_ENABLED) |
| GET | `/api/work-queue/policies` | Per-category work-item policies |
| POST | `/api/work-queue/policies` | Create or update a category's policy |

Tables: work_items, work_item_visibility, work_queue_policies

Read models: work_queue_feed, work_queue_policies


## Environment variables

| Variable | Default | Description |
| --- | --- | --- |
| `BOS_ACCOUNTING_MAX_REQUESTS_PER_CYCLE` | `8` | Hard cap on accounting-provider API requests per sync cycle (rate-limit budget; QBO allows ~500/min per realm, we stay far below). |
| `BOS_ACCOUNTING_METRIC_ADJUSTED_FREIGHT_CENTS` | — | Imported current-period freight deduction, in cents, for BOS_ACCOUNTING_METRIC_BASIS=adjusted_gross_sales. Unset keeps the adjusted metric pending/limited. |
| `BOS_ACCOUNTING_METRIC_ADJUSTED_INSURANCE_CENTS` | — | Imported current-period insurance deduction, in cents, for BOS_ACCOUNTING_METRIC_BASIS=adjusted_gross_sales. Unset keeps the adjusted metric pending/limited. |
| `BOS_ACCOUNTING_METRIC_ADJUSTED_TAXES_CENTS` | — | Imported current-period tax deduction, in cents, for BOS_ACCOUNTING_METRIC_BASIS=adjusted_gross_sales. Unset keeps the adjusted metric pending/limited. |
| `BOS_ACCOUNTING_METRIC_BASELINE_CENTS` | — | Imported monthly baseline value, in cents, for the configured accounting management metric. Used when automated provider baseline extraction is unavailable. |
| `BOS_ACCOUNTING_METRIC_BASIS` | — | Accounting management metric basis: gross_margin | adjusted_gross_sales | invoice_totals. Overrides overlay [accounting.metric_basis].basis. |
| `BOS_ACCOUNTING_METRIC_LABEL` | — | Operator-facing label for the configured accounting management metric. Overrides overlay [accounting.metric_basis].label. |
| `BOS_ACCOUNTING_PROVIDER` | `qbo` | Accounting provider behind the Accounting views: qbo | invoice_ninja | stripe. |
| `BOS_ACCOUNTING_SYNC_ENABLED` | — | Run the accounting sync pump (incremental, request-budgeted). Off by default; the manual Sync-now route works regardless. |
| `BOS_ACCOUNTING_SYNC_INTERVAL_SECS` | `1800` | Seconds between accounting sync pump cycles (min 300 — accounting data rarely needs to be fresher). |
| `BOS_ACCOUNTING_VISIBILITY_POLICY` | — | Internal BusinessOS accounting visibility policy. Allowed modes: shared, admin_only, or authorizer_only; empty/unset uses the overlay [accounting].visibility_policy, whose default is authorizer_only. QBO OAuth scopes remain provider-wide; this controls only who can see BusinessOS accounting views. |
| `BOS_AGENTIC_WEB_RESEARCH_COST_BUDGET_MICROS` | `0` | Maximum model spend budget for one bounded agentic web research run. |
| `BOS_AGENTIC_WEB_RESEARCH_ENABLED` | — | Enable the optional bounded agentic web research enrichment tier. Off by default. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_CONCURRENT_RUNS` | `1` | Maximum concurrent bounded agentic web research runs. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_FETCHED_PAGES` | `4` | Maximum pages fetched by one bounded agentic web research run. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_OUTPUT_TOKENS` | `4096` | Maximum model output tokens for one bounded agentic web research action. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_PAGE_BYTES` | `524288` | Maximum bytes read from one page during bounded agentic web research. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_RESULTS` | `10` | Maximum search results considered by one bounded agentic web research run. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_SEARCHES` | `2` | Maximum search actions in one bounded agentic web research run. |
| `BOS_AGENTIC_WEB_RESEARCH_MAX_STEPS` | `8` | Maximum model/action steps in one bounded agentic web research run. |
| `BOS_AGENTIC_WEB_RESEARCH_TIMEOUT_MS` | `90000` | Wall-clock timeout for one bounded agentic web research run. |
| `BOS_AGENT_EVIDENCE_CLEANUP_ENABLED` | `1` | Enable periodic cleanup of expired per-session agent evidence files staged from provider attachments. |
| `BOS_AGENT_EVIDENCE_CLEANUP_INTERVAL_SECS` | `3600` | Interval between expired agent evidence file cleanup passes. |
| `BOS_AGENT_EVIDENCE_MAX_BYTES` | `10485760` | Maximum bytes BusinessOS will stage for one email attachment evidence file. |
| `BOS_AGENT_EVIDENCE_RETENTION_DAYS` | `30` | Default retention window for staged agent evidence files. |
| `BOS_AGENT_EVIDENCE_ROOT_DIR` | `var/agent-evidence` | Filesystem root for per-session agent evidence files staged from provider attachments. |
| `BOS_AGENT_LAUNCH_ENABLED` | — | Enable launching a Agent Monitor agent session from a work item (with the item's context plus optional operator notes). Reuses BOS_DEBUG_AGENT_MONITOR_URL/_TOKEN. Off by default; intended for the operator's own dashboard, not client instances. |
| `BOS_AGENT_MCP_ENABLED` | — | Enable the optional BusinessOS MCP endpoint for explicitly BOS-contexted AgentMonitor/Fleet agents. Off by default; tools remain operator-authenticated and cannot approve drafts, send email, or write to providers. |
| `BOS_AI_TRIAGE_ENABLED` | — | Enable the tier-2 AI triage pass over fallback (rule-less) mail. Off by default. |
| `BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE` | `5` | Max LLM triage calls per ingest cycle (cost bound). |
| `BOS_AI_TRIAGE_MIN_CONFIDENCE` | `high` | Minimum confidence (high|medium|low) before an AI suggestion becomes a work item; below it the message stays quiet. |
| `BOS_AI_TRIAGE_PACKET_PROPOSALS_ENABLED` | — | When enabled, the tier-2 AI triage pass uses the unified packet proposal call to suggest and stage drafts in one LLM response. Off by default. |
| `BOS_AUTO_PRODUCE_ENABLED` | — | Run the auto-produce pump: accepted items in categories whose policy has auto_produce on get their drafts produced automatically (LLM cost per accept). Off by default. |
| `BOS_AUTO_PRODUCE_INTERVAL_SECS` | `30` | Seconds between auto-produce pump cycles. |
| `BOS_AUTO_PRODUCE_MAX_PER_CYCLE` | `3` | Max LLM produce calls per auto-produce cycle (cost bound). |
| `BOS_BUFFER_ACCESS_TOKEN` | — | Buffer API key used only by the approval-gated outbox delivery adapter. Never included in proposal, model, receipt, or outbox payloads. |
| `BOS_BUFFER_API_URL` | `https://api.buffer.com` | Buffer GraphQL API endpoint for approved social-post delivery. |
| `BOS_BUFFER_CHANNELS_JSON` | — | Configured Buffer targets as JSON array entries {channel_id,name,platform}. Supported platform keys: facebook, googlebusiness, instagram, linkedin, twitter. Every staged proposal must cover the exact configured channel set. |
| `BOS_BUFFER_WRITE_ENABLED` | — | Enable approved social proposals to create Buffer posts. Off by default; closed-gate channel jobs dry-run independently. |
| `BOS_BUILD_SHA` | — | Git sha of the deployed build, stamped into the image by CI (publish-image.yml). Surfaced in /api/diagnostics/health so the support hub can verify which build a client runs. Unset = local/unstamped build. |
| `BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED` | — | Enable the local call-input audio transcription pump. Off by default; also requires a configured source with consent_basis, intake dir, Whisper binary, and model. |
| `BOS_CALL_INPUTS_MAX_AUDIO_BYTES` | `52428800` | Maximum raw audio file size accepted by the local call-input transcription pump. |
| `BOS_CALL_INPUTS_SYNC_ENABLED` | — | Enable the call-input source sync pump. Off by default; raw-audio transcription also requires BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED. |
| `BOS_CALL_INPUTS_SYNC_INTERVAL_SECS` | `300` | Seconds between call-input source sync pump cycles. |
| `BOS_CALL_INPUTS_TRANSCRIPTION_INTAKE_DIR` | — | Local directory watched by the call-input transcription pump for approved raw audio files. Files are staged through the configured call_inputs source; raw audio is not archived by BusinessOS. |
| `BOS_CALL_INPUTS_TRANSCRIPTION_MAX_CONCURRENCY` | `1` | Maximum concurrent local call-input transcription jobs. Defaults to 1 for small single-CPU deployments. |
| `BOS_CALL_INPUTS_TRANSCRIPTION_TIMEOUT_MS` | `300000` | Wall-clock timeout for one local Whisper call-input transcription job. |
| `BOS_CALL_INPUTS_TRANSCRIPTION_TMP_DIR` | `var/call-inputs-transcription` | Temporary directory root for per-job local Whisper transcription work. Per-job temp dirs are cleaned on every exit path. |
| `BOS_CALL_INPUTS_WHISPER_BIN` | — | Path to the local whisper.cpp executable used for call-input raw-audio transcription. |
| `BOS_CALL_INPUTS_WHISPER_MODEL` | — | Path or model id for the local whisper.cpp model used for call-input raw-audio transcription; base.en is the default deployment target for 1 CPU / 1 GB. |
| `BOS_CLAIMS_MAX_REQUESTS_PER_CYCLE` | `5` | Hard cap on Stockforge API requests per claims sync cycle (damage list + pack-photo fetches). |
| `BOS_CLAIMS_SYNC_ENABLED` | — | Run the shipping-damage claims pump (polls Stockforge OPEN damage events into the work queue). Off by default. |
| `BOS_CLAIMS_SYNC_INTERVAL_SECS` | `1800` | Seconds between claims pump cycles (min 300 — damage reports are not minute-urgent). |
| `BOS_CLAIM_DRAFT_TO_ADDR` | — | Recipient of approved shipping-damage packet Gmail drafts (the mailbox that handles carrier/platform filing — e.g. the owner's own address). Unset = claim approval refuses with claim_draft_to_addr_unset. |
| `BOS_CLIENT_ID` | `dev` | Client identifier stamped on every receipt and row. Default for local dev only. |
| `BOS_CLIENT_OVERLAY_DIR` | — | Client overlay directory (client.toml etc). Unset = built-in dev profile. |
| `BOS_CONTENT_PUBLISH_ADAPTER_TOKEN` | — | Bearer token used only for authenticated calls to the client-specific content publisher adapter. |
| `BOS_CONTENT_PUBLISH_ADAPTER_URL` | — | Client-specific HTTP adapter that accepts approved content publish jobs. Unset means direct publishing is unavailable. |
| `BOS_CONTENT_PUBLISH_WRITE_ENABLED` | — | Enable approved content drafts to be written through the configured publisher adapter. Off by default; closed-gate jobs dry-run. |
| `BOS_CONTENT_WEB_FACTS_ENABLED` | — | Opt-in (off by default) for content-draft web-fact enrichment on briefs that literally name a domain. Nested under the BOS_WEB_ENRICHMENT_ENABLED kill-switch — both must be on: this flag enables the feature, the global switch must also permit the web read. |
| `BOS_CRM_CONTEXT_NEUTRAL_SENDER_DOMAINS` | `amazonses.com,hubspotemail.net,intuit.com,mailchimp.com,mandrillapp.com,myshopify.com,paypal.com,quickbooks.intuit.com,sendgrid.net,shopify.com,shopifyemail.com,squareup.com,stripe.com,zendesk.com` | Comma-separated domain roots whose automated/platform sender addresses must not be treated as CRM contacts for inbox context or email-triage CRM facts. Subdomains match on dot boundaries. |
| `BOS_CRM_DEAL_VISIBILITY_POLICY` | `authorizer_only` | Controls visibility of cached CRM deal amounts. Allowed modes: shared, admin_only, or authorizer_only; empty/unset defaults to authorizer_only. |
| `BOS_CRM_PROVIDER` | `hubspot` | CRM provider receiving approved crm_activity notes: hubspot | espocrm. |
| `BOS_CRM_READ_MAX_REQUESTS_PER_CYCLE` | `8` | Hard cap on CRM API requests per cache sync cycle. |
| `BOS_CRM_READ_SYNC_ENABLED` | — | Run the CRM cache sync. Off by default; manual sync still works. |
| `BOS_CRM_READ_SYNC_INTERVAL_SECS` | `1800` | Seconds between CRM cache sync cycles (minimum 300). |
| `BOS_DATA_RETENTION_BATCH_SIZE` | `200` | Maximum email or receipt rows compacted in one receipted transaction. |
| `BOS_DATA_RETENTION_EMAIL_BODY_DAYS` | `90` | Days to retain full plain-text and HTML email bodies; excerpts and metadata remain permanent. |
| `BOS_DATA_RETENTION_ENABLED` | `1` | Enable automatic bounded SQLite retention and storage maintenance. |
| `BOS_DATA_RETENTION_INCREMENTAL_VACUUM_PAGES` | `256` | Maximum freelist pages requested from incremental_vacuum after each retention cycle; zero disables it. |
| `BOS_DATA_RETENTION_INTERVAL_SECS` | `21600` | Seconds between automatic retention cycles; runtime clamps this to at least 900 seconds. |
| `BOS_DATA_RETENTION_MAX_ROWS_PER_CYCLE` | `5000` | Maximum total email and receipt rows compacted in one retention cycle. |
| `BOS_DATA_RETENTION_RECEIPT_PAYLOAD_DAYS` | `90` | Days to retain before/after JSON on applied receipts for the explicit provider-mirror allowlist; receipt rows and idempotency fields remain permanent. |
| `BOS_DEBUG_AGENT_MONITOR_TOKEN` | — | Bearer token used when posting to the local Agent Monitor /api/agents/sessions endpoint (the Debug spawn-agent action and the work-item launch-agent action share it). Unset = no Authorization header. |
| `BOS_DEBUG_AGENT_MONITOR_URL` | — | Base URL for a local Agent Monitor instance. When set, the Debug surface (with BOS_DEBUG_ENABLED) can spawn a Codex agent with diagnostic context, and the work-item launch-agent action (with BOS_AGENT_LAUNCH_ENABLED) can spawn one with work-item context. |
| `BOS_DEBUG_ENABLED` | — | Enable the operator Debug surface. Default off for production overlays; dev/all-slices can still enable via this flag. |
| `BOS_DRIVE_CORPUS_EXCLUDE_FILE_IDS` | — | Comma/space-separated Drive file ids excluded from the RAG corpus. Overrides the overlay [drive_corpus] exclude_file_ids when set. |
| `BOS_DRIVE_CORPUS_EXCLUDE_NAME_PATTERNS` | — | Comma-separated case-insensitive file-name patterns (`*` wildcard) excluded from the RAG corpus. Overrides the overlay value when set. |
| `BOS_DRIVE_CORPUS_FOLDER_IDS` | — | Comma/space-separated Google Drive folder ids whose DIRECT children form the RAG corpus. Overrides the overlay [drive_corpus] folder_ids when set; both unset = corpus unconfigured and the sync pump waits quietly. |
| `BOS_DRIVE_CORPUS_INCLUDE_FILE_IDS` | — | Comma/space-separated Drive file ids included in the RAG corpus regardless of folder. Overrides the overlay value when set. |
| `BOS_DRIVE_CORPUS_USER_ID` | — | Operator user whose Google credential the Drive corpus sync reads with (needs drive.readonly — reconnect Google after the scope joined the consent list). Unset = the only stored credential (single-account mode). |
| `BOS_DRIVE_MAX_REQUESTS_PER_CYCLE` | `8` | Hard cap on Google Drive API requests per sync cycle (listing pages and document reads each cost one). |
| `BOS_DRIVE_SYNC_ENABLED` | — | Run the Drive corpus sync pump (incremental, request-budgeted). Off by default; the manual Sync-now route works regardless. |
| `BOS_DRIVE_SYNC_INTERVAL_SECS` | `1800` | Seconds between Drive corpus sync pump cycles (min 300 — reference docs rarely need to be fresher). |
| `BOS_EMAIL_ENRICHMENT_BACKFILL_BATCH` | `200` | Maximum stored inbound messages the email enrichment backfill will reprocess per cycle. |
| `BOS_EMAIL_ENRICHMENT_BACKFILL_ENABLED` | — | Enable the bounded runtime backfill that re-runs configured inbound email parsers over stored mail. Off by default. |
| `BOS_EMAIL_TRIAGE_FACT_CACHE_TTL_SECS` | `21600` | Freshness window for cached email-triage provider facts. Positive CRM facts use this TTL; negative CRM facts use the smaller of this value and 1800 seconds. |
| `BOS_EMAIL_TRIAGE_FACT_PROVIDER_BUDGET_PER_MESSAGE` | `2` | Maximum live CRM fact lookups the email-triage resolver may spend for one newly ingested message. Reclassify stays cache-only. |
| `BOS_ENRICHMENT_FRESHNESS_ENABLED` | — | Run the enrichment freshness pump for stale critical staged-draft fields. Off by default. |
| `BOS_ENRICHMENT_FRESHNESS_INTERVAL_SECS` | `1800` | Seconds between enrichment freshness pump cycles (min 300). |
| `BOS_ENRICHMENT_FRESHNESS_MAX_ENRICHMENTS_PER_CYCLE` | `3` | Hard cap on enrichment freshness engine runs per cycle; each run keeps the normal web/search budgets. |
| `BOS_ENRICHMENT_FRESHNESS_STALE_AFTER_SECS` | `2592000` | Age in seconds after which accepted critical enrichment proposals are considered stale. |
| `BOS_ESPOCRM_API_KEY` | — | EspoCRM API key (Administration → API Users; role must grant Note create). |
| `BOS_ESPOCRM_BASE_URL` | — | EspoCRM instance base URL (e.g. http://localhost:4580). |
| `BOS_ESPOCRM_WRITE_ENABLED` | — | Provider write gate for EspoCRM. Off (default) = approved CRM drafts deliver through the dry-run client and no note is created. Flipping it is an attended, operator decision. |
| `BOS_GMAIL_INGEST_ENABLED` | — | Enable the Gmail ingestion pump (1/true/yes). Off by default. |
| `BOS_GMAIL_INGEST_INTERVAL_SECS` | `120` | Seconds between Gmail ingestion polls. |
| `BOS_GMAIL_INGEST_QUERY` | `in:inbox newer_than:14d` | Gmail search query selecting messages to ingest. |
| `BOS_GMAIL_OAUTH_CLIENT_ID` | — | Google OAuth client id for the Gmail read connector. |
| `BOS_GMAIL_OAUTH_CLIENT_SECRET` | — | Google OAuth client secret for the Gmail read connector. |
| `BOS_GMAIL_OAUTH_REFRESH_TOKEN` | — | Google OAuth refresh token for the Gmail read connector. |
| `BOS_GMAIL_OAUTH_SCOPES` | — | Space/comma-separated OAuth scopes. Unset = unknown, scope check skipped. |
| `BOS_GMAIL_TRASH_ENABLED` | — | Provider write gate for explicitly moving Gmail messages to Trash. Off (default) = requests are audited and dry-run without changing Gmail. Requires gmail.modify and is independent from Gmail draft creation. |
| `BOS_GMAIL_WRITE_ENABLED` | — | Provider write gate for Gmail DRAFT creation (never send). Off (default) = approved reply drafts deliver through the dry-run client and no Gmail draft is created. Flipping it is an attended, operator decision. |
| `BOS_GOOGLE_CALENDAR_ID` | `primary` | Calendar approved event drafts write to: "primary" or a specific calendar id (Google Calendar settings → calendar → Integrate). The connected account needs write access to it. |
| `BOS_GOOGLE_CALENDAR_WRITE_ENABLED` | — | Provider write gate for Google Calendar. Off (default) = approved drafts deliver through the dry-run client and no event is created. Flipping it is an attended, operator decision. |
| `BOS_HUBSPOT_ACCESS_TOKEN` | — | HubSpot private-app access token for CRM reads and the gated CRM write path. |
| `BOS_HUBSPOT_DEALS_CLOSED_DATE_PROPERTY` | `closedate` | HubSpot deal property used as the closed date for close-rate/contact-to-close reporting (for example closedate or a client-specific close field). |
| `BOS_HUBSPOT_DEALS_LOST_STAGE_IDS` | — | Comma-separated HubSpot deal stage ids that count as lost for close-rate reporting. Client/pipeline specific; no default. |
| `BOS_HUBSPOT_DEALS_OPEN_STAGE_IDS` | — | Comma-separated HubSpot deal stage ids that count as open for deal reporting. Optional today, retained so the pipeline mapping is complete and client-specific. |
| `BOS_HUBSPOT_DEALS_PIPELINE_ID` | — | HubSpot deal pipeline id used for owner-report close-rate/contact-to-close metrics. Client specific; no default. |
| `BOS_HUBSPOT_DEALS_SEGMENT_PROPERTIES` | — | Comma-separated HubSpot deal properties to retain as configured segment cuts in close-rate reporting (for example dealtype, territory, owner field). Optional. |
| `BOS_HUBSPOT_DEALS_STARTED_DATE_PROPERTY` | `createdate` | HubSpot deal property used as the started/contact date for contact-to-close reporting. Defaults to createdate but should be set per client when their pipeline uses a better field. |
| `BOS_HUBSPOT_DEALS_WON_STAGE_IDS` | — | Comma-separated HubSpot deal stage ids that count as won for close-rate reporting. Client/pipeline specific; no default. |
| `BOS_HUBSPOT_PORTAL_ID` | — | HubSpot portal/account id used to build operator deep links to cached CRM contacts and deals. |
| `BOS_HUBSPOT_WRITE_ENABLED` | — | Provider write gate for HubSpot. Off (default) = approved CRM drafts deliver through the dry-run client and no note is created. Flipping it is an attended, operator decision. |
| `BOS_INVOICE_NINJA_API_TOKEN` | — | Invoice Ninja API token (Settings → Account Management → Integrations → API tokens). |
| `BOS_INVOICE_NINJA_BASE_URL` | — | Self-hosted Invoice Ninja base URL (e.g. http://localhost:8003). |
| `BOS_INVOICE_NINJA_WRITE_ENABLED` | — | Provider write gate for Invoice Ninja. Off (default) = approved ledger entries deliver through the dry-run client and nothing is recorded. Flipping it is an attended, operator decision. |
| `BOS_LEAD_DISCOVERY_AUTOSCRAPE_ENABLED` | — | Run the approved-source lead discovery feed poller. Off by default. |
| `BOS_LEAD_DISCOVERY_AUTOSCRAPE_INTERVAL_SECS` | `1800` | Seconds between approved-source lead discovery feed polling cycles (minimum 300). |
| `BOS_LEAD_DISCOVERY_AUTOSCRAPE_MAX_FINDINGS_PER_CYCLE` | `10` | Maximum new lead findings staged by one approved-source feed polling cycle. |
| `BOS_LLM_API_ENDPOINT` | — | Override base URL for the LLM API backend. Unset = provider default. |
| `BOS_LLM_API_KEY` | — | API key for the LLM API backend. |
| `BOS_LLM_API_MODEL` | — | Model id for the LLM API backend. |
| `BOS_LLM_API_PROVIDER` | `anthropic` | LLM API backend provider: anthropic | openai | openrouter. |
| `BOS_LLM_DEFAULT_BACKEND` | — | Typed-LLM backend route: api | harness | local. Unset defaults to api. |
| `BOS_LLM_DEFAULT_MODEL` | — | Default model id/alias for the selected typed-LLM backend. Per-backend and per-purpose model settings override it. |
| `BOS_LLM_HARNESS_MODEL` | — | Model the harness session should use. Unset = harness default. |
| `BOS_LLM_HARNESS_PROGRAM` | `claude` | Executable path/name for the local typed-LLM harness CLI. |
| `BOS_LLM_HARNESS_THINKING_LEVEL` | — | Thinking/effort level for harness sessions. |
| `BOS_LLM_LOCAL_API_KEY` | — | Optional API key for the loopback-only OpenAI-compatible local LLM backend. |
| `BOS_LLM_LOCAL_ENDPOINT` | `http://127.0.0.1:11434/v1/chat/completions` | Loopback OpenAI-compatible endpoint for local inference (Ollama/LM Studio). Non-loopback endpoints are refused. |
| `BOS_LLM_LOCAL_MODEL` | — | Default model id for the loopback-only local LLM backend. |
| `BOS_LLM_MAX_TOKENS` | `4096` | Max output tokens for API backend calls. |
| `BOS_LLM_ROUTE_OVERRIDES` | — | Per-purpose typed-LLM routing overrides, comma list of purpose=api|harness|local optionally followed by :model (e.g. social_post_draft=local:qwen3). Local uses the loopback-only OpenAI-compatible profile and never falls back remotely. |
| `BOS_LLM_TIMEOUT_MS` | `120000` | Timeout for one typed LLM task execution. |
| `BOS_LOG_LEVEL` | `info` | Tracing filter (e.g. info, bos_app=debug). |
| `BOS_OPERATOR_TOKEN` | — | Bearer token required on operator routes. Unset = open (local dev only). |
| `BOS_OUTBOX_DELIVERY_ENABLED` | `1` | Run the outbox delivery worker (on by default; set 0 to pause all provider deliveries). |
| `BOS_OUTBOX_DELIVERY_INTERVAL_SECS` | `15` | Seconds between outbox delivery polls. |
| `BOS_OWNER_REPORT_ALLOWED_OPERATOR_USER_IDS` | — | Comma/space-separated operator user ids allowed to view, generate, and email owner reports. Overrides overlay [owner_reports].allowed_operator_user_ids. Empty = any authenticated operator. |
| `BOS_OWNER_REPORT_CALL_VOLUME_CATEGORY_ID` | — | Email triage category whose inbound messages count as the owner-report call volume metric. Overrides overlay [owner_reports.call_volume].category_id. Unset with no overlay config renders the metric as pending data. |
| `BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_LABEL` | — | Gmail label name used by the deployment for the call-summary source. Overrides overlay [owner_reports.call_volume].gmail_label; used for pending-data honesty, not as a second ingestion path. |
| `BOS_OWNER_REPORT_CALL_VOLUME_GMAIL_QUERY` | — | Gmail query/source selector that should include the call-summary emails counted by owner reports. Overrides overlay [owner_reports.call_volume].gmail_query; used for pending-data honesty, not as a second ingestion path. |
| `BOS_OWNER_REPORT_CALL_VOLUME_LABEL` | — | Operator-facing label for the owner-report call volume KPI. Overrides overlay [owner_reports.call_volume].label. |
| `BOS_OWNER_REPORT_CALL_VOLUME_SOURCE_LABEL` | — | Coverage/source wording for the owner-report call volume KPI, e.g. whether answering-service summaries represent all calls or only summarized calls. Overrides overlay [owner_reports.call_volume].source_label. |
| `BOS_PACKET_PROPOSAL_RUNNING_STALE_AFTER_MS` | `3600000` | Milliseconds after which a still-running Smart draft proposal run is treated as stale on the next read. Default is one hour. |
| `BOS_PACKET_PROPOSAL_TOOL_LOOP_ENABLED` | — | Enable Smart draft's backend-only recorded tool-loop proposal mode. Off by default; no operator-facing trigger is wired in v1. |
| `BOS_PUBLIC_BASE_URL` | `http://127.0.0.1:4400` | Externally reachable base URL (OAuth redirect URIs are derived from it). |
| `BOS_QBO_CLIENT_ID` | — | Intuit OAuth app client id for the QuickBooks Online read connector. |
| `BOS_QBO_CLIENT_SECRET` | — | Intuit OAuth app client secret for the QuickBooks Online read connector. |
| `BOS_QBO_ENVIRONMENT` | `sandbox` | QuickBooks environment: sandbox | production. Selects the API base URL; the connected realm must match. |
| `BOS_QBO_WRITE_ENABLED` | — | Provider write gate for QuickBooks Online (record-payment only). Off (default) = approved ledger entries deliver through the dry-run client and nothing is recorded. Flipping it is an attended, operator decision. |
| `BOS_RELEASE_NOTES_WEBHOOK_SECRET` | — | Bearer token required on /api/webhooks/release-notes. Unset = the webhook route 404s. |
| `BOS_REPORT_DIGEST_DELIVERY_ENABLED` | — | Explicit gate for scheduled owner-report Gmail draft delivery. Requires BOS_REPORT_DIGEST_ENABLED plus recipients and a due schedule. Off by default; manual Email-to-owners still works. |
| `BOS_REPORT_DIGEST_ENABLED` | — | Run the owner-digest pump (generates reports when missing or stale, and evaluates scheduled delivery). Off by default; Generate-now in the Reports view always works. |
| `BOS_REPORT_DIGEST_INTERVAL_SECS` | `21600` | Seconds between owner-digest pump cycles (min 600 — a digest is at most daily-fresh; each stale period costs one LLM narration call). |
| `BOS_REPORT_DIGEST_METRICS` | — | Ordered owner-report email metric ids, comma/space separated. Known ids: sales, calls, follow_ups, inventory, orders, damage_claims, site_traffic, close_rate. Overrides overlay [owner_reports].metrics; unknown ids are ignored. |
| `BOS_REPORT_DIGEST_MTD_DAY` | — | Day of month for scheduled month-to-date owner-report delivery (1-31). Overrides overlay [owner_reports].mtd_day. Unset disables MTD scheduled delivery. |
| `BOS_REPORT_DIGEST_REDACT_FINANCIALS_FOR` | — | Recipient address list for owner-report Gmail drafts that must omit financial metrics and narration that may contain dollar figures. Accepts comma/semicolon/space separated addresses and augments overlay [owner_reports].recipient_profiles without sales metrics. |
| `BOS_REPORT_DIGEST_SUBJECT_PREFIX` | — | Subject prefix for owner-report Gmail drafts. Overrides overlay [owner_reports].subject_prefix. Default: Owner digest. |
| `BOS_REPORT_DIGEST_TO_ADDR` | — | Recipient(s) of owner-digest Gmail drafts. Accepts comma/semicolon/space separated addresses and overrides overlay [owner_reports].recipients. Unset with no overlay recipients = the email-digest action refuses with owner_report_to_addr_unset. |
| `BOS_REPORT_DIGEST_WEEKLY_WEEKDAY` | — | Weekday for scheduled weekly owner-report delivery (monday..sunday). Overrides overlay [owner_reports].weekly_weekday. Unset disables weekly scheduled delivery. |
| `BOS_SEARCH_CONSOLE_ANALYTICS_EXCLUDED_REFERRER_DOMAINS` | — | Comma/space-separated referrer-spam domains excluded from GA4 reporting views in addition to the vendored community list. Overrides overlay [search_console].analytics_excluded_referrer_domains. Raw GA4 snapshots are unchanged. |
| `BOS_SEARCH_CONSOLE_BRANDED_QUERY_PATTERNS` | — | Comma-separated case-insensitive branded query patterns for Search Console cuts (supports `*` wildcard). Overrides overlay [search_console] branded_query_patterns. |
| `BOS_SEARCH_CONSOLE_GA4_PROPERTY_ID` | — | Numeric GA4 property id for behavior/acquisition/conversion reporting. Overrides overlay [search_console] ga4_property_id. Unset renders GA4 metrics as pending setup. |
| `BOS_SEARCH_CONSOLE_MAX_REQUESTS_PER_CYCLE` | `8` | Hard cap on Google Search Console API requests per sync cycle. |
| `BOS_SEARCH_CONSOLE_PROPERTY_URL` | — | Google Search Console property id/url to sync, e.g. sc-domain:example.com or https://www.example.com/. Overrides overlay [search_console] property_url. |
| `BOS_SEARCH_CONSOLE_SYNC_DAYS` | `90` | Recent finalized Search Console days refreshed each sync cycle. |
| `BOS_SEARCH_CONSOLE_SYNC_ENABLED` | — | Run the Search Console sync pump (read-only, request-budgeted). Off by default; manual Sync-now works regardless. |
| `BOS_SEARCH_CONSOLE_SYNC_INTERVAL_SECS` | `1800` | Seconds between Search Console sync pump cycles (min 300). |
| `BOS_SEARCH_CONSOLE_USER_ID` | — | Operator user whose Google credential reads the configured Search Console property. Unset = acting/only credential fallback. |
| `BOS_SERVER_BIND` | `127.0.0.1:4400` | Listen address for the HTTP server. |
| `BOS_SHOPIFY_ACCESS_TOKEN` | — | Shopify Admin API access token used by the read-only sales sync and approved customer-tier writes. |
| `BOS_SHOPIFY_API_VERSION` | `2026-01` | Shopify Admin GraphQL API version used by Shopify sales sync and customer-tier writes. |
| `BOS_SHOPIFY_CLIENT_ID` | — | Shopify custom app client id used to fetch Admin API access tokens when BOS_SHOPIFY_ACCESS_TOKEN is not set. |
| `BOS_SHOPIFY_CLIENT_SECRET` | — | Shopify custom app client secret used to fetch Admin API access tokens when BOS_SHOPIFY_ACCESS_TOKEN is not set. |
| `BOS_SHOPIFY_READ_SYNC_ENABLED` | — | Enable the background Shopify sales cache sync. Off by default; manual sync remains available. |
| `BOS_SHOPIFY_READ_SYNC_INTERVAL_SECS` | `1800` | Seconds between background Shopify sales cache sync cycles. Values below 300 are clamped. |
| `BOS_SHOPIFY_READ_SYNC_MAX_ORDERS_PER_CYCLE` | `250` | Maximum recent Shopify orders fetched per sync cycle. Values are clamped to Shopify's page cap. |
| `BOS_SHOPIFY_SALES_VISIBILITY_POLICY` | `authorizer_only` | Controls Shopify sales dollar visibility. Allowed modes: shared, admin_only, or authorizer_only; empty/unset defaults to authorizer_only. Client overlays may set this to shared while CRM deal dollars remain authorizer_only. |
| `BOS_SHOPIFY_SHOP_DOMAIN` | — | Shopify shop domain (for example example.myshopify.com) for sales sync and approved customer-tier writes. |
| `BOS_SHOPIFY_TIER_MAPPING_JSON` | — | Optional JSON object mapping QBO tier names to explicit Shopify targets. When set, it overrides overlay copy-through tier configuration. Example: {"Wholesale":{"tag":"Wholesale","metafield_namespace":"customer","metafield_key":"tier","metafield_value":"Wholesale","segment_query":"customer_tags CONTAINS 'Wholesale'"}}. |
| `BOS_SHOPIFY_WRITE_ENABLED` | — | Provider write gate for Shopify customer-tier sync. Off (default) = approved sync runs deliver through the dry-run client and no Shopify customer is changed. Opening it is an attended operator decision. |
| `BOS_STATE_DIR` | `./state` | Directory holding the sqlite database and runtime state. |
| `BOS_STOCKFORGE_API_KEY` | — | Stockforge org API key (sfk_live_…) for the read-only inventory connector — an org ADMIN creates a VIEWER-role key in Stockforge Settings → API Keys (shown once). |
| `BOS_STOCKFORGE_APP_URL` | — | Stockforge user-facing app base URL for dashboard deep links (e.g. https://app.stockforge.ai). If unset, known api.stockforge.ai URLs are mapped to app.stockforge.ai. |
| `BOS_STOCKFORGE_BASE_URL` | — | Stockforge API base URL for the read-only inventory connector (e.g. https://api.stockforge.ai). |
| `BOS_STOCKFORGE_MAX_REQUESTS_PER_CYCLE` | `10` | Hard cap on Stockforge API requests per sync cycle (a full cycle needs ~5). |
| `BOS_STOCKFORGE_SYNC_ENABLED` | — | Run the Stockforge sync pump (request-budgeted). Off by default; the manual Sync-now route works regardless. |
| `BOS_STOCKFORGE_SYNC_INTERVAL_SECS` | `900` | Seconds between Stockforge sync pump cycles (min 120 — the order board likes to be fresh). Webhooks make this a fallback cadence. |
| `BOS_STOCKFORGE_WEBHOOK_SECRET` | — | Per-endpoint secret from registering this server's /api/webhooks/stockforge URL as a Stockforge webhook (ADMIN, shown once). Unset = the webhook route 404s; verification is HMAC-SHA256. |
| `BOS_STRIPE_SECRET_KEY` | — | Stripe secret (or restricted) API key for BOS_ACCOUNTING_PROVIDER=stripe — invoice/customer reads, and (behind BOS_STRIPE_WRITE_ENABLED) draft-invoice creation. Prefer a restricted key scoped to Customers, Invoices, and Invoice Items read+write. |
| `BOS_STRIPE_WRITE_ENABLED` | — | Provider write gate for Stripe (create-invoice-DRAFT only; the invoice is never finalized or sent by BusinessOS). Off (default) = approved invoice drafts deliver through the dry-run client and nothing reaches Stripe. Flipping it is an attended, operator decision. |
| `BOS_WEB_ENRICHMENT_ENABLED` | `1` | Kill-switch for guarded website enrichment (read-only fetch of an operator-authored domain that prefills eligible draft fields). Default ON — this is a read, gated only by the same operator note that named the domain; set to 0/false to disable the crawl entirely. |
| `BOS_WEB_SEARCH_ENRICHMENT_API_KEY` | — | Bearer token for the optional web-search enrichment endpoint. Used only when BOS_WEB_SEARCH_ENRICHMENT_ENABLED is on. |
| `BOS_WEB_SEARCH_ENRICHMENT_COST_BUDGET_MICROS` | `100000` | Per-enrichment paid-search budget in micros. Zero refuses search even when the feature gate is on. |
| `BOS_WEB_SEARCH_ENRICHMENT_ENABLED` | — | Enable external web-search enrichment for eligible draft fields. Off by default; LLMs receive only curated, cited search evidence, never arbitrary browser access. |
| `BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT` | — | JSON web-search endpoint template for the generic/SearXNG provider, e.g. a self-hosted SearXNG https://searxng.local/search?q={query}&format=json. Common result JSON shapes are parsed (incl. SearXNG `content`). This is the recommended keyless default. |
| `BOS_WEB_SEARCH_ENRICHMENT_FALLBACK_ENDPOINT` | — | Keyless fallback web-search endpoint queried (as a Generic provider, no API key) when the primary provider errors or rate-limits — e.g. a self-hosted SearXNG URL https://searxng.local/search?q={query}&format=json. Unset = no fallback. |
| `BOS_WEB_SEARCH_ENRICHMENT_MAX_FETCHED_PAGES` | `2` | Max public result pages fetched through the guarded crawler per search enrichment run. |
| `BOS_WEB_SEARCH_ENRICHMENT_MAX_QUERIES` | `1` | Max search queries per draft enrichment run. |
| `BOS_WEB_SEARCH_ENRICHMENT_MAX_RESULTS` | `3` | Max search results retained per query for draft enrichment diagnostics. |
| `BOS_WEB_SEARCH_ENRICHMENT_PROVIDER` | — | Web-search enrichment provider. Supported values: searxng (alias of generic — the recommended keyless self-hosted default, set BOS_WEB_SEARCH_ENRICHMENT_ENDPOINT to its URL) | generic | tavily (needs an API key). Unset = generic endpoint path. |
| `BOS_WEB_SEARCH_ENRICHMENT_TIMEOUT_MS` | `10000` | Timeout for one search API call during draft enrichment. |
