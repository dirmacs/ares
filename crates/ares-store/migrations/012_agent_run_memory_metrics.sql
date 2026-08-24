-- Migration 012: Memory activity metrics on agent_runs
-- Adds explicit Eruka context telemetry for Fleet metrics without changing
-- existing run semantics.

ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS eruka_context_hit BOOLEAN DEFAULT FALSE;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS eruka_read_count BIGINT DEFAULT 0;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS eruka_write_count BIGINT DEFAULT 0;
