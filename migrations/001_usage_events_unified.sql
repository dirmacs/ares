-- Migration 001: Unified usage_events table
--
-- Replaces two conflicting schemas:
--   - db/tenants.rs used: (id, tenant_id, request_count, token_count, created_at)
--   - mcp/usage.rs used: (tenant_id, operation, tokens_used, effective_tokens, success, duration_ms, source, created_at)
--
-- All new rows use created_at as Unix timestamp (BIGINT), source as 'http' or 'mcp'.
-- Run once on the VPS: psql -U ares -d ares -f migrations/001_usage_events_unified.sql

-- Drop existing table if schema is incompatible (safe because monthly_usage_cache is the source of truth for quotas)
DROP TABLE IF EXISTS usage_events;

CREATE TABLE usage_events (
    id               TEXT    NOT NULL PRIMARY KEY,
    tenant_id        TEXT    NOT NULL,
    source           TEXT    NOT NULL DEFAULT 'http',   -- 'http' or 'mcp'

    -- HTTP request tracking
    request_count    BIGINT  NOT NULL DEFAULT 1,        -- always 1 per API call
    token_count      BIGINT  NOT NULL DEFAULT 0,        -- input + output tokens (estimated or real)

    -- MCP operation tracking (NULL for HTTP events)
    operation        TEXT,                              -- e.g. 'mcp.ares_run_agent'
    tokens_used      BIGINT  NOT NULL DEFAULT 0,        -- actual LLM tokens (0 for non-LLM ops)
    effective_tokens BIGINT  NOT NULL DEFAULT 0,        -- max(tokens_used, operation_weight)
    success          BOOLEAN NOT NULL DEFAULT TRUE,
    duration_ms      BIGINT  NOT NULL DEFAULT 0,

    created_at       BIGINT  NOT NULL                   -- Unix timestamp (seconds)
);

CREATE INDEX idx_usage_events_tenant_id  ON usage_events (tenant_id);
CREATE INDEX idx_usage_events_created_at ON usage_events (created_at);
CREATE INDEX idx_usage_events_source     ON usage_events (source);

-- Aggregation cache tables (idempotent)
CREATE TABLE IF NOT EXISTS monthly_usage_cache (
    tenant_id     TEXT   NOT NULL,
    usage_month   BIGINT NOT NULL,   -- Unix timestamp of month start (UTC)
    request_count BIGINT NOT NULL DEFAULT 0,
    token_count   BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, usage_month)
);

CREATE TABLE IF NOT EXISTS daily_rate_limits (
    tenant_id     TEXT   NOT NULL,
    usage_date    BIGINT NOT NULL,   -- Unix timestamp of day start (UTC)
    request_count BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, usage_date)
);
