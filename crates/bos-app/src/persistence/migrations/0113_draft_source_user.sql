-- Source ownership for CRM-record, invoice, and ledger drafts. Personal users
-- may only list/mutate drafts whose source_user_id matches; All-scope still
-- sees legacy NULL rows.

ALTER TABLE crm_record_drafts ADD COLUMN source_user_id TEXT;
ALTER TABLE invoice_drafts ADD COLUMN source_user_id TEXT;
ALTER TABLE ledger_entry_drafts ADD COLUMN source_user_id TEXT;

UPDATE crm_record_drafts
SET source_user_id = (
  SELECT w.source_user_id
  FROM work_items w
  WHERE w.client_id = crm_record_drafts.client_id
    AND w.item_id = crm_record_drafts.item_id
)
WHERE source_user_id IS NULL;

UPDATE invoice_drafts
SET source_user_id = (
  SELECT w.source_user_id
  FROM work_items w
  WHERE w.client_id = invoice_drafts.client_id
    AND w.item_id = invoice_drafts.item_id
)
WHERE source_user_id IS NULL;

UPDATE ledger_entry_drafts
SET source_user_id = (
  SELECT w.source_user_id
  FROM work_items w
  WHERE w.client_id = ledger_entry_drafts.client_id
    AND w.item_id = ledger_entry_drafts.item_id
)
WHERE source_user_id IS NULL;
