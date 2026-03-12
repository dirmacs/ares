-- Agent templates: default configs per product type, seeded by application
CREATE TABLE IF NOT EXISTS agent_templates (
    id TEXT PRIMARY KEY,
    product_type TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    display_name TEXT NOT NULL,
    description TEXT,
    config JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE(product_type, agent_name)
);

-- Per-tenant agent instances (cloned from templates, editable per tenant)
CREATE TABLE IF NOT EXISTS tenant_agents (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    display_name TEXT NOT NULL,
    description TEXT,
    config JSONB NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE(tenant_id, agent_name)
);

CREATE INDEX IF NOT EXISTS idx_tenant_agents_tenant ON tenant_agents(tenant_id);
CREATE INDEX IF NOT EXISTS idx_agent_templates_product ON agent_templates(product_type);
