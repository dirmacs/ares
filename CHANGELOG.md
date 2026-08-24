# Changelog

All notable changes to A.R.E.S will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.1] - 2026-08-24

### Changed — library facade

- **The `ares` facade crate is folded into the `ares-server` package as a `[lib]` target**; `crates/ares` is deleted. The crates.io name `ares` is occupied by an unrelated 2015 package. The name can never publish, so the published entry point for embedding ARES as a library is now `ares-server = "0.9.1"`: `use ares_server::{Context, Execute, Tools, Llm, Plugin, Loader, Dispatch, register_plugins};`. `Store` re-export remains behind the `postgres` feature. The inventory-parity guarantee moved to a root-package test (`tests/inventory_parity.rs`) with force-linking of every capability crate including `ares-http`.

### Added — Cordis rounds 4–9

Nine hardening/feature rounds on the Cordis kernel and its use across the server (full narrative in `docs/cordis-mapping.md` §10–§19):

- **RhaiPolicy scripting** (default-on): declarative TOML entries attach sandboxed Rhai functions to catalog events; `on_error = "deny"` fail-closed gates; isolate-realm multi-instance coexistence; two active policies ship in `config/cordis-entries.toml` (admission audit, emergency-halt).
- **Guarded withdrawal** (paper §4.3.1): derived realm-aware consumer counting; `Context::remove<T>` refuses while active consumers exist (`remove_forced` for internal rollback); admin retire maps refusal to 409.
- **Verified hot-swap + drain-and-shift**: entry rebuilds trial out-of-band before committing (zero absence window, concurrent-get proven); `Loader::replace_provider` plus `POST /admin/cordis/services/{name}/replace` swap providers under live traffic.
- **Static registration**: inventory-collected `CordisPluginFactory` submissions are the primary boot path; manual chains remain as fallback; parity tests pin the factory set.
- **Composition**: `@include` splice, `@group` flatten, `${rhai: …}` config interpolation run at boot and reload (fail-open); production entries program split across files with parity proof.
- **Dependency-cycle detection**: post-apply graph walk warns on rings; surfaced via `Loader::detect_cycle_entry_ids` and `GET /admin/cordis/entries`.
- **Peer-dependency versioning**: `provide_versioned` / `declare_inject_versioned`; major-bucket satisfaction rule; mismatch keeps dependents Inactive reactively.
- **Eager inject reconciliation**: declarations on Active fibers take effect immediately; declare/refresh races lossless.
- **Failed-factory wiring**: failed registrations notify dependents and stay inspectable (`Failed{error}` terminal); fresh-id supersession on re-register.
- **Typed listeners adopted** in scheduler prod paths; **per-event dispatch metrics** at `GET /admin/cordis/events`; **atomic temp+rename** entries persistence; **metatheory property suite** (`cordis::metatheory`) proving quiescence, order confluence, LIFO disposal, reactive invariant.
- **HMR dylib hot-swap** finished: unique-copy dlopen so rebuilt `.so` files actually swap; strictly opt-in behind `--features hmr`.

### Fixed

- Latent deadlock in `RegistryService::register` (read guard held across stale-slot write cleanup), exposed by metatheory leg F.
- Latent double-swap bug in verified rebuilds (promoted values lacked undo; a second swap wedges re-provide).

## [0.7.5] - 2026-04-11

### Changed

- **`mcp` feature decoupled from `postgres`**, replaces the 0.7.4 coarse coupling. Library consumers can now enable `mcp` for protocol glue, client, registry, extension, and tool plumbing **without** dragging `sqlx` and `postgres` into their dependency graph. The MCP *server* (`mcp/server.rs`), usage tracking (`mcp/usage.rs`), and tenant API-key auth (`mcp/auth.rs`, uses `crate::db::tenants::TenantDb`) are now gated behind `cfg(all(feature = "mcp", feature = "postgres"))`. Their only consumer is `start_mcp_server`, which `main.rs` already gates with the same combination. Verified compile-clean for `--features "mcp"`, `--features "mcp,postgres"`, and default features. 234 lib tests pass.

## [0.7.4] - 2026-04-11

### Fixed

- **`mcp` feature implies `postgres`**, the MCP server code under `src/mcp/server.rs` and `src/mcp/usage.rs` references `sqlx::PgPool` directly, so enabling `features = ["mcp"]` without `features = ["postgres"]` produced `E0433: unresolved module sqlx` compile errors. Making `mcp = ["dep:rmcp", "postgres"]` fixes the feature graph so any consumer that turns on MCP gets the transitive postgres dep automatically. Downstream crates that previously had to spell out both features can now enable `mcp` alone. No behavior change, the dep was already implicit at the code level.

> **Note:** 0.7.4 was never published to crates.io. It was superseded by 0.7.5, which decouples the modules behind separate `cfg` gates so `mcp` no longer requires `postgres`.

## [0.7.3] - 2026-04-11

### Fixed

- **`sqlx::query!` / `sqlx::query_as!` macros replaced with runtime variants**, downstream crates no longer need `DATABASE_URL` at compile time or a shipped `.sqlx` cache to build `ares-server`. Fixes compilation failures in any consumer that pulls `ares-server` from crates.io with `features = ["postgres"]`.
 - `src/middleware/usage.rs`: `sqlx::query!(...)` → `sqlx::query(...).bind(...)`
 - `src/db/agent_versions.rs`: `sqlx::query_as!(AgentVersionRecord, ...)` → `sqlx::query_as::<_, AgentVersionRecord>(...).bind(...)`
 - `AgentVersionRecord` now derives `sqlx::FromRow`.

### Note

No schema or behavior change, only compile-time check removed to unblock downstream crate builds. Library crates shipped via crates.io cannot assume consumers have a live DB or prepared cache.

## [0.6.2] - 2026-03-08

### Security

- Config split: Removed all Dirmacs-specific production configs from public repo
 - `ares.toml` (23 agents), `kasino.toml`, `agents/kasino-*.toml` moved to private `dirmacs/ares-config`
 - Public repo retains `ares.example.toml` as generic template
 - Updated `.gitignore` to prevent re-tracking of `ares.toml`, `kasino.toml`, `agents/*.toml`

### Added

- **Compliance auditor agent** (`compliance-auditor.toon`) in overlay, audits projects against Dirmacs Engineering Standards
- **Dirmacs Engineering Standards SOP**, covers repo structure, config architecture, deployment, security, agent quality, and scaling

## [0.6.3] - 2026-03-13

### Added

- Deploy automation API: `POST /api/admin/deploy`, `GET /api/admin/deploy/{id}`, `GET /api/admin/deploys` for triggering and tracking deployments
- Service health monitoring: `GET /api/admin/services` returns status, PID, and port for all VPS services
- Service log viewer: `GET /api/admin/services/{name}/logs` returns recent journalctl output
- Deploy registry: In-memory deploy tracking in AppState with status polling

### Fixed

- Chat handler tenant_id: Fixed tenant_id extraction in chat handler for metered requests
- Migration 002 checksum: Fixed checksum mismatch after migration edit

---

## [0.6.1] - 2026-03-07

### Changed

- LLM Provider: Switched default provider from Anthropic to Groq (free tier)
 - Provider: `groq` at `https://api.groq.com/openai/v1` (OpenAI-compatible)
 - `fast` tier: `llama-3.1-8b-instant` (14,400 req/day free)
 - `balanced` + `powerful` tiers: `llama-3.3-70b-versatile` (GPT-4 class, 6,000 req/day free)
 - Env var: `GROQ_API_KEY` replaces `ANTHROPIC_API_KEY`
 - Anthropic provider kept in `ares.toml`, switch back by changing tier `provider` fields

- VPS build flags: `--no-default-features --features openai,postgres,mcp`
 - Avoids dev-only defaults (`local-db`, `ollama`, `ares-vector`)

### Infrastructure

- First production deployment: Contabo VPS 217.216.78.38
- Caddy reverse proxy: `api.ares.dirmacs.com` → `localhost:8080`
- systemd service: `/etc/systemd/system/ares.service`
- PostgreSQL: `ares` database, user `dirmacs`

---

## [Unreleased]

### Added

- PostgreSQL Database Support: Migrated from SQLite/libsql to PostgreSQL
 - New `PostgresClient` with `sqlx::PgPool` for connection pooling
 - Multi-tenant database support via `TenantDb`
 - Automatic database migrations on startup
 - Support for PostgreSQL `$1, $2, ...` query placeholders
 - Added `#[derive(sqlx::FromRow)]` for database models
 - Updated all SQL queries for PostgreSQL syntax
 - New `init_postgres_db()` function for simplified initialization
 - Location: `src/db/postgres.rs`, `src/db/tenants.rs`

### Changed

- BREAKING: Database backend changed from SQLite to PostgreSQL
 - Connection strings now use PostgreSQL format: `postgres://user:pass@host:5432/dbname`
 - Removed `turso_url_env` and `turso_token_env` from configuration
 - Default database URL: `postgres://postgres:postgres@localhost:5432/ares`
 - See Migration Guide below for upgrade instructions

### Removed

- libsql/SQLite: Complete removal of SQLite backend
 - Removed `libsql` dependency
 - Removed `TursoClient` and `src/db/turso.rs`
 - Removed `turso` feature flag
 - Removed `hnsw_rs` dependency (moved to pgvector)

- OllamaToolCoordinator: Removed in favor of the unified `ToolCoordinator`
 - `OllamaToolCoordinator` struct
 - `ToolCoordinatorResult` struct (ollama-specific)
 - `ToolCallRecord` struct
 - `ToolCallingConfig` struct (ollama-specific)
 - `OllamaClient::with_config()` and `with_config_and_params()` constructors
 - `OllamaClient::tool_config()` and `set_tool_config()` methods
 - `OllamaClient::generate_with_tool_loop()` method
 - `execute_tool_call()` and `format_tool_result()` helper functions
 - Related tests

- LegacyEmbeddingService: Removed deprecated wrapper struct from `src/rag/embeddings.rs`
 - Use `EmbeddingService` directly instead

- LlamaCppClient::with_params_legacy(): Removed legacy constructor
 - Use `with_config_params()` or `with_params()` instead

- Legacy Environment Variables documentation: Removed from README.md
 - Use the standard environment variables documented in the Configuration section

### PostgreSQL migration guide

#### For local development

1. **Install PostgreSQL**:
 ```bash
   # Ubuntu/Debian
   sudo apt install postgresql postgresql-contrib
   
   # macOS
   brew install postgresql
   
   # Windows
   # Download from https://www.postgresql.org/download/windows/
   ```

2. **Create database and user**:
 ```bash
   sudo -u postgres psql -c "CREATE DATABASE ares;"
   sudo -u postgres psql -c "CREATE USER ares WITH PASSWORD 'your_password';"
   sudo -u postgres psql -c "GRANT ALL PRIVILEGES ON DATABASE ares TO ares;"
   ```

3. **Set environment variable**:
 ```bash
   export DATABASE_URL="postgres://ares:your_password@localhost:5432/ares"
   ```

4. **Run ARES**: Migrations run automatically on startup

#### For production (Contabo VPS)

See `sops/deployment-checklist.md` for complete VPS deployment instructions.

### PostgreSQL migration guide

#### For local development

1. **Install PostgreSQL**:
 ```bash
   # Ubuntu/Debian
   sudo apt install postgresql postgresql-contrib
   
   # macOS
   brew install postgresql
   
   # Windows
   # Download from https://www.postgresql.org/download/windows/
   ```

2. **Create database and user**:
 ```bash
   sudo -u postgres psql -c "CREATE DATABASE ares;"
   sudo -u postgres psql -c "CREATE USER ares WITH PASSWORD 'your_password';"
   sudo -u postgres psql -c "GRANT ALL PRIVILEGES ON DATABASE ares TO ares;"
   ```

3. **Set environment variable**:
 ```bash
   export DATABASE_URL="postgres://ares:your_password@localhost:5432/ares"
   ```

4. **Run ARES**: Migrations run automatically on startup

#### For production (Contabo VPS)

See `sops/deployment-checklist.md` for complete VPS deployment instructions.

### Migration guide

If you were using any of the removed APIs:

1. **OllamaToolCoordinator** → Use `ToolCoordinator` from `src/llm/coordinator.rs`
 ```rust
   // Old
   let coordinator = OllamaToolCoordinator::new(client, registry);
   
   // New
   use ares::llm::coordinator::{ToolCoordinator, ToolCallingConfig};
   let coordinator = ToolCoordinator::new(client, registry, ToolCallingConfig::default());
   ```

2. **LegacyEmbeddingService** → Use `EmbeddingService`
 ```rust
   // Old
   let service = LegacyEmbeddingService::new("model")?;
   
   // New
   let service = EmbeddingService::with_default_model()?;
   ```

3. **LlamaCppClient::with_params_legacy()** → Use `with_config_params()`
 ```rust
   // Old
   let client = LlamaCppClient::with_params_legacy(path, ctx, threads, max_tokens)?;
   
   // New
   let client = LlamaCppClient::with_config_params(path, ctx, threads, max_tokens, 0.7, 0.9)?;
   ```

## [0.5.0] - 2026-02-01

### Added

- Unified ToolCoordinator: Provider-agnostic multi-turn tool calling orchestration
 - New `ToolCoordinator` struct for managing tool calling across all LLM providers
 - `ToolCallingConfig` for configuring max iterations, parallel tool calls, and timeouts
 - `ConversationMessage` enum for unified message representation
 - New `generate_with_tools_and_history()` method added to `LLMClient` trait
 - Implemented for all 4 providers: OpenAI, Anthropic, Ollama, LlamaCpp
 - Location: `src/llm/coordinator.rs`

### Deprecated

- OllamaToolCoordinator: Deprecated in favor of the new unified `ToolCoordinator`
 - Note: Removed in v0.6.0 - see migration guide above
 - Migrate to `ToolCoordinator` for cross-provider compatibility

## [0.4.0] - 2026-02-01

### Added

- Anthropic Claude API Provider: Full support for Claude models via the Anthropic API
 - New `anthropic` feature flag
 - Supports Claude 3.5 Sonnet, Claude 3 Opus, Haiku, and all Claude model variants
 - Streaming support with tool calling
 - Implements full `LLMClient` trait
 - Location: `src/llm/anthropic.rs`

- Token Usage Tracking: LLM responses now include token usage statistics
 - New `TokenUsage` struct with `prompt_tokens`, `completion_tokens`, `total_tokens`
 - Added `usage` field to `LLMResponse`
 - Tracked across all LLM providers
 - Location: `src/llm/types.rs`

- New Feature Bundles for Local Embeddings:
 - `full-local-embeddings` - Full features with local embeddings (Linux/macOS only)
 - `full-ui-local-embeddings` - Full features with UI and local embeddings

### Changed

- **`full` feature no longer includes `local-embeddings`**: The `local-embeddings` feature has been removed from the `full` feature bundle due to ort-sys linker errors on Windows MSVC. Use `full-local-embeddings` on Linux/macOS if you need local embeddings.

### Fixed

- ort-sys Windows MSVC Linker Error: Added compile-time error for `local-embeddings` feature on Windows MSVC targets
 - Prevents cryptic linker errors by failing fast with a helpful message
 - Users on Windows must use WSL, remote embedding APIs, or Linux/macOS
 - Location: `src/rag/embeddings.rs`

- lru security advisory: Updated `lru` to 0.16.3 to fix RUSTSEC-2026-0002 (stacked borrows unsound in IterMut)

### Security

- RUSTSEC-2026-0002: Fixed by updating `lru` crate to 0.16.3

## [0.3.3] - 2026-01-28

### Fixed

#### Critical (P0)

- Embedding Model Recreation: Fixed bug where embedding model was recreated on every call
 - Model now reused via `OnceLock` pattern, significantly improving performance
 - Location: `src/rag/embeddings.rs`

- Rate Limiter Not Applied: Fixed rate limiting middleware not being applied to routes
 - Added `tower_governor` integration with proper state sharing
 - Location: `src/main.rs`, `Cargo.toml`

- Character Chunking Bug: Fixed chunker splitting in middle of UTF-8 characters
 - Now uses `chars().count()` instead of byte length for character chunking
 - Location: `src/rag/chunker.rs`

#### High priority (P1)

- Refresh Token Invalidation: Fixed refresh tokens not being invalidated on logout
 - Added `delete_session_by_token_hash()` to `DbPool` trait for session cleanup on logout
 - Location: `src/api/handlers/auth.rs`, `src/db/traits.rs`

- Model Config Params Not Passed: Fixed LLM clients ignoring temperature/top_p/max_tokens
 - All LLM clients now properly apply model configuration parameters
 - Location: `src/llm/ollama.rs`, `src/llm/openai.rs`, `src/llm/llamacpp.rs`

- RAG Collection User Isolation: Added user isolation for RAG collections
 - Collections now prefixed with user ID to prevent cross-user access
 - Location: `src/api/handlers/rag.rs`

- TOON Agents Not in Registry: Fixed TOON-defined agents not integrated with AgentRegistry
 - Added `register_toon_agents()` to load agents from TOML config into registry
 - Location: `src/agents/registry.rs`

#### Medium priority (P2)

- LRU Cache Not Evicting: Fixed `LruEmbeddingCache` not properly evicting oldest entries
 - Implemented proper LRU eviction with access ordering
 - Location: `src/rag/cache.rs`

- BM25/Fuzzy Index Persistence: Added persistent BM25 and fuzzy search indices
 - Indices now saved to disk and loaded on startup
 - Location: `src/rag/search.rs`

- Error Handling Inconsistent: Standardized error handling across codebase
 - Added structured `AppError` with consistent error codes and context
 - Location: `src/types/mod.rs`

- Qdrant Missing get() Method: Added missing `get()` method to Qdrant vector store
 - Location: `src/db/qdrant.rs`

#### Low priority (P3)

- Health Endpoint: Added `/health` endpoint for load balancer probes
 - Returns JSON with status, version, and uptime
 - Location: `src/main.rs`

- Logout Endpoint: Added `/api/auth/logout` endpoint
 - Properly invalidates refresh tokens and clears session
 - Location: `src/api/handlers/auth.rs`, `src/api/routes.rs`

- AppError Structured Context: Fixed `AppError` to include structured context
 - Added `context` field for additional error metadata
 - Location: `src/types/mod.rs`

- JWT Secret Validation: Added minimum length validation for JWT secret
 - Errors on startup if JWT_SECRET is less than 32 characters
 - Location: `src/utils/toml_config.rs`

- Streaming Methods Missing: Added `stream_with_system()` and `stream_with_history()` to `LLMClient` trait
 - All LLM provider implementations now support streaming with system prompts and history
 - Location: `src/llm/client.rs`, `src/llm/ollama.rs`, `src/llm/openai.rs`, `src/llm/llamacpp.rs`

- AgentType Extensibility: Made `AgentType` enum extensible with `Custom(String)` variant
 - Added `from_string()` method for parsing unknown agent types
 - Location: `src/types/mod.rs`, `src/agents/*.rs`

### Changed

- Updated test mocks to include new `LLMClient` streaming methods
- Removed obsolete vector store stubs (already completed in prior version)

## [0.3.2] - 2026-01-28

### Added

- Query-Level Typo Correction: Fuzzy search now corrects typos in search queries
 - `QueryCorrection` struct for vocabulary-based word correction
 - `correct_word()` and `correct_query()` methods using Levenshtein distance
 - `search_bm25_with_correction()` and `search_hybrid_with_correction()` methods
 - Vocabulary built from indexed documents for domain-specific corrections
 - Location: `src/rag/search.rs`
 - Closes GitHub issue #4

- Embedding Cache: In-memory LRU cache for embedding vectors
 - `EmbeddingCache` trait with `get/set/invalidate/clear/stats` methods
 - `LruEmbeddingCache` implementation with SHA-256 hashing, configurable max entries, optional TTL
 - `CachedEmbeddingService` wrapper for transparent caching
 - Thread-safe with `parking_lot` RwLock
 - 12 complete tests
 - Location: `src/rag/cache.rs`

### Changed

- Updated documentation to reflect implemented features
 - `KNOWN_ISSUES.md`: Marked fuzzy search typo issue as resolved
 - `DIR-24_RAG_IMPLEMENTATION_PLAN.md`: Marked embedding cache as implemented
 - `FUTURE_ENHANCEMENTS.md`: Updated embedding cache section
 - `README.md`: Updated version reference

### Removed

- Stale session log file (`session-ses_43bd.md`)

## [0.3.1] - 2026-01-16

### Fixed

- Vector Persistence (CRITICAL): Fixed bug where vectors were not saved to disk
 - Root cause: the HNSW index did not support iteration, so `save_collection()` saved empty files
 - Added `export_all()` method to `HnswIndex` in `crates/ares-vector/src/index.rs`
 - Added `export_all()` method to `Collection` in `crates/ares-vector/src/collection.rs`
 - Updated `save_collection()` in `crates/ares-vector/src/persistence.rs` to actually save vectors
 - Added regression tests: `test_vector_persistence_regression` and `test_metadata_persistence`

- Race Condition in Parallel Model Loading (MEDIUM): Fixed concurrent download failures
 - Root cause: Multiple threads loading fastembed model simultaneously caused conflicts
 - Added per-model initialization locks using `OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>>`
 - Applied locks to `EmbeddingService::new()`, `embed_texts()`, and `embed_sparse()`
 - Location: `src/rag/embeddings.rs`

### Known issues

- Fuzzy Search with Query Typos (LOW): Query "progamming languge" returns 0 results
 - See GitHub issue #4 for details and proposed fix
 - Workaround: Use semantic search or spell queries correctly

## [0.3.0] - 2026-01-13

### Added

- ares-vector: Pure-Rust vector database with HNSW indexing
 - No external dependencies (Qdrant, Milvus, etc. not required)
 - Memory-mapped persistence via `memmap2`
 - Multiple distance metrics: Cosine, Euclidean, Dot Product
 - Thread-safe with `parking_lot` RwLocks
 - Collection management (create, delete, list)
 - Located in `crates/ares-vector/`

- RAG Pipeline: complete document retrieval system
 - Document ingestion with automatic chunking
 - Multiple chunking strategies: word, character, semantic
 - Configurable chunk size and overlap

- Embedding Service: Multi-model embedding support
 - BGE family (small, base, large) via FastEmbed
 - All-MiniLM models (L6, L12)
 - Nomic Embed Text v1.5
 - Qwen3 Embeddings (via Candle)
 - GTE-Modern-BERT (via Candle)
 - Sparse embeddings (SPLADE) for hybrid search

- Multi-Strategy Search: Multiple search algorithms
 - Semantic: Vector similarity search
 - BM25: Traditional TF-IDF keyword matching
 - Fuzzy: Levenshtein distance for typo tolerance
 - Hybrid: Weighted combination of semantic + BM25

- Reranking: Cross-encoder reranking for improved relevance
 - MiniLM-L6-v2 cross-encoder
 - BGE Reranker support

- RAG API Endpoints:
 - `POST /api/rag/ingest` - Ingest documents with chunking
 - `POST /api/rag/search` - Multi-strategy search with optional reranking
 - `GET /api/rag/collections` - List all collections
 - `DELETE /api/rag/collections/{name}` - Delete a collection

- New feature flag: `ares-vector` for pure-Rust vector store

### Changed

- CI Workflow: Added `ares-vector` feature to test matrix across all platforms
- Feature flags: Now 15+ feature flags (was 12+)

## [0.2.5] - 2024-12-21

### Changed

- Swagger UI is now optional: The interactive API documentation (Swagger UI) is now behind the `swagger-ui` feature flag
 - This reduces the default binary size and build time
 - The core server no longer requires network access during build
 - Enable with `cargo build --features swagger-ui` or use the `full` bundle
 - When enabled, Swagger UI is available at `/swagger-ui/`

- Improved docs.rs compatibility: Documentation builds now work on docs.rs
 - Removed problematic dependencies from docs.rs builds (`llamacpp`, `qdrant`, `swagger-ui`)
 - These features require native compilation or network access docs.rs doesn't support

### Fixed

- docs.rs build failures: Fixed build failures caused by:
 - `utoipa-swagger-ui` requiring network access to download Swagger UI assets
 - `llama-cpp-sys-2` requiring native C++ compilation
 - `qdrant-client` build script requiring filesystem write access

## [0.2.4] - 2024-12-21

### Fixed

- CI workflow: Fixed rust-cache key validation errors caused by commas in feature matrix
- Clippy errors: Fixed various clippy warnings treated as errors in CI
- Test compilation: Fixed `ChatCompletionTools` enum pattern matching in OpenAI tests

## 0.2.3

### Added

- CLI Commands: Full-featured command-line interface with colored TUI output
 - `ares-server init` - Scaffold a new A.R.E.S project with all configuration files
 - `ares-server config` - View and validate configuration
 - `ares-server agent list` - List all configured agents
 - `ares-server agent show <name>` - Show details for a specific agent
 - Global options: `--config`, `--verbose`, `--no-color`
 - Init options: `--force`, `--minimal`, `--no-examples`, `--provider`, `--host`, `--port`

- Embedded Web UI: Leptos-based frontend that can be bundled with the backend
 - New `ui` feature flag to embed the UI in the server binary
 - New `full-ui` feature bundle (all features + UI)
 - UI served at `/` when enabled
 - SPA routing support for client-side navigation

- Node.js Runtime Detection: Build-time check for bun, npm, or deno when UI feature is enabled

- CLI Integration Tests: complete test suite for all CLI commands
 - Unit tests for output formatting
 - Unit tests for init scaffolding
 - Integration tests for command execution

### Changed

- Installation Experience: Users can now run `ares-server init` after installing via `cargo install`
 - No longer requires cloning the repository to get started
 - Auto-generates `ares.toml`, `.env.example`, and all TOON configuration files
 - Creates directory structure: `data/`, `config/agents/`, `config/models/`, etc.

- Justfile: Added new commands
 - `just init` - Initialize project using CLI
 - `just build-ui` - Build with embedded UI (auto-detects Node.js runtime)
 - `just build-full-ui` - Build with all features including UI
 - `just run-ui` - Run server with UI feature
 - `just check-node` - Check for available Node.js runtime

- CI Workflow: Updated to include CLI tests and UI builds
 - New `cli-tests` job for CLI integration tests
 - New `build-ui` job for UI feature compilation
 - Tests run on all supported platforms

- Dockerfile: Updated for new CLI and binary name
 - Multi-stage build with UI support
 - Non-root user for improved security
 - Proper binary name (`ares-server`)

- Documentation: complete updates
 - README.md: Added CLI commands, UI feature, troubleshooting, requirements sections
 - QUICK_REFERENCE.md: Added CLI quick reference
 - Added CHANGELOG.md

### Fixed

- Configuration loading no longer requires environment variables for info commands
 - `ares-server config` works without JWT_SECRET set
 - `ares-server agent list/show` works without environment variables

## [0.2.2] - 2024-12-20

### Added

- Hot-reload configuration support for `ares.toml`
- TOON format support for agent, model, tool, and workflow configurations
- Dynamic configuration manager for runtime config changes
- Per-agent tool filtering

### Changed

- Improved error messages for configuration validation
- Better handling of missing configuration files

## [0.2.1] - 2024-12-15

### Added

- Workflow engine for multi-agent orchestration
- Deep research endpoint with parallel subagents
- MCP (Model Context Protocol) server support

### Fixed

- Memory management for long conversations
- Token counting accuracy for streaming responses

## [0.2.0] - 2024-12-10

### Added

- Multi-provider LLM support (Ollama, OpenAI, LlamaCpp)
- Tool calling with automatic schema generation
- JWT-based authentication
- Swagger UI for API documentation
- RAG support with semantic search
- Web search tool (no API key required)

### Changed

- Migrated to Axum web framework
- Improved streaming response handling
- Better error handling and logging

## [0.1.0] - 2024-12-01

### Added

- Initial release
- Basic chat functionality with Ollama
- SQLite database support
- Simple agent framework
- REST API endpoints

---

[0.6.3]: https://github.com/dirmacs/ares/compare/v0.6.2...v0.6.3
[0.5.0]: https://github.com/dirmacs/ares/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/dirmacs/ares/compare/v0.3.3...v0.4.0
[0.3.3]: https://github.com/dirmacs/ares/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/dirmacs/ares/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/dirmacs/ares/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/dirmacs/ares/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/dirmacs/ares/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/dirmacs/ares/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/dirmacs/ares/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/dirmacs/ares/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/dirmacs/ares/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/dirmacs/ares/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dirmacs/ares/releases/tag/v0.1.0
