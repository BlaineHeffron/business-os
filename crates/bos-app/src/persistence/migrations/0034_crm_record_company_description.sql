-- Company description for a proposed CRM account (e.g. the og:description /
-- About-page summary from website enrichment). Nullable; operator-editable;
-- written to the EspoCRM Account's description field on create.
ALTER TABLE crm_record_drafts ADD COLUMN company_description TEXT;
