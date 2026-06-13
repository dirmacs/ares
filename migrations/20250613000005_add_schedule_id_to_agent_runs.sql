ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS schedule_id TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_runs_schedule_id
    ON agent_runs (schedule_id)
    WHERE schedule_id IS NOT NULL;
