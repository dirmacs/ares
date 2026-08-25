# Changelog

All notable changes to ARES are documented here. This project follows [Semantic Versioning](https://semver.org/).

---

## Unreleased

**Reactive fiber lifecycle, guarded file writes, and opt-in intelligence controls.**

### Added

**Kernel (`ares-cordis`)**

- Reactive `Pending` fiber state: an `Active` fiber whose dependency is genuinely withdrawn disposes its effects (LIFO) and rests `Pending`; it reactivates through `Loading` when the provider returns. Apply errors stay terminal `Failed`; peer-version refusals over a live provider still rest `Inactive`. `Pending` fibers reserve their registry key and survive `prune_disposed`
- `EventOptions { prepend, global }` on the new `on_with` / `once_with` listener registrations, plus `emit_filtered`: per-dispatch filtering where non-global listeners are offered to a filter predicate and `global` listeners always join. Existing `on` / `once` / `emit` signatures are unchanged
- `Context::get_relaxed::<T>()`: like `get`, but serves a locally-owned value while its provider fiber transitions (`Active` / `Loading` / `Reloading` / `Unloading` / `Pending`); disposed and `Failed` owners stay refused
- Fiber state observers: `Fiber::subscribe_state` delivers every lifecycle transition to synchronous observers with panic isolation; the returned handle cancels the subscription

**Admin HTTP**

- `PATCH /admin/cordis/entries/{id}`: typed partial update driven by `cordis::loader::EntryUpdate` (`config`, `disabled`, `isolate`, `intercept`). Only present fields change; `{}` is a validated no-op that still persists and re-applies; `id` / `plugin` are deliberately not patchable (identity changes are DELETE + PUT). Replies with the post-patch entry and per-action outcomes; unknown ids answer 404

**Tools fence**

- Layer 3 write guards on `Fence`: writes require a prior `fence_read` observation unless the mode allows blind writes (`FS_NOT_OBSERVED`); `CreateIfAbsent` refuses existing paths (`FS_EXISTS`); `ReplaceIfVersion` compares the `mtime ^ size` fingerprint captured at read time (`FS_VERSION_CONFLICT`). Bytes land through a sibling temp file renamed into place, new files get `0600` on unix, errors carry structured `FS_*` codes, and a bounded 200-entry audit ring is readable through `audit_log`. Layers 0-2 behave exactly as before

**LLM**

- Retry-before-salvage JSON policy: the micro engine re-requests a malformed-JSON answer identically up to `json_retries` times (default 2) before the substring-salvage fallback runs
- Per-provider concurrency governor: optional pool setting `max_in_flight` caps simultaneous dispatches per provider, with `governor_acquire_timeout` (default 30 s) bounding the wait. Permits release only at terminal stream items, and saturation fails closed. Without the setting, behavior is unchanged and no wrappers install
- Model-profile catalog: one cross-provider `ModelProfile` table (capabilities, context window, speed tier, cost) merging the static tables with runtime catalog entries. `lean_hint` renders the whole catalog for prompt injection in well under 50 tokens, `describe_full` prints one record, and `route` picks the cheapest capable model for a modality. The catalog is opt-in; nothing wires it into default model selection

**Skills**

- Delegated-result review gate (opt-in `review_delegated_results`): nested `SkillCall` results pass a fixed-template consistency and task-fit review before integration. A rejection replaces the result with a structured rejection that keeps the original for re-dispatch; a reviewer outage passes the result through unchanged. Off by default
- Ambient enrichment (opt-in `AmbientEnrichmentConfig`): after assistant completions, two parallel micro calls classify intent and extract up to five keyword tags; the outcomes ride the existing skill-step record as `response_payload.ambient_enrichment` (no new storage). Enrichment never delays or fails the completion; failures are skipped silently. Off by default

---

## 0.9.0, 2026-08-22

**Service architecture, unified execution, wrapper removal.**

### Added

- `Execute::run` with the full resolve-create-execute pipeline and `RunTracker` observability
- `EventsService::waterfall_around`: around-middleware waterfall that runs `core` at the end; a skip of `next` skips core
- `Context::inject` waits on the `ReflectService` TypeId notifier (`ensure_notifier` + `changed`); if ReflectService is absent or the sender is dropped, it falls through to a 5ms poll
- Product events: `tools.list` / `tools.resolve` / `tools.execute`, `llm.get_client` / `llm.complete`, `llm.generate` / `llm.generate_tools` (`ConfigurableAgent`), `agent.run` (waterfall), `agent.admit` (`Dispatch::Bail`), `agent.started` (`Dispatch::Parallel`)
- Skills isolate the request `ctx` (`isolate::<Tools>(tenant_id)`) instead of opening a new root
- Skill `LlmCall` steps strictly run `Llm::complete` through the `llm.complete` waterfall; `SkillEngine` and `SkillsService` have no direct provider `generate_with_history` fallback
- Skill `ToolCall` steps run `Tools::execute` (`tools.execute` waterfall) on the tenant isolate
- `ExecutionResult` return type with resolution metadata (source tier, run ID)
- `RunTracker` trait extracted to `ares-agent` for decoupled run observability
- `Service` impl directly on `AgentRegistry` and `ConfigBasedLLMFactory` (no wrappers needed)
- `agent_config_from_user_agent` helper in `ares-agent::configurable`
- `Fiber::refresh` reruns registered plugin `apply` after epoch recompute
- `EventsService` `Parallel` returns JSON `null`; `Serial` bails on the first non-null handler result
- Store loader factory runs SQL migrations and seeds agent templates
- Overlay fills empty loader `entry.config` from `ares.toml`; TOON reloads notify `Tools` and `Execute`
- `TenantRealms` open-then-intercept on request paths; dispose on admin tenant delete
- JWT research plus remaining v1 stream/agent handlers open the tenant realm before intercept
- Isolate labels win over intercept for the same `TypeId`; unlabeled types still intercept
- Leftover `execution_stack` dual `Execute` installer removed
- Default `ares-server` library build has no axum (`http` is optional) and no longer re-exports `ProviderRegistry`
- Single `Execute` loader key (ares-agent); Overlay/`ServerRuntime` provide host extras
- JWT middleware looks up tenant claims in Store, fail-closes 401 when the tenant does not exist, then opens `TenantRealms` and intercepts `TenantContext`; user claims isolate with no dummy Free tenant
- `Llm::from_client` is the public test constructor; `no_http` no longer builds `ProviderRegistry`
- Root `ares-server` package keeps its binary; the library target serves embedders; integration tests depend on `ares-server` / `ares-http`

### Changed

- All 5 execution sites (chat, v1, scheduler, trigger, pipeline) now delegate to `Execute`
- `Tools`, `Llm`, and `Execute` public methods run through Cordis `waterfall_around` when `EventsService` is on ctx
- `SkillsService` and `SkillEngine` LLM/tool steps use those same events instead of calling the tool or client directly
- `resolve_agent` delegates to crate-private `Resolver` when available (legacy fallback retained)
- Removed `AgentRegistryService` and `LlmFactoryService` wrappers (consumers use types directly)
- Deleted deprecated `start_background_reload` function and its test
- Calculator tool registered via `ctx.plugin(CalculatorService)` in addition to legacy path
- Version bump to 0.9.0
- `Tools`, `Llm`, `Execute`, and skills remain event-first on `EventsService` waterfalls
- `run_server` still instantiates Overlay first, then remaining loader entries
- Scheduler, pipeline, and trigger domain loops remain native ARES engines behind `Execute`
- `ProviderRegistry` remains on `ares-llm` for `Llm::new` / `AgentRegistry::from_config`
- Overlay lives in `crates/ares-http/src/overlay.rs`; the server still registers the Overlay factory

---

## 0.8.0, 2026-08-21

**Service-based architecture with dependency injection.**

### Added

- `cordis` crate: typed `Context` container, `Fiber` lifecycle, `Service` trait, `RegistryService` with plugin pattern, `Loader` with config reconciliation, `EventsService` with 5 dispatch modes, `ReflectService` for hot-reload coordination.
- Unified services: `UnifiedToolService` (merges static, runtime, and MCP tools), `LlmService` (circuit breaker with failover), `AgentResolverService` (3-tier resolution: tenant, community, system).
- Handler migration: 177 handlers moved from `State<AppState>` to `State<Arc<Context>>` with `ctx.get::<T>()`.
- Admin API split from single 190KB file into 15 domain-specific modules.
- V1 API split from 73KB file into 5 modules.
- File-watch hot-reload with 500ms debounce (replaces 60s polling).
- Rhai scripting support for custom tools and services.

### Changed

- `AppState` god-struct (17-22 fields) replaced by `pub type AppState = Arc<Context>`.
- `build_router(ctx)` is the primary router constructor; `base_router` remains as deprecated shim.
- Rust toolchain updated to 1.98.
- All docs humanized (removed AI-sounding prose patterns).
- `docs/src/SUMMARY.md` now includes Cordis chapters (mapping, remedies, capabilities, baseline, YAGNI, redesign) plus Architecture.
- mdBook GH-pages rebuilt for 0.8.0 (`gh-pages` branch `docs: rebuild gh-pages book for 0.8.0 Cordis`).

---

## 0.7.3

Previous release line (see git tags). Changes tracked in git history before changelog formalization.

## 0.6.3

**Multi-provider LLM, tenant agents, and enterprise metering.**

This release transforms ARES from a single-provider system into a full multi-provider LLM platform with enterprise-grade tenant management.

### Added

- **Multi-provider LLM routing**: support for 4 providers (Groq, Anthropic, NVIDIA DeepSeek, Ollama) and 11 models through a unified API.
- **Model tier system**: `fast`, `balanced`, `powerful`, `deepseek`, and `local` tiers with automatic provider routing.
- **Tenant agent system**: agents stored in the database per tenant. Template-based provisioning with full CRUD via admin API.
- **Agent templates**: seed templates applied automatically on startup. New tenants receive a default agent set.
- **Usage metering**: `usage_events` table, `monthly_usage_cache`, and `daily_rate_limits` for tracking tokens, requests, and costs per tenant.
- **API key authentication**: `Authorization: Bearer ares_xxx` on `/v1/*` routes with tenant scoping.
- **Enterprise agent templates**: 4 specialized agent templates (`trade-classifier`, `trade-risk`, `trade-monitor`, `trade-reporter`) for the first enterprise deployment.
- **Tenant-scoped API routes**: both JWT-protected (`/api/trading/*`) and API-key (`/v1/trading/*`) endpoints.
- **Admin provisioning API**: atomic tenant creation: schema + agents + API key in a single operation.

### Changed

- Chat handler now resolves `tenant_id` from authentication context instead of hardcoded values.
- Provider configuration moved from code to `ares.toml` for runtime flexibility.
- Rate limit enforcement now operates at both the provider and tenant level.

### Fixed

- Chat handler tenant_id resolution for multi-tenant requests.

---

## 0.6.2

**Streaming and SSE support.**

### Added

- **Server-Sent Events streaming**: `POST /v1/chat/stream` endpoint for real-time token-by-token responses.
- **Stream handler**: unified streaming across all providers with consistent SSE format.
- **Context continuation**: `context_id` parameter for maintaining conversation history across requests.

### Changed

- Response format standardized to `{"response", "agent", "context_id"}` across all endpoints.

---

## 0.6.1

**Tool calling and RAG foundations.**

### Added

- **Tool calling framework**: define tools per agent. ARES manages the tool-call loop, execution, and response assembly.
- **RAG pipeline**: retrieval-augmented generation with pluggable document stores.
- **Workflow engine**: chain multiple agents into multi-step workflows with deterministic execution.

### Changed

- Agent configuration schema extended to support tool definitions and RAG settings.

---

## 0.5.0

**JWT authentication and user management.**

### Added

- **User registration and login**: `POST /api/auth/register`, `POST /api/auth/login`.
- **JWT token lifecycle**: 15-minute access tokens, refresh token rotation, logout/invalidation.
- **Role-based access**: user roles with permission checks on protected routes.
- **Admin authentication**: `X-Admin-Secret` header for internal administration endpoints.

### Changed

- All `/api/*` routes now require JWT authentication.
- Error responses standardized with `error` and `code` fields.

---

## 0.4.0

**PostgreSQL backend and multi-tenant schema.**

### Added

- **PostgreSQL integration**: full migration from in-memory storage to PostgreSQL with `sqlx`.
- **Auto-migration**: `sqlx::migrate!()` runs on startup. No manual SQL required.
- **Tenant schema**: `tenants`, `tenant_agents`, and `api_keys` tables with foreign key relationships.
- **Tenant tiers**: Free, Dev, Pro, and Enterprise tiers with configurable limits.

### Changed

- All state persistence moved from in-memory structures to PostgreSQL.
- Connection pooling via `sqlx::PgPool` with configurable pool size.

---

For the complete commit history, see the [ARES repository on GitHub](https://github.com/dirmacs/ares).
