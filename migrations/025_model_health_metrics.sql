-- Migration 025: Model health metrics table
-- Supports DIR1-72: Per-model aggregation UI panel

CREATE TABLE IF NOT EXISTS model_health_metrics (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    model TEXT NOT NULL,
    period_start BIGINT NOT NULL,
    period_end BIGINT NOT NULL,
    total_calls BIGINT NOT NULL DEFAULT 0,
    successful_calls BIGINT NOT NULL DEFAULT 0,
    failed_calls BIGINT NOT NULL DEFAULT 0,
    avg_latency_ms BIGINT NOT NULL DEFAULT 0,
    p50_latency_ms BIGINT NOT NULL DEFAULT 0,
    p95_latency_ms BIGINT NOT NULL DEFAULT 0,
    p99_latency_ms BIGINT NOT NULL DEFAULT 0,
    total_tokens BIGINT NOT NULL DEFAULT 0,
    total_cost_usd DECIMAL(16, 8) NOT NULL DEFAULT 0,
    error_rate_pct DECIMAL(5, 2) NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    UNIQUE (tenant_id, model, period_start, period_end)
);

CREATE INDEX idx_model_health_tenant ON model_health_metrics(tenant_id);
CREATE INDEX idx_model_health_period ON model_health_metrics(period_start DESC);