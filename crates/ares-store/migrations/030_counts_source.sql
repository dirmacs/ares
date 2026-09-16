-- Migration 030: counts_source on usage_events (metering truth).
--
-- Adds nullable `counts_source` TEXT to label whether token counts were
-- provider-reported, locally estimated, or unknown (ingest path).
-- Old rows stay NULL (no backfill). New writers bind:
--   pipeline/scheduler/trigger/http -> 'reported' when LLM usage present,
--   otherwise 'estimated'; ingest -> 'unknown'.
-- Additive only, safe to apply under a running server.

ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS counts_source TEXT;
