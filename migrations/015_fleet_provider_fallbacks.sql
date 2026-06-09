-- Migration 015: Add fallback_providers to fleet_provider_secrets
-- Stores an ordered list of fallback provider names as JSONB.

ALTER TABLE fleet_provider_secrets
    ADD COLUMN IF NOT EXISTS fallback_providers JSONB;
