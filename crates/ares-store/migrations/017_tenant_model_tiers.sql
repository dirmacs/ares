-- Per-tenant model tier mapping
-- Maps abstract tiers (powerful, fast, cheap) to concrete models per tenant
CREATE TABLE IF NOT EXISTS tenant_model_tiers (
    tenant_id       TEXT    NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    tier_name       TEXT    NOT NULL,              -- 'powerful', 'fast', 'cheap'
    provider_name   TEXT    NOT NULL,              -- provider to use
    model_name      TEXT    NOT NULL,              -- concrete model id
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL,
    PRIMARY KEY (tenant_id, tier_name)
);

CREATE INDEX IF NOT EXISTS idx_tenant_model_tiers_tenant ON tenant_model_tiers(tenant_id);
