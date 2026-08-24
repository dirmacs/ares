CREATE TABLE IF NOT EXISTS tenant_tool_allowlist (
    id          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id   TEXT NOT NULL,
    tool_name   TEXT NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    UNIQUE(tenant_id, tool_name)
);
CREATE INDEX idx_tenant_tool_allowlist ON tenant_tool_allowlist(tenant_id);

CREATE TABLE IF NOT EXISTS tenant_model_allowlist (
    id          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id   TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    UNIQUE(tenant_id, model_id)
);
CREATE INDEX idx_tenant_model_allowlist ON tenant_model_allowlist(tenant_id);

CREATE TABLE IF NOT EXISTS tenant_rag_allowlist (
    id          TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id   TEXT NOT NULL,
    rag_source  TEXT NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    UNIQUE(tenant_id, rag_source)
);
CREATE INDEX idx_tenant_rag_allowlist ON tenant_rag_allowlist(tenant_id);
