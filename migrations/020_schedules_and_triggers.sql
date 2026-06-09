-- Cron schedules for agents
CREATE TABLE IF NOT EXISTS agent_schedules (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id       TEXT    NOT NULL,
    agent_name      TEXT    NOT NULL,
    cron_expression TEXT    NOT NULL,  -- e.g. '0 9 * * *' for daily at 9am
    timezone        TEXT    NOT NULL DEFAULT 'UTC',
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    last_run_at     BIGINT,
    next_run_at     BIGINT,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_schedules_tenant ON agent_schedules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_schedules_next_run ON agent_schedules(next_run_at) WHERE enabled = TRUE;

-- Event triggers
CREATE TABLE IF NOT EXISTS event_triggers (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id       TEXT    NOT NULL,
    name            TEXT    NOT NULL,
    event_type      TEXT    NOT NULL, -- 'webhook', 'document_upload', 'field_change', 'agent_complete'
    event_config    JSONB   NOT NULL, -- {path: '/webhook/abc', method: 'POST', ...}
    target_agent    TEXT    NOT NULL,
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_event_triggers_tenant ON event_triggers(tenant_id);

-- Inter-agent pipeline links
CREATE TABLE IF NOT EXISTS agent_pipelines (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    tenant_id       TEXT    NOT NULL,
    source_agent    TEXT    NOT NULL,
    target_agent    TEXT    NOT NULL,
    condition       TEXT,              -- optional filter expression
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_pipelines_link ON agent_pipelines(tenant_id, source_agent, target_agent);
