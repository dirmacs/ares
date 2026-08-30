# Operations

This chapter covers configuration, supervision, observability, security posture, CI gates, and backup notes for running an ARES server. All keys come from the config structs cited per section.

## Configuration file

The server reads `ares.toml` from the working directory. `ares-server --config my-config.toml` selects another file. `ares-server init` scaffolds a project with `ares.toml`, `.env.example`, and a `config/` directory tree (`src/cli/init.rs`). The root schema is `AresConfig` (`crates/ares-http/src/overlay.rs`). Check any config file before deploy with `ares-server config --validate`. The `[auth]` and `[database]` tables are required, but an empty table satisfies them because every field inside carries a default.

### `[server]` group

`ServerConfig` (`crates/ares-http/src/config.rs`):

| Field | Default | Meaning |
|---|---|---|
| `host` | `"127.0.0.1"` | Bind address. |
| `port` | 3000 | Listen port. |
| `log_level` | `"info"` | One of `trace`, `debug`, `info`, `warn`, `error`. |
| `cors_origins` | `["http://localhost:3000"]` | Allowed CORS origins. Set explicit origins in production. |
| `rate_limit_per_second` | 100 | Requests per second per IP; 0 disables limiting. |
| `rate_limit_burst` | 10 | Rate limiter burst size. |

#### CORS behavior

`build_cors_layer` (`src/main.rs`) maps the origin list to three modes:

1. Single `"*"` entry, or an empty list: any origin is allowed, credentials are disabled, and a warning logs at startup. Browsers reject credentials together with wildcard origins, so this mode never sends them.
2. Any other list: only listed origins pass. Credentials are enabled. Origins that fail to parse drop out silently.

Allowed methods cover GET, POST, PUT, PATCH, DELETE, and OPTIONS. Allowed request headers are `Authorization`, `Content-Type`, `Accept`, `Origin`, and `x-admin-secret`.

#### Rate limit algorithm

The two `rate_limit_*` keys drive one middleware built at startup (`src/main.rs`, around the layer assembly):

- When `rate_limit_per_second > 0`, the app wraps in `tower_governor::GovernorLayer`. The governor runs the Generic Cell Rate Algorithm (GCRA) per client IP: `per_second` sets the sustained refill rate, and `burst_size` permits short bursts above it before rejections start.
- `use_headers()` adds standard `x-ratelimit-*` headers to responses, so clients can observe their budget.
- A background task prunes stale per-IP state every 60 seconds to bound memory.
- Setting `rate_limit_per_second = 0` skips the layer entirely. Startup then logs a warning that disabling limiting is not recommended for production.

A second, independent limiter lives in the API-key auth middleware: tenant daily usage accumulates in the PostgreSQL `daily_rate_limits` table keyed by `(tenant_id, usage_date)` and caches per tenant in memory (`crates/ares-store/src/tenants.rs`). If the database check itself fails, requests fail closed with HTTP 500 `Failed to check rate limit`.

### `[auth]` group

`AuthConfig`:

| Field | Default | Meaning |
|---|---|---|
| `jwt_secret_env` | `"JWT_SECRET"` | Environment variable name holding the JWT (JSON Web Token) secret. |
| `jwt_access_expiry` | 900 | Access token lifetime in seconds (15 minutes). Short-lived by design; refresh tokens renew sessions. |
| `jwt_refresh_expiry` | 604800 | Refresh token lifetime in seconds (7 days). |
| `api_key_env` | `"API_KEY"` | Environment variable name holding the API key. |

Secrets live in environment variables by name, never in `ares.toml`. Expiry values are plain second counts; there is no separate unit key.

### `[database]` group

`DatabaseConfig` (`crates/ares-store/src/config.rs`):

| Field | Default | Meaning |
|---|---|---|
| `url` | `postgres://postgres:postgres@localhost:5432/ares` | PostgreSQL connection string. Holds tenants, agents, skills, run history, billing, and compaction snapshots. |
| `qdrant` | none | Optional `QdrantConfig` table for an external vector store. See RAG. |

### `[providers.*]` group

Each named provider deserializes into `ProviderConfig` (`crates/ares-llm/src/config.rs`), tagged by `type = "..."`. Existing shapes (`openai`, `azure`, `anthropic`, `bedrock`, `ollama`) stay valid. Additional variants match every genai `AdapterKind` name (`gemini`, `groq`, `openrouter`, `openai_resp`, `bedrock_api`, and the rest). `type = "openai"` auto-routes `gpt-5*` to the OpenAI Responses adapter. A missing environment variable fails at client creation with a clear error.

An optional `[nvidia]` group (`NvidiaConfig`) adds catalog settings: `api_key_env`, `api_base`, `models_url`, `catalog_refresh_seconds`, and `default_model`. When absent, the registry synthesizes one NVIDIA provider from defaults.

### `[models.*]` group

`ModelConfig` (`crates/ares-llm/src/config.rs`): `provider` (name under `[providers]`), `model` (identifier sent to the provider), `temperature` (default 0.7), `max_tokens` (default 512).

### Provider pool

The client pool takes a `PoolConfig` (`crates/ares-llm/src/pool.rs`) with these fields:

- `max_in_flight` — maximum simultaneous dispatches admitted per provider. Absent means unlimited and no governor installs.
- `governor_acquire_timeout` — how long a dispatch waits for an in-flight slot before failing closed. Default 30 seconds.
- `max_connections_per_provider` — default 10.
- `min_idle_connections` — default 2.
- `idle_timeout` — default 300 seconds.
- `max_lifetime` — default 1800 seconds.
- `health_check_interval` — default 60 seconds.
- `acquire_timeout` — wait budget for borrowing a pooled client. Default 30 seconds.
- `enable_health_check` — default true.

A permit spans the whole call including streams; saturation fails closed. This admission model rejects excess callers instead of queueing them onto the backend, so a saturated provider degrades loudly and early rather than silently stretching latencies.

## Supervised operation runbook

Start the server with `--supervise` (`src/main.rs`, `src/supervisor.rs`). The daemon runs the real server as a child copy marked by the `CORDIS_SUPERVISED` environment variable. Dropping the child's standard-input handle is the stop request; the child watches for end-of-file and tears down gracefully.

Exit codes drive the loop:

| Exit code | Constant | Effect |
|---|---|---|
| 51 | `EXIT_RESTART` | Start a fresh child. Rapid loops back off exponentially. |
| 52 | `EXIT_QUIT` | End supervision; shut down for good. |
| 53 | `EXIT_BOOT` | Boot failed; report and do not restart. |

Any other terminal status also ends the loop, including death by signal. Hot restarts use code 51 so configuration changes apply without dropping the daemon.

### Restart loop guard rails

The loop carries four safeguards (`src/supervisor.rs`):

- **Rapid-restart cap.** Five exits inside a 30-second window stop the loop with an error instead of spinning. A plugin that crashes at boot trips this cap.
- **Health reset.** A child that ran at least 10 minutes counts as healthy. Its exit clears the strike ladder, so old crashes never doom a fresh process.
- **Backoff ladder.** A child that exited within 10 seconds never proved health. The next respawn delays 100 ms, doubling per consecutive unhealthy run: 100 ms, 200 ms, 400 ms, 800 ms, 1.6 s, 3.2 s, capped at 5 s.
- **Shutdown grace.** After a stop request, the child gets 10 seconds to exit on its own; past that, the daemon force-kills it. The grace bounds the goodbye, never the working lifetime.

Nested supervision refuses to start: a child marked `CORDIS_SUPERVISED` never spawns its own daemon. Exit code 53 mirrors the child's real code to the daemon's process exit, so systemd or another service manager still observes the failure.

```mermaid
flowchart TD
    A[Child exits] --> B{Code}
    B -- 51 RESTART --> C{5 restarts in 30 s?}
    C -- yes --> H[Stop loop with error]
    C -- no --> D{Run under 10 s?}
    D -- yes --> E[Delay backoff ladder]
    D -- no --> F[Reset ladder]
    E --> G[Spawn fresh child]
    F --> G
    B -- 52 QUIT --> I[Shut down daemon]
    B -- 53 BOOT --> J[Report failure, do not restart]
    B -- other / signal --> I
```

## Observability

- Health endpoints: `/health` (Http plugin) and `/health/detailed` (`src/main.rs`). Admin routes expose health metrics and model metrics under `/health/list_health_metrics` and `/health/list_model_metrics` (`crates/ares-http/src/api/routes.rs`). Observed responses from a running v0.10 server (`exercised`):

  ```console
  $ curl -s http://localhost:3000/health
  OK

  $ curl -s http://localhost:3000/health/detailed
  {"status":"healthy","version":"0.1.0","checks":{},"agents":[],"latency_ms":1}
  ```

  `/health` answers plain text for cheap probes. `/health/detailed` returns JSON with per-check status, registered agents, and measured latency.
- Telemetry records: every LLM call produces an `LlmCallRecord` that carries `cached_tokens` and `total_time_ms` alongside token counts (`crates/ares-llm/src/observability.rs`). Micro-call cache hits report `latency_ms: 0` and carry a `cache_hit` flag.

### Telemetry field semantics

Both new columns are optional integers (`Option<i64>`), and their absence carries meaning:

- `cached_tokens` reports tokens served from the provider-side prompt cache. It is `None` when unknown or unreported. When present it is always zero or more and forms a subset of `prompt_tokens`.
- `total_time_ms` measures end-to-end wall-clock time for the whole call, including retries and queueing. Callers commonly mirror `latency_ms` into it when they cannot measure the two separately. `None` means not measured.

### Exporter routing

Log exporters fan records out through the `ExporterRouter` (`crates/ares-llm/src/exporter.rs`):

- Each registered sink declares which records it accepts through a `RecordLevel` gate: `Debug`, `Info`, `Warn`, or `Error`. The level is metadata about the record; it does not change the record.
- The built-in stdout formatter emits a `tracing` info event for successful calls and a warn event for failures. Both include `cached_tokens` and `total_time_ms`; an absent `Option` emits nothing rather than a placeholder.
- Sinks include stdout formatters, database writers, OTLP (OpenTelemetry Protocol) forwarders, and test captures.
- An exporter failure logs a warning inside the exporter and never fails inference.

There is no Prometheus endpoint; scrape-style monitoring must read the admin endpoints above. Skill-step records attach ambient enrichment metadata under the `ambient_enrichment` key when enabled (see Agents).

## Security posture

ARES fails closed on its trust boundaries:

- RAG ingestion and search require authentication and check tenant allowlists before touching data.
- Delegation arguments, review gates, and tool allowlists restrict what agents may call; unknown tools stay blocked when `allowed_tools` names a set.
- Provider governors cap concurrent dispatches and reject excess callers instead of queueing them onto the backend.
- Secrets resolve from named environment variables at use time; config files hold only variable names.

Rotate credentials with this procedure:

1. Add the new value under a fresh environment variable name.
2. Update the matching `*_env` key in `ares.toml`.
3. Restart or hot-restart (exit code 51) the supervised process.
4. Remove the old environment variable after the new one proves active.

Never place secret values in `ares.toml`, TOON files, or version control.

## Backup and restore runbook

Storage choice comes from `[database]` (`DatabaseConfig`, `crates/ares-store/src/config.rs`):

- `url` — PostgreSQL connection string, default `postgres://postgres:postgres@localhost:5432/ares`. PostgreSQL holds tenants, agents, skills, run history, billing, and compaction snapshots.
- `qdrant` — optional external vector store. Back up collections through Qdrant's own snapshot mechanism; ARES treats it as an external service.
- The embedded `ares-vector` store persists under `[rag.vector] vector_path` (default `./data/vectors`). Include that directory in file-level backups.

Backup procedure:

1. Quiesce RAG ingestion first. Stop writes or pause ingest traffic so chunk files stay consistent during the copy.
2. Dump PostgreSQL with standard tooling: `pg_dump "$DATABASE_URL" > ares-backup.sql`. Schedule dumps to match your recovery point objective.
3. Copy the vector directory while writes are quiesced: `cp -a ./data/vectors /backups/vectors-$(date +%F)/`.
4. For Qdrant deployments, take its snapshot through Qdrant's API or tooling. Do not copy its files behind its back.
5. Verify each artifact restores before trusting the schedule: load the dump into a scratch database and open the copied vector directory read-only.

Restore procedure, in this order:

1. Stop the ARES server.
2. Restore PostgreSQL first: create the database, apply `ares-backup.sql`, then let migrations reconcile schema state on next boot.
3. Restore the vector directory second, back to the exact `[rag.vector] vector_path` the restored config names.
4. Restore Qdrant snapshots third, if used.
5. Start the server so migrations run against the restored database before traffic arrives.

Order matters: vector data references documents whose metadata lives in PostgreSQL, so restoring the database first keeps ids consistent.

## CI quality gates

The repository pins one automated quality gate in GitHub Actions (`.github/workflows/ci.yml`, job `crap`, named "CRAP Score Gate"):

```
cargo install cargo-crap
cargo crap --workspace --format json --threshold 30 --fail-above
```

CRAP (Change Risk Anti-Patterns) combines cyclomatic complexity with test coverage per function. The upstream metric definition (Savoia & Evans, 2007; implemented by cargo-crap) is:

$$\mathrm{CRAP}(m) \;=\; \mathrm{comp}(m)^2 \times \left(1 - \frac{\mathrm{cov}(m)}{100}\right)^{3} \;+\; \mathrm{comp}(m)$$

Here \\(\mathrm{comp}(m)\\) is the function's cyclomatic complexity and \\(\mathrm{cov}(m)\\) its line coverage percentage. Three properties explain the gate's shape:

- A trivial, fully covered function scores exactly \\(1\\).
- At full coverage the cubic term collapses, so \\(\mathrm{CRAP}\\) equals complexity. Tests cap the risk; they do not remove it.
- Above complexity \\(\approx 30\\), no coverage level brings the score under the threshold of 30. Oversized functions fail regardless of tests.

Two concrete scores, taken from the upstream tool's own example table: a function with complexity 12 and zero coverage scores \\(12^2 \times (1-0)^3 + 12 = 156\\). A function with complexity 4 at roughly 44% coverage scores \\(16 \times (1-0.444)^3 + 4 \approx 6.7\\).

The gate fails when any workspace function exceeds the threshold of 30. Treat a red gate as work, not noise. Split the flagged function or raise its test coverage.
