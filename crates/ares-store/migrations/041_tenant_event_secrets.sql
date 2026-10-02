-- Migration 041: per-tenant event-webhook secrets (item 2.16a).
--
-- One row per tenant holds the SHA-256 (lowercase hex) of that tenant's event
-- secret. ONLY THE HASH IS STORED: the secret itself is never written here,
-- logged or returned. A request to /api/events/document-upload,
-- /api/events/field-change or /api/webhooks/{trigger_id} presents the secret in
-- `X-Webhook-Secret`; the tenant is the tenant of the row whose hash matches,
-- and the request body never names the tenant.
--
-- - `tenant_id` is the primary key: one secret per tenant.
-- - `secret_sha256` is unique: no two tenants can share a secret, so a secret
--   identifies exactly one tenant. The CHECK admits only a lowercase hex
--   SHA-256 digest, so a plaintext secret pasted here by mistake is refused.
-- - `created_at` / `rotated_at` are unix seconds, like the neighbouring tables;
--   `rotated_at` stays NULL until a secret is replaced.
--
-- Additive only: one new table, no existing table or row changes, so it is
-- safe to apply under a running server. The table starts empty and nothing
-- reads it until the 2.16a handlers ship; every row is provisioned by the
-- owner (the item's deploy row).
--
-- Reversal: DROP TABLE IF EXISTS tenant_event_secrets;
-- Revert the 2.16a handlers first or together with it: handlers that find the
-- table missing fail closed (HTTP 500 for every webhook request, nothing runs).
CREATE TABLE IF NOT EXISTS tenant_event_secrets (
    tenant_id     TEXT   NOT NULL PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    secret_sha256 TEXT   NOT NULL UNIQUE CHECK (secret_sha256 ~ '^[0-9a-f]{64}$'),
    created_at    BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM now())::BIGINT),
    rotated_at    BIGINT
);
