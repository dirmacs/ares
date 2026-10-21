-- Append-only audit of tenant pause state changes.
--
-- `tenants.paused_by` records the *current* actor and is cleared when the tenant
-- is unpaused, so on its own it cannot answer "who stopped this tenant, and
-- when?" after the fact — which is exactly the question an incident review asks.
-- This table can: one row per change, never updated, never deleted.
--
-- Kept separate from `20261016000014_tenant_flags` rather than appended to
-- it, so that migration's checksum stays stable for databases that have
-- already applied it. The attribution columns it needs
-- (`paused_by`, `paused_at`) land in `20261016000016_tenant_pause_attribution`,
-- which runs after it.

CREATE TABLE IF NOT EXISTS tenant_pause_audit (
    id         BIGSERIAL PRIMARY KEY,
    tenant_id  TEXT    NOT NULL,
    paused     BOOLEAN NOT NULL,
    actor      TEXT    NOT NULL,
    changed_at BIGINT  NOT NULL
);

-- Read pattern is "what happened to this tenant, most recent first".
CREATE INDEX IF NOT EXISTS idx_tenant_pause_audit_tenant
    ON tenant_pause_audit(tenant_id, changed_at DESC);