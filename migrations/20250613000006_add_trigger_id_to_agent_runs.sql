ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS trigger_id TEXT;
CREATE INDEX IF NOT EXISTS idx_agent_runs_trigger_id ON agent_runs(trigger_id) WHERE trigger_id IS NOT NULL;
