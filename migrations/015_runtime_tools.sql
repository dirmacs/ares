-- Migration 015: Runtime-defined Tools
-- Allows operators to create tools via API/UI without code changes

CREATE TABLE IF NOT EXISTS runtime_tools (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL UNIQUE,
    display_name VARCHAR(255),
    description TEXT NOT NULL,
    tool_type VARCHAR(50) NOT NULL CHECK (tool_type IN ('http', 'mcp', 'script', 'sql')),
    parameters_schema JSONB NOT NULL,
    execution_config JSONB NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    version INTEGER NOT NULL DEFAULT 1,
    is_public BOOLEAN NOT NULL DEFAULT false,
    created_by UUID REFERENCES tenants(id),
    tenant_id UUID REFERENCES tenants(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for tenant-scoped queries
CREATE INDEX idx_runtime_tools_tenant ON runtime_tools(tenant_id);
CREATE INDEX idx_runtime_tools_public ON runtime_tools(is_public) WHERE is_public = true;
CREATE INDEX idx_runtime_tools_enabled ON runtime_tools(enabled) WHERE enabled = true;

-- Tool versions for rollback/history
CREATE TABLE IF NOT EXISTS runtime_tool_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tool_id UUID NOT NULL REFERENCES runtime_tools(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    parameters_schema JSONB NOT NULL,
    execution_config JSONB NOT NULL,
    description TEXT,
    changed_by UUID REFERENCES tenants(id),
    change_summary TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_runtime_tool_versions_tool ON runtime_tool_versions(tool_id);
CREATE UNIQUE INDEX idx_runtime_tool_versions_unique ON runtime_tool_versions(tool_id, version);

-- Tool execution logs for debugging
CREATE TABLE IF NOT EXISTS runtime_tool_executions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tool_id UUID NOT NULL REFERENCES runtime_tools(id) ON DELETE CASCADE,
    tenant_id UUID REFERENCES tenants(id),
    agent_run_id UUID REFERENCES agent_runs(id) ON DELETE SET NULL,
    input_args JSONB NOT NULL,
    output_result JSONB,
    status VARCHAR(20) NOT NULL CHECK (status IN ('success', 'error', 'timeout')),
    error_message TEXT,
    duration_ms BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_runtime_tool_executions_tool ON runtime_tool_executions(tool_id);
CREATE INDEX idx_runtime_tool_executions_tenant ON runtime_tool_executions(tenant_id);
CREATE INDEX idx_runtime_tool_executions_agent_run ON runtime_tool_executions(agent_run_id);
CREATE INDEX idx_runtime_tool_executions_created ON runtime_tool_executions(created_at DESC);

-- Updated_at trigger for runtime_tools
CREATE TRIGGER update_runtime_tools_updated_at
    BEFORE UPDATE ON runtime_tools
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();