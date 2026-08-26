-- Website-enrichment trace: a bounded, operator-facing record of what the
-- read-only crawl fetched, what the deterministic pass extracted, and the exact
-- text fed to the gap-filler LLM. Nullable; populated by the enrichment
-- after_stage hook and cleared on approval (it is a pre-approval review aid, not
-- durable state). JSON shape = bos_contracts::crm_record_drafts::CrmEnrichmentTrace.
ALTER TABLE crm_record_drafts ADD COLUMN enrichment_trace_json TEXT;
