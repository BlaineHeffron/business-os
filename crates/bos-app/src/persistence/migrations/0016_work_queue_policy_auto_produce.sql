-- Per-category auto-produce opt-in: accepting an item in this category lets
-- the auto-produce pump draft its packet kinds automatically (LLM cost), so
-- the flag defaults off and the operator flips it per category.
ALTER TABLE work_queue_policies
    ADD COLUMN auto_produce INTEGER NOT NULL DEFAULT 0;
