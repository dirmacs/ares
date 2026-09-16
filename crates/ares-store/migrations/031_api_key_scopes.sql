-- Migration 031: API key scopes + per-key usage attribution.
--
-- Additive only, safe to apply under a running server (`IF NOT EXISTS`).
-- - `api_keys.scopes` carries the least-privilege scope for the key.
--   Starter vocabulary: `full` (default, byte-identical behavior) and
--   `ingest` (may only call POST /v1/usage/events). Unknown values
--   normalize to `full` in code; old rows default to `full`.
-- - `usage_events.api_key_id` attributes each metered/ingested row to the
--   key that authenticated it. Nullable so old rows stay NULL (no backfill)
--   and non-key writers (scheduler/trigger/pipeline) leave it NULL.
ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS scopes TEXT NOT NULL DEFAULT 'full';
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS api_key_id TEXT;
CREATE INDEX IF NOT EXISTS idx_usage_events_api_key_id ON usage_events (api_key_id);
