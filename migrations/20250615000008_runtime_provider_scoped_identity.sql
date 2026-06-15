DROP INDEX IF EXISTS idx_runtime_providers_name;
CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_providers_scope_name
    ON runtime_providers (COALESCE(tenant_id, ''), name);
