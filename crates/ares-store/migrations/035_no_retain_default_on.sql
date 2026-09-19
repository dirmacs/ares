-- Migration 035: no-retain default ON (row 31 ruling, 2026-09-19).
--
-- Trace content redacts by default. Retaining raw content becomes an
-- explicit, owner-approved opt-out per tenant. The flip aligns existing
-- rows and every future tenant created without an explicit flag.
--
-- Self-healing and idempotent: the ADD COLUMN mirrors migration 029 for
-- databases where 029 is recorded but the column drifted, and the SET
-- DEFAULT plus UPDATE are safe to re-run. Redaction affects future writes
-- only; stored bytes are untouched.
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS no_retain BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE tenants ALTER COLUMN no_retain SET DEFAULT TRUE;
UPDATE tenants SET no_retain = TRUE WHERE no_retain = FALSE;
