-- External usage ingest for library-embedded callers (POST /v1/usage/events).
-- Additive only: existing writers omit these columns (all nullable), the
-- ingest handler always sets them. Safe to apply under a running server.
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS request_id TEXT;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS outcome_class TEXT;
ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS reason_code TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS uq_usage_events_tenant_request
    ON usage_events (tenant_id, request_id) WHERE request_id IS NOT NULL;
