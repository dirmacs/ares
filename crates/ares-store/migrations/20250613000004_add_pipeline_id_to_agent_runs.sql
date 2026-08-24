-- Migration 20250613000004: Add pipeline_id to agent_runs
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS pipeline_id TEXT;