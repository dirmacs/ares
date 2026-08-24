-- Migration 024: Per-tenant LLM token budgets
-- DIR1-54

CREATE TABLE IF NOT EXISTS tenant_token_budgets (
    id          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id   TEXT NOT NULL UNIQUE,
    period      TEXT NOT NULL DEFAULT 'monthly',
    token_limit BIGINT NOT NULL,
    tokens_used BIGINT NOT NULL DEFAULT 0,
    period_start BIGINT NOT NULL,
    period_end   BIGINT NOT NULL,
    alert_threshold BIGINT NOT NULL DEFAULT 80,
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_token_budgets_tenant ON tenant_token_budgets(tenant_id);

-- Track token usage by run for audit
CREATE TABLE IF NOT EXISTS token_usage_log (
    id          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id   TEXT NOT NULL,
    run_id      TEXT,
    agent_name  TEXT,
    model       TEXT,
    input_tokens  BIGINT NOT NULL,
    output_tokens BIGINT NOT NULL,
    total_tokens  BIGINT NOT NULL,
    created_at    BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_token_usage_tenant ON token_usage_log(tenant_id, created_at);
