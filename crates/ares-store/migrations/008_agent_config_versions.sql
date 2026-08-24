-- Migration 008: Agent Config Versions
-- Tracks every TOON agent config version loaded at startup/hot-reload.
-- Enables rollback (Sprint 12) and kill switch.

CREATE TABLE IF NOT EXISTS agent_config_versions (
    id              TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text,
    agent_id        TEXT NOT NULL,        -- TOON config name/slug
    version         TEXT NOT NULL,        -- semver from config.version field
    config_json     JSONB NOT NULL,       -- full serialized ToonAgentConfig (for rollback)
    is_active       BOOLEAN NOT NULL DEFAULT true,
    change_source   TEXT NOT NULL DEFAULT 'startup', -- 'startup', 'hot_reload', 'api'
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (agent_id, version)
);

CREATE INDEX IF NOT EXISTS idx_acv_agent_id ON agent_config_versions(agent_id);
CREATE INDEX IF NOT EXISTS idx_acv_active   ON agent_config_versions(agent_id, is_active);
