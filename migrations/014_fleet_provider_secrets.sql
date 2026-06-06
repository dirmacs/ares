-- Migration 014: Fleet Provider Secrets
-- Encrypted-at-rest storage for fleet-wide, tenant-agnostic provider API keys
-- and configuration overrides. Decrypted into memory at startup; hot-swap
-- via Arc<ArcSwap<...>> on every admin write.
--
-- The master key is derived from $FLEET_SECRETS_KEY (SHA-256) and lives in
-- /etc/dirmacs/fleet-secrets.env on the DIRMACS VPS. OSS deployments without
-- the env var simply don't read from this table.
--
-- Schema:
--   provider_name   Unique name; matches the ProviderConfig key in ares.toml.
--   ciphertext      AES-256-GCM ciphertext of the API key (or NULL when
--                   only api_base/default_model is overridden and no key).
--   nonce           12-byte AES-GCM nonce paired with ciphertext.
--   api_base        Optional override for the provider base URL.
--   default_model   Optional override for the provider's default model.
--   has_api_key     Boolean: true if ciphertext is set. Lets the UI render
--                   a "Set / Not set" badge without exposing the ciphertext.
--   updated_at      Unix seconds; set on every write.
--   updated_by      Admin identity (from JWT) that last wrote the row.

CREATE TABLE IF NOT EXISTS fleet_provider_secrets (
    id              TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    provider_name   TEXT NOT NULL UNIQUE,
    ciphertext      BYTEA,
    nonce           BYTEA,
    api_base        TEXT,
    default_model   TEXT,
    has_api_key     BOOLEAN NOT NULL DEFAULT false,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_fps_provider_name ON fleet_provider_secrets(provider_name);

-- Defense in depth: ciphertext + nonce must be both null or both non-null.
ALTER TABLE fleet_provider_secrets
    DROP CONSTRAINT IF EXISTS fps_ciphertext_nonce_pair;
ALTER TABLE fleet_provider_secrets
    ADD CONSTRAINT fps_ciphertext_nonce_pair
    CHECK (
        (ciphertext IS NULL AND nonce IS NULL)
        OR (ciphertext IS NOT NULL AND nonce IS NOT NULL)
    );

ALTER TABLE fleet_provider_secrets
    DROP CONSTRAINT IF EXISTS fps_key_means_has_api_key;
ALTER TABLE fleet_provider_secrets
    ADD CONSTRAINT fps_key_means_has_api_key
    CHECK (NOT has_api_key OR (ciphertext IS NOT NULL AND nonce IS NOT NULL));
