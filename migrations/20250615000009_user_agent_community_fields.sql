-- Align `user_agents` with the `ares_store::postgres::UserAgent` struct.
-- The struct gained community-sharing and rating fields but no migration
-- was added; `SELECT *` + `sqlx::FromRow` in the agent resolver fails with
-- "column is_public does not exist" on any database built from migrations.

ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS display_name TEXT;
ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS description TEXT;
ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS extra TEXT NOT NULL DEFAULT '{}';
ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS usage_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS rating_sum INTEGER NOT NULL DEFAULT 0;
ALTER TABLE user_agents ADD COLUMN IF NOT EXISTS rating_count INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_user_agents_is_public ON user_agents(is_public) WHERE is_public = true;
