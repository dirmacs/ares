-- Row 7: retention on tenant end-user records. NULL keeps the row until the
-- tenant purge path deletes it; a non-NULL value marks the row for the sweep.
ALTER TABLE tenant_users
    ADD COLUMN IF NOT EXISTS purge_after TIMESTAMPTZ;
CREATE INDEX IF NOT EXISTS idx_tenant_users_purge_after
    ON tenant_users(purge_after)
    WHERE purge_after IS NOT NULL;
