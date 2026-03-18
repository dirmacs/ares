-- Migration 009: Metering v2 - enrich agent_runs and usage_events for production billing
--
-- This migration adds model and provider tracking to support detailed billing.
-- All changes are idempotent to support safe re-runs.

-- Step 1: Add columns to agent_runs table
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS model_name TEXT DEFAULT 'unknown';
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS provider_name TEXT DEFAULT 'unknown';
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS is_streaming BOOLEAN DEFAULT FALSE;

-- Step 2: Add columns to usage_events table
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS input_tokens BIGINT DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS output_tokens BIGINT DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS model_name TEXT;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS agent_name TEXT;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS provider_name TEXT;

-- Step 3: Create indexes for efficient querying by model and agent
CREATE INDEX IF NOT EXISTS idx_agent_runs_model ON agent_runs(model_name);
CREATE INDEX IF NOT EXISTS idx_usage_events_model ON usage_events(model_name);
CREATE INDEX IF NOT EXISTS idx_usage_events_agent ON usage_events(agent_name);