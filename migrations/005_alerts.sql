-- Platform alerts: system health, quota warnings, error rate spikes
CREATE TABLE IF NOT EXISTS alerts (
    id          TEXT    PRIMARY KEY,
    severity    TEXT    NOT NULL DEFAULT 'info',  -- critical, warning, info
    source      TEXT    NOT NULL,                 -- 'fleet', 'quota', 'agent', 'system'
    title       TEXT    NOT NULL,
    message     TEXT    NOT NULL,
    resolved    BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  BIGINT  NOT NULL,
    resolved_at BIGINT,
    resolved_by TEXT
);
CREATE INDEX IF NOT EXISTS idx_alerts_resolved ON alerts(resolved);
CREATE INDEX IF NOT EXISTS idx_alerts_severity ON alerts(severity);
CREATE INDEX IF NOT EXISTS idx_alerts_created  ON alerts(created_at);
