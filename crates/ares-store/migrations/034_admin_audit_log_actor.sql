-- Migration 034: Record who performed each admin action.
--
-- Adds `admin_audit_log.actor` (nullable). Admin requests authenticated with
-- a JWT store the token `sub` claim. Requests authenticated with the static
-- `X-Admin-Secret` header store the literal `admin_secret`, because no user
-- identity exists on that path. Rows written before this migration keep NULL.
--
-- Additive only: no existing column or row changes, so the migration is safe
-- to apply under a running server.

ALTER TABLE admin_audit_log ADD COLUMN IF NOT EXISTS actor TEXT NULL;
