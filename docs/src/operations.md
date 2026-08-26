# Operations

This chapter covers configuration, supervision, observability, security posture, and backup notes for running an ARES server. All keys come from the config structs cited per section.

## Configuration file

The server reads `ares.toml` from the working directory. `ares-server --config my-config.toml` selects another file. `ares-server init` scaffolds a project with `ares.toml`, `.env.example`, and a `config/` directory tree (`src/cli/init.rs`). The root schema is `AresConfig` (`crates/ares-http/src/overlay.rs`).

### `[server]` group

`ServerConfig` (`crates/ares-http/src/config.rs`):

| Field | Default | Meaning |
|---|---|---|
| `host` | `"127.0.0.1"` | Bind address. |
| `port` | 3000 | Listen port. |
| `log_level` | `"info"` | One of `trace`, `debug`, `info`, `warn`, `error`. |
| `cors_origins` | `["*"]` | Allowed CORS origins. Set explicit origins in production. |
| `rate_limit_per_second` | 100 | Requests per second per IP; 0 disables limiting. |
| `rate_limit_burst` | 10 | Rate limiter burst size. |

### `[auth]` group

`AuthConfig`:

| Field | Default | Meaning |
|---|---|---|
| `jwt_secret_env` | `"JWT_SECRET"` | Environment variable name holding the JWT (JSON Web Token) secret. |
| `jwt_access_expiry` | 900 | Access token lifetime in seconds (15 minutes). |
| `jwt_refresh_expiry` | 604800 | Refresh token lifetime in seconds (7 days). |
| `api_key_env` | `"API_KEY"` | Environment variable name holding the API key. |

Secrets live in environment variables by name, never in `ares.toml`.

### `[providers.*]` group

Each named provider deserializes into `ProviderConfig` (`crates/ares-llm/src/config.rs`), tagged by `type = "..."`. Variants: `openai` (fields `api_key_env`, `api_base`, `default_model`; also serves NVIDIA NIM and compatible endpoints), `azure` (`api_key_env`, `base_url_env`, `default_model`), `anthropic` (`api_key_env`, `default_model`), `bedrock` (`api_key_env`, `region_env`, `default_model`), and `ollama` (`base_url`, `default_model`). A missing environment variable fails at client creation with a clear error.

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

A permit spans the whole call including streams; saturation fails closed.

## Supervised mode

Start the server with `--supervise` (`src/main.rs`, `src/supervisor.rs`). The daemon runs the real server as a child copy marked by the `CORDIS_SUPERVISED` environment variable. Exit codes drive the loop:

| Exit code | Constant | Effect |
|---|---|---|
| 51 | `EXIT_RESTART` | Start a fresh child. Rapid loops back off exponentially. |
| 52 | `EXIT_QUIT` | End supervision; shut down for good. |
| 53 | `EXIT_BOOT` | Boot failed; report and do not restart. |

Any other terminal status also ends the loop. Hot restarts use code 51 so configuration changes apply without dropping the daemon.

## Observability

- Health endpoints: `/health` (Http plugin) and `/health/detailed` (`src/main.rs`). Admin routes expose health metrics and model metrics under `/health/list_health_metrics` and `/health/list_model_metrics` (`crates/ares-http/src/api/routes.rs`).
- Telemetry records: every LLM call produces an `LlmCallRecord` that carries `cached_tokens` and `total_time_ms` alongside token counts (`crates/ares-llm/src/observability.rs`). Micro-call cache hits report `latency_ms: 0` and carry a `cache_hit` flag.
- Log exporters: the `ExporterRouter` (`crates/ares-llm/src/exporter.rs`) fans LLM and tool call records out to registered sinks — stdout formatters, database writers, OTLP (OpenTelemetry Protocol) forwarders, or test captures. Each sink gates on a `RecordLevel` (`Debug`, `Info`, `Warn`, `Error`). An exporter failure logs a warning inside the exporter and never fails inference.
- Skill-step records attach ambient enrichment metadata under the `ambient_enrichment` key when enabled (see Agents).

There is no Prometheus endpoint; scrape-style monitoring must read the admin endpoints above.

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

## Backup notes

Storage choice comes from `[database]` (`DatabaseConfig`, `crates/ares-store/src/config.rs`):

- `url` — PostgreSQL connection string, default `postgres://postgres:postgres@localhost:5432/ares`. PostgreSQL holds tenants, agents, skills, run history, billing, and compaction snapshots. Back it up with standard database tooling such as `pg_dump` on a schedule that matches your recovery point objective.
- `qdrant` — optional external vector store. Back up collections through Qdrant's own snapshot mechanism; ARES treats it as an external service.
- The embedded `ares-vector` store persists under `[rag.vector] vector_path` (default `./data/vectors`). Include that directory in file-level backups. Stop writes or quiesce ingestion during the copy so chunk files stay consistent.

Restore order matters: restore PostgreSQL first, then vector data, then start the server so migrations run against the restored database.
