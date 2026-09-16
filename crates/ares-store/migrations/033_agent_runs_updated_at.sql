-- Migration 033: agent_runs updated_at heartbeat.
--
-- Additive only, safe to apply under a running server (`IF NOT EXISTS`).
-- - `agent_runs.updated_at` mirrors `created_at` on insert and advances on
--   every status-transition UPDATE (store fns, sweeper reap, v1 trace UPDATEs).
--   NULL means a pre-033 row not yet backfilled; backfill from created_at.
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS updated_at BIGINT;
UPDATE agent_runs SET updated_at = created_at WHERE updated_at IS NULL;
