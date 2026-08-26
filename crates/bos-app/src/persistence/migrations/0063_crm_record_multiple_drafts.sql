-- CRM record-create drafts can now fan out one work item into multiple staged
-- drafts, one per missing contact, while preserving per-draft approval.
DROP INDEX IF EXISTS crm_record_drafts_active_item;
