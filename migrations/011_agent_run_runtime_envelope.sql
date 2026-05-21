-- Migration 011: Generic runtime provenance on agent_runs
--
-- This keeps the schema product-neutral while letting managed runtimes
-- persist tenant/workspace/session/config provenance for each run.

ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS workspace_id TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS session_id TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS request_source TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS product TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS agent_config_source TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS agent_config_version TEXT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS eruka_binding_id TEXT;

UPDATE agent_runs
SET request_source = 'unknown'
WHERE request_source IS NULL;

CREATE INDEX IF NOT EXISTS idx_agent_runs_tenant_workspace
    ON agent_runs (tenant_id, workspace_id)
    WHERE workspace_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_runs_session
    ON agent_runs (session_id)
    WHERE session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_runs_request_source
    ON agent_runs (request_source)
    WHERE request_source IS NOT NULL;
