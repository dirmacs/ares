# Cordis Redesign — YAGNI Ladder (Phase -1, Step 3)

**Date:** 2026-08-20
**Commit:** `e4f3bcc` (11 workspace crates + `ares-server` root)
**Rule:** `rust-safe-large` YAGNI ladder — walk each crate before writing any new crate. No code change in this step.

## Workspace at e4f3bcc

| Crate | Lines (rs) | Files | Deps (key) | Description |
|-------|------------|-------|------------|-------------|
| ares-types | 1,980 | 4 | axum, utoipa, chrono, serde | TenantTier, tenant models, shared API types |
| ares-config | 6,135 | 5 | toon-format, toml, arc-swap, notify, reqwest, aes-gcm | TOML/TOON config, 110 KB toml_config.rs, fleet_secrets, nvidia_catalog |
| ares-db | 23,019 | 30+ | sqlx, libsql, qdrant, lancedb, lance, chromadb, pinecone | DB + vector store clients, 20+ modules (tenants, skills, schedules, runtime_*) |
| ares-llm | 13,503 | 13 | async-openai, arc-swap, parking_lot, futures | LLM clients, provider_registry, pool, coordinator, capabilities |
| ares-agents | 9,460 | 15 | ares-llm, ares-tools, ares-db | Agent registry, resolver, configurable, orchestrator, research, memory |
| ares-tools | 7,797 | 18 | daedra, scraper, boa_engine, rmcp, arc-swap | Built-in tools, registry, runtime_registry, mcp_bridge, connectors/* |
| ares-mcp | 6,772 | 8 | rmcp, reqwest, toon-format | MCP client/server, registry, auth |
| ares-rag | 8,627 | 6 | fastembed, text-splitter, lancor, lru | Chunking, embeddings, search, reranker |
| ares-vector | 4,440 | 8 | hnsw_rs, anndists, scc, memmap2 | Pure-Rust HNSW, published crate 0.1.2 |
| ares-auth | 902 | 2 | jsonwebtoken, argon2 | JWT + argon2 only |
| ares-memory | 1,291 | 1 | chrono, serde | Single-file LRU session store |

Total workspace Rust ≈ 88k lines (excluding `src/` root ~190KB admin.rs etc. + `crates/ares-vector` published).

## Decisions

### Keep as standalone crate (justified)

- **ares-types** — KEEP. Cross-cutting types used by all crates (`TenantTier`, API DTOs). 1,980 lines is above noise threshold, and it has axum/utoipa dependencies that leaf crates need without pulling the whole server. Workspace `version.workspace` ensures single version.

- **ares-config** — KEEP but split internally. 6,135 lines, cross-cutting, but `toml_config.rs` alone is 110 KB per plan (currently split across `toml_config.rs`/`toon_config.rs`/`nvidia_catalog.rs`/`fleet_secrets.rs`). YAGNI: keep as one crate (config is a coherent domain), but Phase 5 must split by domain (`server`, `auth`, `providers`, `tools`, `agents`, `workflows`, `rag`, `billing`) behind `Service` traits — not as separate crates, as modules. Do not create 8 config crates.

- **ares-db** — KEEP but modularize internally. 23k lines is the workspace's largest crate, but it is the DB boundary (traits + implementations for postgres/turso/vectors). Splitting into `ares-db-postgres`/`ares-vector-stores` would be premature — the 6 backends share `traits.rs` and transaction logic. Instead, enforce feature-gated modules (`postgres`, `turso`, `qdrant`, etc.) and plan Phase 3 to replace polling reload with `Fiber::refresh`. Do not merge into `ares-server` — DB belongs at leaf.

- **ares-llm** — KEEP. 13.5k lines, provider-agnostic LLM abstraction, client pool, observability. Touches every agent execution path. Needs its own crate to isolate `async-openai`/`ollama-rs` deps behind features and to own `ProviderRegistry` → `LlmService` migration. Keep `openai`, `ollama` features.

- **ares-tools** — KEEP. 7.8k lines, tool registry + runtime registry + connectors. Distinct from agents/llm, owns execution semantics (`Tool` trait, `Arc<Tool>`). Will become `ToolService` in Phase 5 that composes static + runtime + MCP.

- **ares-rag** — KEEP. 8.6k lines, RAG pipeline (chunker, embeddings, reranker, cache, search). Distinct vector dependency path (`lancor`, `text-splitter`, `fastembed`). Keep alongside `ares-vector`.

- **ares-vector** — KEEP. 4.4k lines, published crate `ares-vector 0.1.2` with its own README/license, uses `hnsw_rs`/`anndists`/`scc`. Already versioned independently and excluded from the build gate (`default` but not in `cargo check --no-default-features --features openai,postgres,mcp`). Must remain leaf crate — do not merge.

### Merge (below YAGNI threshold)

- **ares-auth** (902 lines, 2 files) — MERGE into new `ares-core` or keep as leaf but question justification. Currently JWT + argon2 only, no DB, no config. Ladder: a standalone crate needs ≥2 consumers with distinct feature sets or a publishable boundary. `ares-auth` is consumed only by `ares-server` (middleware) and `ares-types` (claims). YAGNI says merge into `ares-runtime`/`ares-core` (proposed `crates/ares-context` or `crates/ares-core`). Decision: **Merge into `ares-core` (new leaf crate `ares-cordis-core`/`ares-context` will absorb auth traits) or into `ares-server` root if no core crate is created**. For the redesign, auth becomes a `Service` (`JwtService`) provided via `Context`, not a crate boundary. Path: re-export `jsonwebtoken`/`argon2` behind `JwtService` in `ares-core`, deprecate `ares-auth` with `pub use ares_core::auth::*` for one release if needed, then remove. No client-specific logic — confirm generic.

- **ares-memory** (1,291 lines, 1 file) — MERGE into `ares-agents` or `ares-core`. Currently a single `lib.rs` LRU session store (`ConversationMemory`, `MemoryStore`). No independent versioning, no external deps beyond `chrono`/`serde`. YAGNI says a 1-file crate is ceremony. Decision: **Merge into `ares-agents`** (where it is already consumed via `ares-agents/src/memory/*` and `context_provider.rs`) or into `ares-core` as `MemoryService`. The `lru = "0.16.3"` dep moves with it. Delete crate boundary; keep module `ares_agents::memory` (already exists) and promote `SessionMemoryService` as a `Service`.

### Borderline — keep with conditions

- **ares-agents** (9,460 lines) — KEEP as standalone, but do not let it absorb memory. It already has `ares-memory` as dep (circular pressure). After merging `ares-memory`, `ares-agents` becomes the orchestration crate. Consider whether `research/`, `orchestrator`, `loop_detector` belong in `ares-runtime`. YAGNI: keep `ares-agents` (orchestration is distinct from tool/provider execution), but the new `AgentExecutionService` (Phase 4) should live in `ares-agents`, not `ares-context`, to keep business logic out of the generic context primitive.

- **ares-mcp** (6,772 lines) — KEEP with deprecation path to merge into `ares-tools`. The plan flags `ToolRegistry`/`RuntimeToolRegistry`/`McpRegistry` fragmentation (P4). `ares-mcp` duplicates tool abstractions (`McpRegistry`, `McpTool`). Ideal: `ares-mcp` becomes a feature of `ares-tools` (`mcp` feature already exists in `ares-tools/Cargo.toml` → `dep:ares-mcp`). Ladder says keep as crate for now (MCP uses `rmcp` 0.12.0 with distinct transport), but Phase 5 must unify behind `ToolService` so `ares-tools` owns the trait and `ares-mcp` is just a bridge implementation. Do not create a new crate; do not merge yet — prove the `ToolService` composition first, then evaluate post-spike whether `ares-mcp` stays or collapses into `ares-tools/src/mcp_bridge.rs`.

## New Crates (per plan)

- **ares-cordis-core / ares-context** (spike) — CREATE as leaf crate per Phase 1, Step 8. Zero internal ARES deps, only `tokio`, `thiserror`, `tracing`, `anymap`/`hashbrown`, `arc-swap`. This is the Cordis primitive crate (`Context`, `Fiber`, `Effect`, `Disposable`, `EventsService`, `RegistryService`, `Loader`). It is **not** a merger target — it is the new foundation. YAGNI: start as `crates/ares-cordis-core` (or `crates/ares-context`) with ~1–2k lines, no `libloading` HMR, no WASM. Stub file-watch → `Fiber::reload()`.

- **ares-runtime / ares-core (potential)** — DEFER. Only create if `ares-auth` + `ares-memory` merged need a home that is not `ares-server` and not `ares-cordis-core`. The plan mentions `ares-runtime`/`ares-core` as optional absorbers for small crates. YAGNI says do not pre-create — first prove the spike (Phase 1) and the `AppState` → `Context` migration (Phase 2 step 12) can absorb `ares-auth`/`ares-memory` without a new crate. If AppState decomposition reveals a shared service layer, then introduce `ares-runtime` in Phase 2 as needed.

## Anti-Decisions (explicitly not doing)

- Do not create `ares-config-domains` (8 crates for server/auth/providers/tools/agents/workflows/rag/billing) — that is Phase 5 module split, not crate split.
- Do not create `ares-db-postgres`/`ares-db-vectors` splits — feature flags suffice.
- Do not create `ares-providers`/`ares-workflows` crates — current `ares-llm` + `src/workflows` boundary is sufficient; engines become `Service`s, not crates.
- Do not merge `ares-vector` despite small size — it is published and has distinct `hnsw_rs` deps and `rust-version = "1.75"` (vs 1.91 for workspace).

## Ordering

Phase -1 decision is log only. Implementation order for Phase 1–2:

1. Create `crates/ares-cordis-core` (leaf, no ARES deps) — proves Context/Fiber theorem.
2. Merge `ares-memory` into `ares-agents` (or `ares-core`) after spike — one `State<AppState>` shim commit, then delete crate.
3. Merge `ares-auth` after spike — `JwtService` in `ares-cordis-core`.
4. Keep all other 9 crates (types, config, db, llm, agents, tools, mcp, rag, vector) as-is; internal module splits happen in place.

## Verification

- No code changed in this step — decision log only (this file).
- `cargo check --no-default-features --features openai,postgres,mcp` must still pass on `cordis-redesign` after this doc commit (it will — doc-only change).
