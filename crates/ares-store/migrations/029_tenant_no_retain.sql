-- Migration 029: Per-tenant no-retain flag for trace content (ST-3 / F-A11).
--
-- Adds `tenants.no_retain` (default false). When true, trace writers
-- (`run_llm_calls`, `run_tool_calls`) and the `agent_runs.error` close-out
-- persist a redaction marker instead of raw content. Row ids, keying, token
-- counts, latency and status stay intact so cost aggregation is unaffected.
--
-- Additive only: existing rows default to false (retain, byte-identical to
-- today). Enabling a real tenant is owner SQL at deploy, never this slice:
--   UPDATE tenants SET no_retain = TRUE WHERE id = '<tenant_id>';
-- Safe to apply under a running server.

ALTER TABLE tenants ADD COLUMN IF NOT EXISTS no_retain BOOLEAN NOT NULL DEFAULT FALSE;
