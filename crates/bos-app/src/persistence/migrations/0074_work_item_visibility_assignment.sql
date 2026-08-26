-- Shared-inbox queue visibility and local assignment.
-- source_user_id remains the provider/input owner; these rows control which
-- named operators may see/mutate a work item in the queue.

ALTER TABLE work_items ADD COLUMN assignee_user_id TEXT;

CREATE TABLE work_item_visibility (
    client_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, item_id, user_id)
);

CREATE INDEX work_item_visibility_user
    ON work_item_visibility (client_id, user_id, item_id);

INSERT OR IGNORE INTO work_item_visibility
    (client_id, item_id, user_id, created_at_ms)
SELECT client_id, item_id, source_user_id, created_at_ms
FROM work_items
WHERE source_user_id IS NOT NULL AND source_user_id != '';
