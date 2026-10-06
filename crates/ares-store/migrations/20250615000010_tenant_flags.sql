-- Per-tenant pause switch: stop one tenant's runs without touching anyone
-- else's, alongside the existing global emergency stop.
--
-- ARES has one global emergency stop today. This adds a per-tenant flag so one
-- client's runs can be stopped without touching anyone else's, plus a `strict`
-- column added now because a later item (2.9) needs it. Only the columns land
-- here; `strict` behaviour is out of scope for this item.
--
-- `paused_by` / `paused_at` record who did it and when, so the action is
-- attributable rather than anonymous.

ALTER TABLE tenants ADD COLUMN IF NOT EXISTS paused BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS strict BOOLEAN NOT NULL DEFAULT false;

-- Attribution: NULL whenever the tenant is not paused.
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS paused_by TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS paused_at BIGINT;

-- Existing tenants default to not paused, and nothing else about them changes.
CREATE INDEX IF NOT EXISTS idx_tenants_paused ON tenants(paused) WHERE paused = true;
