-- Admin audit log: tracks all admin mutations for compliance
CREATE TABLE IF NOT EXISTS admin_audit_log (
    id            TEXT   PRIMARY KEY,
    action        TEXT   NOT NULL,     -- 'create_tenant', 'update_quota', 'create_api_key', 'delete_agent', etc.
    resource_type TEXT   NOT NULL,     -- 'tenant', 'agent', 'api_key', 'alert'
    resource_id   TEXT   NOT NULL,
    details       TEXT,                -- JSON string with extra context
    admin_ip      TEXT,
    created_at    BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_log_created ON admin_audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_log_resource ON admin_audit_log(resource_type, resource_id);
