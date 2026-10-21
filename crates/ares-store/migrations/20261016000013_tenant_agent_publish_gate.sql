-- Migration 038 (item 2.6a): the draft -> publish gate for tenant agent
-- configs (ruling 2026-10-01-DELEGATED-sr-2.6-core-design, D-1 to D-3).
--
-- `config` stays what runs: the published config, the only column the run
-- path reads. An edit is written to `draft_config` and changes nothing that
-- runs until a second admin publishes it, which moves the draft into
-- `config` and sets `published_digest`. A row whose `published_digest` is
-- NULL was never published: the run path refuses it, and its `config`
-- holds the empty object, which names no model and no skill.
--
-- 037 is left free for items 2.5 and 2.9.

-- The digest of a tenant agent config (D-2): sha256 over the canonical
-- `jsonb` text, hex-encoded. This is its one definition: the cutover below,
-- every publish and every rollback call it, and no Rust code re-implements
-- it.
CREATE FUNCTION tenant_agent_config_digest(config JSONB) RETURNS TEXT
    LANGUAGE sql STABLE STRICT
    AS $$ SELECT encode(sha256(convert_to(config::text, 'UTF8')), 'hex') $$;

-- `draft_authors` is every actor who wrote the current draft since the last
-- publish (ruling section 3.2); `draft_by` is the last of them. TEXT[] rather
-- than JSONB: a flat set of actor ids, checked with `= ANY(..)` inside the
-- publish transaction, with no shape to validate.
ALTER TABLE tenant_agents
    ADD COLUMN draft_config JSONB,
    ADD COLUMN draft_by TEXT,
    ADD COLUMN draft_authors TEXT[],
    ADD COLUMN draft_at BIGINT,
    ADD COLUMN published_digest TEXT,
    ADD COLUMN published_by TEXT,
    ADD COLUMN approved_by TEXT,
    ADD COLUMN published_at BIGINT;

-- The cutover (D-3): every existing row, enabled or not, is published with
-- the digest of the config it runs today, so nothing that runs stops and a
-- re-enabled row needs no publish its content never had.
UPDATE tenant_agents
SET published_digest = tenant_agent_config_digest(config),
    published_by = 'cutover',
    approved_by = 'cutover',
    published_at = EXTRACT(EPOCH FROM now())::BIGINT;

-- Each cutover config gets a version record that carries its digest, so it
-- stays a version rollback can promote (D-6). The record has the shape the
-- store writes (`tenant_agent_snapshot`), under the store's key
-- `tenant:<tenant_id>:<agent_name>`, and becomes the active version.
UPDATE agent_config_versions
SET is_active = false
WHERE agent_id IN (
    SELECT 'tenant:' || tenant_id || ':' || agent_name FROM tenant_agents
);

INSERT INTO agent_config_versions (agent_id, version, config_json, is_active, change_source)
SELECT 'tenant:' || ta.tenant_id || ':' || ta.agent_name,
       'cutover',
       jsonb_build_object(
           'snapshot_type', 'tenant_agent',
           'runtime_config_version',
               COALESCE(NULLIF(btrim(ta.config ->> 'version'), ''), 'tenant-db:' || ta.updated_at),
           'published_digest', ta.published_digest,
           'published_by', 'cutover',
           'approved_by', 'cutover',
           'tenant_agent', jsonb_build_object(
               'id', ta.id,
               'tenant_id', ta.tenant_id,
               'agent_name', ta.agent_name,
               'display_name', ta.display_name,
               'description', ta.description,
               'config', ta.config,
               'enabled', ta.enabled,
               'created_at', ta.created_at,
               'updated_at', ta.updated_at
           )
       ),
       true,
       'cutover'
FROM tenant_agents ta;
