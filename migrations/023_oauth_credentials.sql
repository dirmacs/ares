-- Migration 023: OAuth Credentials
-- Encrypted-at-rest storage for per-tenant OAuth credentials.
--
-- The master key is derived from $FLEET_SECRETS_KEY (SHA-256).
-- client_secret, access_token, and refresh_token are encrypted with AES-256-GCM
-- and stored as ciphertext + nonce BYTEA pairs.

CREATE TABLE IF NOT EXISTS oauth_credentials (
    id                          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id                   TEXT NOT NULL,
    provider                    TEXT NOT NULL,
    connector_type              TEXT NOT NULL,
    client_id                   TEXT NOT NULL,
    client_secret_ciphertext    BYTEA NOT NULL,
    client_secret_nonce         BYTEA NOT NULL,
    access_token_ciphertext     BYTEA,
    access_token_nonce          BYTEA,
    refresh_token_ciphertext    BYTEA,
    refresh_token_nonce         BYTEA,
    expires_at                  BIGINT,
    scope                       TEXT,
    created_at                  BIGINT NOT NULL,
    updated_at                  BIGINT NOT NULL,
    UNIQUE(tenant_id, provider, connector_type)
);

CREATE INDEX IF NOT EXISTS idx_oauth_credentials_tenant_id ON oauth_credentials(tenant_id);

-- Defense in depth: ciphertext + nonce must be both null or both non-null.
ALTER TABLE oauth_credentials
    DROP CONSTRAINT IF EXISTS oauth_access_token_pair;
ALTER TABLE oauth_credentials
    ADD CONSTRAINT oauth_access_token_pair
    CHECK (
        (access_token_ciphertext IS NULL AND access_token_nonce IS NULL)
        OR (access_token_ciphertext IS NOT NULL AND access_token_nonce IS NOT NULL)
    );

ALTER TABLE oauth_credentials
    DROP CONSTRAINT IF EXISTS oauth_refresh_token_pair;
ALTER TABLE oauth_credentials
    ADD CONSTRAINT oauth_refresh_token_pair
    CHECK (
        (refresh_token_ciphertext IS NULL AND refresh_token_nonce IS NULL)
        OR (refresh_token_ciphertext IS NOT NULL AND refresh_token_nonce IS NOT NULL)
    );
