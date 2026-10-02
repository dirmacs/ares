-- Migration 037: per-tenant flags `paused` and `strict` (item 2.5a).
--
-- What this does to production:
--   Adds two NOT NULL boolean columns to `tenants`, both DEFAULT false:
--     paused  the per-tenant kill switch. `ares_agent::admit` and the research
--             handler read it on every run; true refuses the run with a typed
--             `unavailable` before any model call. Nothing sets it yet except an
--             owner command; the admin toggle lands with item 2.5c.
--     strict  reserved for item 2.9. Nothing reads it in this migration's item.
--   Additive and idempotent (IF NOT EXISTS). PostgreSQL 11 and later store a
--   constant default as catalog metadata, so there is no table rewrite and the
--   ACCESS EXCLUSIVE lock is held only for the catalog change. Every existing
--   row reads false: no tenant's behaviour changes until `paused` is set.
--
-- Reversal (roll the binary back FIRST: a binary that reads `paused` fails its
-- read once the column is gone, and a failed read refuses the run):
--   ALTER TABLE tenants
--     DROP COLUMN IF EXISTS paused,
--     DROP COLUMN IF EXISTS strict;
ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS paused boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS strict boolean NOT NULL DEFAULT false;
