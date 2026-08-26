ALTER TABLE follow_up_task_drafts ADD COLUMN source_user_id TEXT;
ALTER TABLE tasks ADD COLUMN source_user_id TEXT;

UPDATE follow_up_task_drafts
SET source_user_id = (
  SELECT w.source_user_id
  FROM work_items w
  WHERE w.client_id = follow_up_task_drafts.client_id
    AND w.item_id = follow_up_task_drafts.item_id
)
WHERE source_user_id IS NULL;

UPDATE tasks
SET source_user_id = (
  SELECT d.source_user_id
  FROM follow_up_task_drafts d
  WHERE d.client_id = tasks.client_id
    AND d.task_id = tasks.task_id
)
WHERE source_user_id IS NULL;
