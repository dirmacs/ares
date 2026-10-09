-- Migration 20261009000010: agent run tree (parent_run_id / root_run_id).
--
-- When one agent calls another, the specialist run gets its own agent_runs
-- row. These two columns record which run called which: `parent_run_id` is
-- the run that created this one, `root_run_id` is the top of the tree. Both
-- are NULL for an ordinary top-level run.
--
-- No foreign keys: a child may be written before its parent finishes, and
-- agent_runs rows are deleted per tenant, which a FK would refuse.

ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS parent_run_id TEXT;

ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS root_run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_runs_root_run_id
    ON agent_runs (root_run_id)
    WHERE root_run_id IS NOT NULL;
