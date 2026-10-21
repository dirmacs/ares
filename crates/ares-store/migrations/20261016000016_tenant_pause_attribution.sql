-- Migration 20261016000016: pause attribution on `tenants` (companion to
-- 20261016000014, which adds `paused`/`strict`).
--
-- `paused` says a tenant is stopped; it cannot say who stopped it or when.
-- This adds the two columns that answer that, plus the partial index the
-- admission path uses to find paused tenants without scanning the table:
--   paused_by  TEXT, NULL whenever the tenant is not paused
--   paused_at  BIGINT (unix seconds), NULL whenever the tenant is not paused
--
-- Additive and idempotent (ADD COLUMN IF NOT EXISTS). PostgreSQL 11 and later
-- store a NULL-able column addition as catalog metadata, so there is no table
-- rewrite and the ACCESS EXCLUSIVE lock is held only for the catalog change.
-- Existing rows read NULL, which the store maps to "no actor recorded", so no
-- tenant's behaviour changes until it is paused.
--
-- Reversal (roll the binary back FIRST: `set_tenant_paused` writes these
-- columns, so a binary mid-write fails once they are gone):
--   ALTER TABLE tenants DROP COLUMN IF EXISTS paused_by, DROP COLUMN IF EXISTS paused_at;
--   DROP INDEX IF EXISTS idx_tenants_paused;
--
-- Kept in its own migration rather than folded into 20261016000014 so that
-- migration's checksum stays stable for databases that have already applied
-- it. 20261016000015 (the pause audit table) is a different object and does
-- not depend on these columns.

ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS paused_by TEXT,
    ADD COLUMN IF NOT EXISTS paused_at BIGINT;

-- The admission path reads `paused`; the index keeps that a catalog lookup
-- rather than a sequential scan on a large tenants table.
CREATE INDEX IF NOT EXISTS idx_tenants_paused ON tenants(paused) WHERE paused = true;
