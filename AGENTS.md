# AGENTS.md — AI Agent Guidelines for ARES

## What is ARES?

ARES is the agentic chatbot server at the core of the Dirmacs platform. It orchestrates LLM interactions, manages multi-tenant API access, runs configurable AI agents with tool calling, and provides RAG and research capabilities.

## Architecture Context

- **Multi-tenant**: Tenants have API keys, usage quotas, and isolated agent configurations
- **Provider-agnostic**: LLM calls go through an abstraction layer (`src/llm/client.rs`) — Ollama, OpenAI, Anthropic, and LlamaCpp are interchangeable
- **Feature-gated**: Heavy optional features (MCP, specific vector stores, UI embedding) are behind Cargo feature flags. Always check which features are enabled before touching gated code
- **Config-driven**: Agents, models, tools, and workflows are defined in TOML/TOON config files and hot-reloaded. The server reads `ares.toml` at startup

## Common Tasks

### Adding a new API endpoint
1. Add handler in `src/api/handlers/` (new file or extend existing)
2. Register route in `src/api/routes.rs`
3. Add types/models in `src/models/` if needed
4. Add corresponding hurl test in `hurl/cases/`

### Adding a new LLM provider
1. Create `src/llm/new_provider.rs` implementing the provider trait
2. Add feature flag in `Cargo.toml`
3. Register in `src/llm/mod.rs` behind `#[cfg(feature = "new_provider")]`
4. Add model config in `ares.example.toml`

### Adding a new tool
1. Define tool in `config/tools/` (TOON format)
2. Implement execution logic in `src/tools/`
3. Register in tool registry

### Working with the database
- All queries in `src/db/` use raw SQL with `sqlx::query().bind()` — no ORM, no query macros
- New tables need a migration file in `migrations/` (numbered sequentially)
- `SUM()` and other aggregate functions must cast results to explicit types (`::BIGINT`, `::TEXT`, etc.)

## Key Decisions

- **PostgreSQL over SQLite**: Migrated from Turso/libsql to PostgreSQL for pgvector support, concurrent access, and production reliability. All Docker/deployment configs now reference `DATABASE_URL` (not `TURSO_URL`)
- **TOON over TOML for agents**: Agent configs use TOON format for simpler, line-oriented definitions that are easier to hot-reload
- **Embedded vector DB as default**: `ares-vector` (pure Rust HNSW) is the default vector store — no external dependencies needed. Qdrant, pgvector, etc. are optional
- **Axum 0.8**: Uses `{param}` path syntax (not `:param`)

## Production Environment

- Deployed on Contabo VPS behind Caddy reverse proxy (auto-TLS)
- Systemd service: `ares.service`
- Logs: `journalctl -u ares` and `/var/log/caddy/ares-access.log`
- Config repo: `dirmacs/ares-config` (private) holds production `ares.toml` and agent definitions
- CORS: Restricted to `admin.dirmacs.com` and `eruka.dirmacs.com`
- Admin API: Protected by `X-Admin-Secret` header (filtered from Caddy logs)
