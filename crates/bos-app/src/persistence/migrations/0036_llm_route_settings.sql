-- ai_usage slice: operator-configurable typed-LLM routing settings.
-- Secrets remain deployment config; these rows choose backend/model/time-budget.

CREATE TABLE llm_route_settings (
    client_id TEXT PRIMARY KEY,
    default_backend TEXT NOT NULL CHECK (default_backend IN ('api', 'harness')),
    default_model TEXT,
    max_tokens INTEGER NOT NULL,
    timeout_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE llm_route_overrides (
    client_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    backend TEXT NOT NULL CHECK (backend IN ('api', 'harness')),
    model TEXT,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, purpose)
);
