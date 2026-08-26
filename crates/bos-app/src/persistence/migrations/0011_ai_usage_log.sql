-- ai_usage slice: one row per typed-LLM execution (API route) or per harness
-- attempt (harness route). Written through store_core like every mutation.

CREATE TABLE ai_usage_log (
    client_id TEXT NOT NULL,
    usage_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    task_kind TEXT,
    route TEXT NOT NULL,             -- api | harness
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    thinking_level TEXT,
    tokens_in INTEGER,
    tokens_out INTEGER,
    total_tokens INTEGER,
    cached_tokens INTEGER,
    cost_micros INTEGER,
    latency_ms INTEGER NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 1,
    error_code TEXT,
    correlation_id TEXT NOT NULL DEFAULT '',
    provider_request_id TEXT,
    recorded_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, usage_id)
);

CREATE INDEX ai_usage_log_recent
    ON ai_usage_log (client_id, recorded_at_ms DESC);
