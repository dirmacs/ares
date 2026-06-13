-- Migration 027: Add grace_period_seconds to agent_schedules
-- Supports DIR1-73: Configurable missed-run grace period

ALTER TABLE agent_schedules 
ADD COLUMN IF NOT EXISTS grace_period_seconds INTEGER NOT NULL DEFAULT 120;