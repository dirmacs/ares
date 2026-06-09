CREATE TABLE IF NOT EXISTS agent_templates (
    id              TEXT    PRIMARY KEY DEFAULT gen_random_uuid()::text,
    product_type    TEXT    NOT NULL,
    agent_name      TEXT    NOT NULL,
    display_name    TEXT    NOT NULL,
    description     TEXT,
    config          JSONB   NOT NULL,
    created_at      BIGINT  NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_templates_name ON agent_templates(agent_name);
