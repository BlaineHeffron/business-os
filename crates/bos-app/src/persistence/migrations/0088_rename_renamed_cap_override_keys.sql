-- Phase-2 env-var rename: preserve admin-panel runtime overrides across the
-- no-alias rename of two editable caps. Idempotent; affects 0 rows if unset.
--
-- If a canonical row already exists, keep the newer row by updated_at_ms. Move
-- entity_revisions too because runtime-setting optimistic revisions are keyed
-- by the env var name.
INSERT OR REPLACE INTO runtime_setting_overrides
    (client_id, var_name, value, updated_at_ms)
SELECT legacy.client_id,
       'BOS_AUTO_PRODUCE_MAX_PER_CYCLE',
       legacy.value,
       legacy.updated_at_ms
    FROM runtime_setting_overrides legacy
    LEFT JOIN runtime_setting_overrides canonical
      ON canonical.client_id = legacy.client_id
     AND canonical.var_name = 'BOS_AUTO_PRODUCE_MAX_PER_CYCLE'
    WHERE legacy.var_name = 'BOS_PRODUCE_MAX_PER_CYCLE'
      AND (canonical.client_id IS NULL OR legacy.updated_at_ms >= canonical.updated_at_ms);
DELETE FROM runtime_setting_overrides
    WHERE var_name = 'BOS_PRODUCE_MAX_PER_CYCLE';

INSERT OR REPLACE INTO entity_revisions
    (client_id, entity_kind, entity_id, revision, updated_at_ms)
SELECT legacy.client_id,
       legacy.entity_kind,
       'BOS_AUTO_PRODUCE_MAX_PER_CYCLE',
       legacy.revision,
       legacy.updated_at_ms
    FROM entity_revisions legacy
    LEFT JOIN entity_revisions canonical
      ON canonical.client_id = legacy.client_id
     AND canonical.entity_kind = legacy.entity_kind
     AND canonical.entity_id = 'BOS_AUTO_PRODUCE_MAX_PER_CYCLE'
    WHERE legacy.entity_kind = 'runtime_setting_override'
      AND legacy.entity_id = 'BOS_PRODUCE_MAX_PER_CYCLE'
      AND (canonical.client_id IS NULL OR legacy.updated_at_ms >= canonical.updated_at_ms);
DELETE FROM entity_revisions
    WHERE entity_kind = 'runtime_setting_override'
      AND entity_id = 'BOS_PRODUCE_MAX_PER_CYCLE';

INSERT OR REPLACE INTO runtime_setting_overrides
    (client_id, var_name, value, updated_at_ms)
SELECT legacy.client_id,
       'BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE',
       legacy.value,
       legacy.updated_at_ms
    FROM runtime_setting_overrides legacy
    LEFT JOIN runtime_setting_overrides canonical
      ON canonical.client_id = legacy.client_id
     AND canonical.var_name = 'BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE'
    WHERE legacy.var_name = 'BOS_AI_TRIAGE_MAX_PER_CYCLE'
      AND (canonical.client_id IS NULL OR legacy.updated_at_ms >= canonical.updated_at_ms);
DELETE FROM runtime_setting_overrides
    WHERE var_name = 'BOS_AI_TRIAGE_MAX_PER_CYCLE';

INSERT OR REPLACE INTO entity_revisions
    (client_id, entity_kind, entity_id, revision, updated_at_ms)
SELECT legacy.client_id,
       legacy.entity_kind,
       'BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE',
       legacy.revision,
       legacy.updated_at_ms
    FROM entity_revisions legacy
    LEFT JOIN entity_revisions canonical
      ON canonical.client_id = legacy.client_id
     AND canonical.entity_kind = legacy.entity_kind
     AND canonical.entity_id = 'BOS_AI_TRIAGE_MAX_LLM_CALLS_PER_CYCLE'
    WHERE legacy.entity_kind = 'runtime_setting_override'
      AND legacy.entity_id = 'BOS_AI_TRIAGE_MAX_PER_CYCLE'
      AND (canonical.client_id IS NULL OR legacy.updated_at_ms >= canonical.updated_at_ms);
DELETE FROM entity_revisions
    WHERE entity_kind = 'runtime_setting_override'
      AND entity_id = 'BOS_AI_TRIAGE_MAX_PER_CYCLE';
