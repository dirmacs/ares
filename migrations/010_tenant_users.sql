-- Migration 010: Tenant Users (Population 2)
-- End-users of client products (per-client user populations).
-- These are NOT DIRMACS users — they cannot access Eruka/admin/portal.
-- Managed by dirmacs-core on behalf of clients.

CREATE TABLE IF NOT EXISTS tenant_users (
    id                  TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id           TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- Managed mode: email identifies the user
    email               TEXT,
    -- BYO mode: client's own user ID
    external_user_id    TEXT,
    -- Display name
    name                TEXT,
    -- Role within the client's product
    role                TEXT NOT NULL DEFAULT 'user',
    -- Eruka sub-workspace for this user's context
    workspace_id        TEXT,
    -- Client's own tier for this user (free, premium, etc.)
    tier                TEXT DEFAULT 'free',
    -- Client-specific metadata (JSON)
    metadata            JSONB DEFAULT '{}',
    -- Password hash (argon2, for managed mode only — NULL for BYO)
    password_hash       TEXT,
    -- Timestamps
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Constraints: unique email per tenant, unique external_id per tenant
    UNIQUE(tenant_id, email),
    UNIQUE(tenant_id, external_user_id)
);

-- Fast lookups
CREATE INDEX IF NOT EXISTS idx_tenant_users_tenant ON tenant_users(tenant_id);
CREATE INDEX IF NOT EXISTS idx_tenant_users_email ON tenant_users(tenant_id, email);
CREATE INDEX IF NOT EXISTS idx_tenant_users_external ON tenant_users(tenant_id, external_user_id);
CREATE INDEX IF NOT EXISTS idx_tenant_users_workspace ON tenant_users(workspace_id);
