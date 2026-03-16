-- Migration 000: Core tenants and api_keys tables
--
-- These tables were missing from migrations — the code always assumed they existed.
-- Run BEFORE migrations 001, 002, 003.
--
-- Run: psql -U dirmacs -d ares -f migrations/000_tenants.sql

CREATE TABLE IF NOT EXISTS tenants (
    id          TEXT    NOT NULL PRIMARY KEY,
    name        TEXT    NOT NULL,
    tier        TEXT    NOT NULL DEFAULT 'free',
    created_at  BIGINT  NOT NULL,
    updated_at  BIGINT  NOT NULL
);

CREATE TABLE IF NOT EXISTS api_keys (
    id          TEXT    NOT NULL PRIMARY KEY,
    tenant_id   TEXT    NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    key_hash    TEXT    NOT NULL,
    key_prefix  TEXT    NOT NULL UNIQUE,
    name        TEXT    NOT NULL DEFAULT 'default',
    is_active   INTEGER NOT NULL DEFAULT 1,
    created_at  BIGINT  NOT NULL,
    expires_at  BIGINT
);

CREATE INDEX IF NOT EXISTS idx_api_keys_tenant_id ON api_keys(tenant_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_key_prefix ON api_keys(key_prefix);
CREATE INDEX IF NOT EXISTS idx_tenants_name ON tenants(name);
