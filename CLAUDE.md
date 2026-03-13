# CLAUDE.md — Development Guide for AI Assistants

## Project Overview

**A.R.E.S** (Agentic Retrieval Enhanced Server) is a production Rust server for multi-agent AI orchestration. It provides LLM-agnostic chat, RAG, tool calling, research workflows, and MCP integration. Published to crates.io as `ares-server`.

- **Language:** Rust (edition 2021, MSRV 1.91)
- **Framework:** Axum 0.8 (async HTTP)
- **Database:** PostgreSQL 16 via sqlx 0.8
- **Deployed at:** api.ares.dirmacs.com (VPS, systemd)

## Build & Run

```bash
cargo build --release                          # Default features: postgres, ollama, ares-vector
cargo build --release --features full          # All providers + qdrant + mcp + swagger-ui
cargo run --release -- serve                   # Start server (reads ares.toml)
cargo run --release -- init                    # Initialize project structure
cargo test                                     # Run all tests
```

Key feature flags: `postgres`, `ollama`, `openai`, `anthropic`, `llamacpp`, `mcp`, `ares-vector`, `qdrant`, `pgvector`, `local-embeddings`, `ui`, `swagger-ui`, `full`, `minimal`.

Default features are `postgres`, `ollama`, `ares-vector`.

## Project Structure

```
src/
├── main.rs                  # Binary entry point, CLI, server startup
├── lib.rs                   # Library root, re-exports
├── agents/                  # Agent trait, configurable agents, orchestrator, router
├── api/handlers/            # Axum route handlers (admin, auth, chat, rag, research, etc.)
├── api/routes.rs            # Route tree
├── auth/                    # JWT auth, password hashing, middleware
├── cli/                     # CLI subcommands, init, colored output
├── db/                      # Database layer (postgres.rs, traits.rs, vector stores)
├── llm/                     # LLM provider abstraction (ollama, openai, anthropic, llamacpp)
├── mcp/                     # MCP server + client (feature-gated)
├── memory/                  # User context & personalization
├── middleware/               # API key auth, usage tracking
├── models/                  # Serde/Utoipa schema definitions
├── rag/                     # RAG pipeline (chunking, embeddings, search, reranking)
├── research/                # Multi-step research coordinator
├── tools/                   # Built-in tools (calculator, web_search)
├── types/                   # Shared types, AppError
├── utils/                   # Config parsing (TOML + TOON), helpers
└── workflows/               # Declarative workflow engine
crates/
├── ares-vector/             # Embedded HNSW vector DB (pure Rust)
└── pawan/                   # CLI agent tool
```

## Database

PostgreSQL with 7 migrations in `migrations/`. Schema covers tenants, API keys, usage events, agent runs, alerts, audit log.

`SUM()` queries must cast to `::BIGINT` — sqlx maps SQL NUMERIC to Rust `Decimal`, not `i64`.

## Configuration

- `ares.toml` — runtime config (gitignored, production secrets). See `ares.example.toml` for template.
- `config/agents/*.toon` — agent definitions in TOON format
- `config/models/*.toon` — model tier definitions
- `config/tools/*.toon` — tool specs
- `config/workflows/*.toon` — workflow orchestration
- `config/mcps/*.toon` — MCP server integrations

### TOON format rules
- Strings starting with `-` must be quoted: `"--endpoint"`
- Arrays use indexed syntax: `args[2]: "--endpoint","https://example.com/mcp"`
- Line-oriented, one key-value per line

## Testing

```bash
cargo test                                # Unit + integration tests
hurl --test hurl/cases/*.hurl             # HTTP API tests (requires running server)
```

Integration tests in `tests/`, HTTP tests in `hurl/cases/`.

## Key Conventions

- Feature-gated modules: MCP is behind `#[cfg(feature = "mcp")]` — guard all MCP imports and usage
- Error type: `AppError` in `src/types/` — use it consistently
- All DB queries use `sqlx::query()` with `.bind()` pattern (no macros)
- Auth: JWT Bearer tokens for user routes, `X-Admin-Secret` header for admin routes, API keys for tenant routes
- Commits as: `Baalateja Kataru <baalateja.k@gmail.com>`

## What NOT to Do

- Don't add `use ares::mcp::*` without `#[cfg(feature = "mcp")]` guard
- Don't use `SUM()` in SQL without `::BIGINT` cast
- Don't commit `ares.toml` (production config with secrets) — it's gitignored
- Don't set CORS origins to `*` in production — use explicit origins in `src/utils/toml_config.rs`
- Don't use `.unwrap()` in production code paths
