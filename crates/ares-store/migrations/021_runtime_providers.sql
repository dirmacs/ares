CREATE TABLE IF NOT EXISTS runtime_providers (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id       TEXT,                               -- NULL = fleet-wide
    name            TEXT    NOT NULL,                   -- e.g. "azure_openai", "bedrock"
    display_name    TEXT    NOT NULL,
    provider_type   TEXT    NOT NULL,                   -- "openai-compatible", "anthropic-compatible", "custom"
    api_base        TEXT    NOT NULL,
    auth_type       TEXT    NOT NULL,                   -- "api_key", "oauth2", "aws_sigv4"
    default_model   TEXT,
    headers         JSONB,                              -- extra headers
    request_transform JSONB,                            -- path template, body remap
    response_transform JSONB,                           -- body remap
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_providers_name ON runtime_providers(name);
