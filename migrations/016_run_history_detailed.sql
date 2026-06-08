-- Migration 016: Detailed run history with LLM calls, tool calls, and cost tracking
-- Provides step-by-step observability for agent runs

-- Step 1: LLM calls within an agent run
CREATE TABLE IF NOT EXISTS run_llm_calls (
    id              TEXT    PRIMARY KEY,
    run_id          TEXT    NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    tenant_id       TEXT    NOT NULL,
    agent_name      TEXT    NOT NULL,
    step_index      INT     NOT NULL,              -- execution order within the run
    provider        TEXT    NOT NULL,              -- e.g., 'openai', 'nvidia', 'anthropic', 'ollama'
    model           TEXT    NOT NULL,              -- e.g., 'gpt-4o', 'nemotron-3-ultra'
    prompt_tokens   BIGINT  NOT NULL DEFAULT 0,
    completion_tokens BIGINT NOT NULL DEFAULT 0,
    total_tokens    BIGINT  NOT NULL DEFAULT 0,
    estimated_cost_usd NUMERIC(12,6) NOT NULL DEFAULT 0,  -- computed from pricing config
    latency_ms      BIGINT  NOT NULL DEFAULT 0,
    status          TEXT    NOT NULL DEFAULT 'success', -- 'success', 'error', 'timeout'
    error_message   TEXT,
    request_payload JSONB,                          -- sanitized request (no API keys)
    response_payload JSONB,                         -- sanitized response
    created_at      BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_llm_calls_run ON run_llm_calls(run_id);
CREATE INDEX IF NOT EXISTS idx_run_llm_calls_tenant ON run_llm_calls(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_run_llm_calls_agent ON run_llm_calls(agent_name, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_run_llm_calls_model ON run_llm_calls(model, created_at DESC);

-- Step 2: Tool calls within an agent run
CREATE TABLE IF NOT EXISTS run_tool_calls (
    id              TEXT    PRIMARY KEY,
    run_id          TEXT    NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    tenant_id       TEXT    NOT NULL,
    agent_name      TEXT    NOT NULL,
    step_index      INT     NOT NULL,
    tool_name       TEXT    NOT NULL,               -- e.g., 'http_get', 'sql_query', 'mcp_search'
    tool_type       TEXT    NOT NULL,               -- 'http', 'script', 'sql', 'mcp'
    arguments       JSONB   NOT NULL,               -- sanitized arguments
    result          JSONB,                          -- sanitized result
    latency_ms      BIGINT  NOT NULL DEFAULT 0,
    status          TEXT    NOT NULL DEFAULT 'success', -- 'success', 'error', 'timeout'
    error_message   TEXT,
    created_at      BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_tool_calls_run ON run_tool_calls(run_id);
CREATE INDEX IF NOT EXISTS idx_run_tool_calls_tenant ON run_tool_calls(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_run_tool_calls_tool ON run_tool_calls(tool_name, created_at DESC);

-- Step 3: Agent run cost aggregation (per-run cost summary)
CREATE TABLE IF NOT EXISTS run_costs (
    run_id          TEXT    PRIMARY KEY REFERENCES agent_runs(id) ON DELETE CASCADE,
    tenant_id       TEXT    NOT NULL,
    agent_name      TEXT    NOT NULL,
    total_llm_calls INT     NOT NULL DEFAULT 0,
    total_tool_calls INT    NOT NULL DEFAULT 0,
    total_prompt_tokens BIGINT NOT NULL DEFAULT 0,
    total_completion_tokens BIGINT NOT NULL DEFAULT 0,
    total_estimated_cost_usd NUMERIC(12,6) NOT NULL DEFAULT 0,
    total_duration_ms BIGINT NOT NULL DEFAULT 0,
    created_at      BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_costs_tenant ON run_costs(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_run_costs_agent ON run_costs(agent_name, created_at DESC);

-- Step 4: Budget configuration per tenant
CREATE TABLE IF NOT EXISTS tenant_budgets (
    tenant_id           TEXT    PRIMARY KEY,
    monthly_limit_usd   NUMERIC(12,2) NOT NULL,    -- monthly budget in USD
    daily_limit_usd     NUMERIC(12,2),             -- optional daily budget
    alert_threshold_pct INT     NOT NULL DEFAULT 80, -- alert at % of budget
    currency            TEXT    NOT NULL DEFAULT 'USD',
    created_at          BIGINT  NOT NULL,
    updated_at          BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tenant_budgets_updated ON tenant_budgets(updated_at DESC);

-- Step 5: Budget alert log (tracks when alerts fired)
CREATE TABLE IF NOT EXISTS budget_alerts (
    id              TEXT    PRIMARY KEY,
    tenant_id       TEXT    NOT NULL REFERENCES tenants(id),
    alert_type      TEXT    NOT NULL,              -- 'daily_exceeded', 'monthly_exceeded', 'threshold_reached'
    current_spend_usd NUMERIC(12,2) NOT NULL,
    limit_usd       NUMERIC(12,2) NOT NULL,
    threshold_pct   INT     NOT NULL,
    period_start    BIGINT  NOT NULL,              -- Unix timestamp of period start
    period_end      BIGINT  NOT NULL,              -- Unix timestamp of period end
    acknowledged    BOOLEAN NOT NULL DEFAULT FALSE,
    acknowledged_by TEXT,
    acknowledged_at BIGINT,
    created_at      BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_budget_alerts_tenant ON budget_alerts(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_budget_alerts_unacked ON budget_alerts(acknowledged) WHERE acknowledged = FALSE;

-- Step 6: Agent health metrics (aggregated periodically)
CREATE TABLE IF NOT EXISTS agent_health_metrics (
    id                  TEXT    PRIMARY KEY,
    tenant_id           TEXT    NOT NULL,
    agent_name          TEXT    NOT NULL,
    period_start        BIGINT  NOT NULL,          -- Unix timestamp (e.g., hour/day bucket)
    period_end          BIGINT  NOT NULL,
    total_runs          BIGINT  NOT NULL DEFAULT 0,
    successful_runs     BIGINT  NOT NULL DEFAULT 0,
    failed_runs         BIGINT  NOT NULL DEFAULT 0,
    avg_latency_ms      BIGINT  NOT NULL DEFAULT 0,
    p50_latency_ms      BIGINT  NOT NULL DEFAULT 0,
    p95_latency_ms      BIGINT  NOT NULL DEFAULT 0,
    p99_latency_ms      BIGINT  NOT NULL DEFAULT 0,
    total_tokens        BIGINT  NOT NULL DEFAULT 0,
    total_cost_usd      NUMERIC(12,6) NOT NULL DEFAULT 0,
    error_rate_pct      NUMERIC(5,2) NOT NULL DEFAULT 0,
    created_at          BIGINT  NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_health_tenant ON agent_health_metrics(tenant_id, period_start DESC);
CREATE INDEX IF NOT EXISTS idx_agent_health_agent ON agent_health_metrics(agent_name, period_start DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_health_unique ON agent_health_metrics(tenant_id, agent_name, period_start, period_end);