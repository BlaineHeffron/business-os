INSERT INTO entity_revisions (client_id, entity_kind, entity_id, revision, updated_at_ms)
SELECT t.client_id, 'task', t.task_id, 1, t.updated_at_ms
FROM tasks t
WHERE NOT EXISTS (
    SELECT 1
    FROM entity_revisions er
    WHERE er.client_id = t.client_id
      AND er.entity_kind = 'task'
      AND er.entity_id = t.task_id
);
