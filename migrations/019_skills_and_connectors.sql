-- Executable skills table
CREATE TABLE IF NOT EXISTS skills (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id       TEXT    NOT NULL,
    name            TEXT    NOT NULL,
    display_name    TEXT    NOT NULL,
    description     TEXT,
    skill_type      TEXT    NOT NULL DEFAULT 'workflow', -- 'workflow', 'connector', 'composite'
    steps           JSONB   NOT NULL, -- array of {type: 'tool_call'|'llm_call'|'condition', ...}
    input_schema    JSONB,             -- JSON schema for inputs
    output_schema   JSONB,             -- JSON schema for outputs
    tools           TEXT[],            -- required tools
    is_public       BOOLEAN NOT NULL DEFAULT FALSE,
    created_by      TEXT,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_skills_tenant ON skills(tenant_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_skills_name_tenant ON skills(tenant_id, name);

-- Connector configurations (special tool type for external services)
CREATE TABLE IF NOT EXISTS connectors (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id       TEXT    NOT NULL,
    name            TEXT    NOT NULL,
    service_type    TEXT    NOT NULL, -- 'google_drive', 'slack', 'linkedin', 'hubspot', 'salesforce', 'email', 'custom'
    auth_config     JSONB   NOT NULL, -- {type: 'oauth2'|'api_key', ...}
    endpoints       JSONB   NOT NULL, -- array of {name, method, url, headers, body_template}
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_connectors_tenant ON connectors(tenant_id);
