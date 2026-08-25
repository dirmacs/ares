-- Telemetry chain completion: persist provider-side prompt-cache hits and
-- whole-call wall time on `run_llm_calls`.
--
-- `LlmCallRecord`/`TokenUsage` already carry `cached_tokens` and
-- `total_time_ms`, but the columns were missing here so the values died at
-- the sink. Both are nullable BIGINTs: providers that do not report cache
-- hits leave them NULL, and NULL-safe aggregates power the admin
-- /admin/stats/cache-hits endpoint.

ALTER TABLE run_llm_calls ADD COLUMN IF NOT EXISTS cached_tokens BIGINT;
ALTER TABLE run_llm_calls ADD COLUMN IF NOT EXISTS total_time_ms BIGINT;
