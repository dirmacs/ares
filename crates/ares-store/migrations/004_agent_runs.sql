-- Agent run tracking: records every agent execution with timing and token counts
CREATE TABLE IF NOT EXISTS agent_runs (
    id            TEXT    PRIMARY KEY,
    tenant_id     TEXT    NOT NULL REFERENCES tenants(id),
    agent_name    TEXT    NOT NULL,
    user_id       TEXT,
    status        TEXT    NOT NULL DEFAULT 'completed',  -- pending, running, completed, failed
    input_tokens  BIGINT  NOT NULL DEFAULT 0,
    output_tokens BIGINT  NOT NULL DEFAULT 0,
    duration_ms   BIGINT  NOT NULL DEFAULT 0,
    error         TEXT,
    created_at    BIGINT  NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_runs_tenant ON agent_runs(tenant_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_agent  ON agent_runs(tenant_id, agent_name);
CREATE INDEX IF NOT EXISTS idx_agent_runs_created ON agent_runs(created_at);
