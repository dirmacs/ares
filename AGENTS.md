# Agents.md, AI agent guidelines for ARES

## What is ARES?

ARES is the agentic chatbot server at the core of the Dirmacs platform. It orchestrates LLM interactions, manages multi-tenant API access, runs configurable AI agents with tool calling, and provides RAG and research capabilities.

## Architecture Context

- Multi-tenant: Tenants have API keys, usage quotas, and isolated agent configurations.
- Provider-agnostic: LLM calls go through `Llm` and `LLMClient`. HTTP providers use genai. Select the provider at run time with `type` in `ares.toml`.
- Feature-gated: Heavy optional features (MCP, vector stores, UI, `llamacpp`) sit behind Cargo feature flags. HTTP providers are not Cargo features.
- Configuration-driven: Agents, models, tools, and workflows come from TOML/TOON configuration files with hot reload. The server reads `ares.toml` at startup.

Before you touch gated code, make sure that the relevant Cargo features are enabled.

## Common tasks

### Adding a new API endpoint

1. Put the handler in `crates/ares-http/src/api/handlers/` (new file or an existing file).
2. Register the route in `crates/ares-http/src/api/routes.rs`.
3. If the endpoint needs new shared types, put them in `crates/ares-types`.
4. Add the matching hurl test in `hurl/cases/`.

### Adding a new HTTP LLM provider

1. Add a `ProviderConfig` variant in `crates/ares-llm/src/config.rs`.
2. Map that variant to a genai `AdapterKind` in `crates/ares-llm/src/client.rs`.
3. Keep HTTP traffic in `crates/ares-llm/src/genai_client.rs`. Do not add a crate feature. Do not add `openai.rs`.
4. Add the model configuration to `ares.example.toml`.

### Adding a new tool

1. Define the tool in `config/tools/` (TOON format).
2. Put the execution logic in `crates/ares-tools/src/`.
3. Register the tool in the registry in `crates/ares-tools`.

### Working with the database

- All queries in `crates/ares-store/src/` use raw SQL through `sqlx::query().bind()`. The queries use no ORM and no query macros.
- New tables need a migration file in `crates/ares-store/migrations/`. Number the migration files sequentially.
- Aggregate functions such as `SUM()` must cast results to explicit types (`::BIGINT`, `::TEXT`, and more).

## Key decisions

- PostgreSQL over SQLite: ARES moved from Turso/libsql to PostgreSQL for pgvector support, concurrent access, and production reliability. All Docker and deployment configuration references `DATABASE_URL`, not `TURSO_URL`.
- TOON over TOML for agents: Agent configurations use the TOON format for simple, line-oriented definitions that support hot reload.
- Embedded vector database as default: `ares-vector` (pure Rust HNSW) is the default vector store and needs no external dependency or service. Qdrant, LanceDB, and pgvector are optional.
- Axum 0.8: The router uses the `{param}` path syntax, not `:param`.

## Production environment

- A Contabo VPS hosts the deployment behind a Caddy reverse proxy with auto-TLS.
- Systemd service: `ares.service`
- Logs: `journalctl -u ares` and `/var/log/caddy/ares-access.log`
- Configuration repository: `dirmacs/ares-config` (private) holds the production `ares.toml` and the agent definitions.
- CORS allows only requests from `admin.dirmacs.com` and `eruka.dirmacs.com`.
- The admin API requires the `X-Admin-Secret` header. Caddy filters this header from its logs.
