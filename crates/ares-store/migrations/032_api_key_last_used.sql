-- Migration 032: API key last-used heartbeat.
--
-- Additive only, safe to apply under a running server (`IF NOT EXISTS`).
-- - `api_keys.last_used_at` records the last successful `verify_api_key`
--   (unix seconds, BIGINT like the rest of the schema). NULL means never used.
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS last_used_at BIGINT;
