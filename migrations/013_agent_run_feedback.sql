-- Migration 013: Reviewer and quality feedback for agent runs
--
-- Stores human/operator feedback against individual runs while also allowing
-- aggregate per-agent quality summaries.

CREATE TABLE IF NOT EXISTS agent_run_feedback (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    run_id TEXT,
    feedback_type TEXT NOT NULL,
    score DOUBLE PRECISION,
    flags TEXT[] NOT NULL DEFAULT '{}',
    notes TEXT,
    reviewer TEXT,
    created_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_run_feedback_tenant_agent
    ON agent_run_feedback (tenant_id, agent_name, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_agent_run_feedback_run
    ON agent_run_feedback (run_id)
    WHERE run_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_run_feedback_type
    ON agent_run_feedback (feedback_type);
