-- Migration 001: Unified usage_events table
--
-- Resolves two conflicting schemas:
--   HTTP (db/tenants.rs): (id, tenant_id, request_count, token_count, created_at)
--   MCP (mcp/usage.rs):   (tenant_id, operation, tokens_used, effective_tokens, success, duration_ms, source, created_at)
--
-- Strategy: CREATE TABLE IF NOT EXISTS with the full unified schema (safe for fresh installs),
-- then ADD COLUMN IF NOT EXISTS for each new column (safe for existing installs — no data loss).
-- All created_at values are Unix timestamps (BIGINT).
--
-- Run: psql -U ares -d ares -f migrations/001_usage_events_unified.sql

-- Step 1: Create the table if it doesn't exist (full unified schema for fresh installs)
CREATE TABLE IF NOT EXISTS usage_events (
    id               TEXT    NOT NULL PRIMARY KEY,
    tenant_id        TEXT    NOT NULL,
    source           TEXT    NOT NULL DEFAULT 'http',   -- 'http' or 'mcp'
    request_count    BIGINT  NOT NULL DEFAULT 1,
    token_count      BIGINT  NOT NULL DEFAULT 0,
    operation        TEXT,
    tokens_used      BIGINT  NOT NULL DEFAULT 0,
    effective_tokens BIGINT  NOT NULL DEFAULT 0,
    success          BOOLEAN NOT NULL DEFAULT TRUE,
    duration_ms      BIGINT  NOT NULL DEFAULT 0,
    created_at       BIGINT  NOT NULL
);

-- Step 2: Add new columns if upgrading from old schema (idempotent — safe to re-run)
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS source           TEXT    NOT NULL DEFAULT 'http';
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS operation        TEXT;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS tokens_used      BIGINT  NOT NULL DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS effective_tokens BIGINT  NOT NULL DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS success          BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS duration_ms      BIGINT  NOT NULL DEFAULT 0;

-- Step 3: Indexes (IF NOT EXISTS — safe to re-run)
CREATE INDEX IF NOT EXISTS idx_usage_events_tenant_id  ON usage_events (tenant_id);
CREATE INDEX IF NOT EXISTS idx_usage_events_created_at ON usage_events (created_at);
CREATE INDEX IF NOT EXISTS idx_usage_events_source     ON usage_events (source);

-- Step 4: Aggregation cache tables (idempotent)
CREATE TABLE IF NOT EXISTS monthly_usage_cache (
    tenant_id     TEXT   NOT NULL,
    usage_month   BIGINT NOT NULL,
    request_count BIGINT NOT NULL DEFAULT 0,
    token_count   BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, usage_month)
);

CREATE TABLE IF NOT EXISTS daily_rate_limits (
    tenant_id     TEXT   NOT NULL,
    usage_date    BIGINT NOT NULL,
    request_count BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, usage_date)
);
