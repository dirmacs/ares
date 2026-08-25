CREATE TABLE IF NOT EXISTS conversation_snapshots (
    session_id     TEXT PRIMARY KEY,
    entries        JSONB NOT NULL DEFAULT '[]',
    critical       JSONB NOT NULL DEFAULT '[]',
    memory         TEXT NOT NULL DEFAULT '',
    last_audit_seq BIGINT NOT NULL DEFAULT 0,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
