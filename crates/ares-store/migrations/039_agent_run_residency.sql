-- 039: where each agent run went (item 2.12b; DELEGATED ruling 2026-09-29, 2.12 migrations).
--
--   resolved_endpoint  the endpoint of the provider that ANSWERED the run, reduced to scheme, host,
--                      port and path (no userinfo, no query, no fragment, no trailing slash).
--   region             the region of that provider where it has one (Bedrock, Vertex), else NULL.
--
-- The server writes both from the resolved provider, never from a request field, and neither holds
-- a key or a secret. `region` is visibility only: it decides nothing about where a run may go.
--
-- What it does to production: two nullable TEXT columns on agent_runs, no default, no backfill.
-- Postgres adds such a column by changing the catalog only; it does not rewrite the table. The
-- ALTER takes ACCESS EXCLUSIVE on agent_runs for the moment it needs to change the catalog, and
-- nothing else. Every row that exists reads NULL for both columns. Nothing changes for any
-- reader or writer that does not name them.
--
-- Reversal (no .down files exist in this directory; run by hand, and only after the code that
-- writes the columns has been rolled back):
--   ALTER TABLE agent_runs DROP COLUMN IF EXISTS resolved_endpoint, DROP COLUMN IF EXISTS region;
ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS resolved_endpoint TEXT,
    ADD COLUMN IF NOT EXISTS region TEXT;
