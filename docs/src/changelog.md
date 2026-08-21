# Changelog

All notable changes to ARES are documented here. This project follows [Semantic Versioning](https://semver.org/).

---

## 0.8.0, 2026-08-21

**Cordis redesign, Context/Fiber/Service/Loader + handler migration.**

Ground-up Rust redesign that adopts Cordis ideas (Γ^∞ = μΓ. Γ × (Γ→Γ) × Σ) for modularity, safe runtime reconfiguration, and tech-debt removal, while preserving all capabilities (multi-provider LLM, tool calling, RAG, MCP client+server, multi-tenant auth, scheduler/pipeline/trigger/skill/workflow engines, hot-reload).

### Added

- **Cordis core** (`crates/ares-cordis-core` leaf, zero ARES deps): `Context{store+isolate+intercept+fiber+parent+root}`, witnessed effects LIFO `Disposable` + `EffectGuard`, `TypeId`-keyed coherent table, Fiber states `Inactive/Reloading/Active/Unloading` + inertia `Mutex` + epoch `:uid` watch fan-out, Events 5 modes `Emit/Parallel/Serial/Bail/Waterfall` (`broadcast`+`JoinSet`/`tower::Service`), Loader `EntryTree` reconcile (`RebuildFiber/UpdateConfig/Retire/Begin`), `ReflectService` `notify` BFS + `watch` fan-out, file-watch HMR `watch_many` 500 ms debounce (90% value, `libloading` deferred behind `hmr`).
- **Service wiring**: 8 `root_ctx.plugin(...).await` calls replace 17 sequential `run_server` steps, `ConfigService` → `CatalogService` → `ProviderRegistryService` → `AuthServiceWrapper` → `AgentServiceWrapper` → `ToolServiceWrapper` → `SchedulerService` (60 000 ms tick + catch-up) → `HealthJobService` (inventory health loop), plus `PipelineService`/`TriggerService`/`SkillsService`/`WorkflowService` (downstream-triggered, inject `AgentExecutionService`). Command: `root_ctx.plugin(ConfigService).plugin(CatalogService).plugin(ProviderRegistryService).plugin(AuthServiceWrapper).plugin(AgentServiceWrapper).plugin(ToolServiceWrapper).plugin(SchedulerService).plugin(HealthJobService)`.
- **Unified services**: `ToolService` precedence `tenant runtime → fleet runtime → MCP bridge → static` with `ctx.isolate`; `LlmService` breaker `Closed/Open/HalfOpen` (5/30 s) + `ModelOverride` via `ctx.intercept`; `AgentResolverService` ordered `tenant DB → community → system` with `ctx.isolate`; single `AgentExecutionService` for all 5 call sites; `SchedulerService` real tick via `cron` crate + `NOTIFY/LISTEN`.
- **Handler migration**: 177 handlers `State<AppState> → State<Arc<Context>>` + `ctx.get::<Service>()`, `AppState` struct deleted (`src/lib.rs` → `pub type AppState = Arc<Context>` alias), `admin.rs` 3059→165 thin shards (15 files `admin/*`), `v1.rs` 1074→161 thin shards (5 files), `cfg(feature)` 0 in handlers via `Service::check()`.

### Changed

- `src/lib.rs` god-struct eliminated; `build_router(ctx: Arc<Context>)` is primary, `base_router` deprecated shim retained one release.
- `run_server` shrinks from 17 steps to ~8 `plugin` calls; `inventory` static registration replaces manual wiring.
- `rust-version = "1.98"` stable, Axum 0.8 `:param` retained, `features` both `openai,postgres,mcp` and `no-default` must pass (`bkataru` + `ares.toml` symlink ignored).
- Generic, provider-agnostic, zero client-specific code preserved.

### Docs

- `README.md` updated with Architecture (Cordis) section (Context/Fiber/Service/Loader, 8 plugin wiring, unified services, migration counts, HMR).
- New `docs/src/platform/architecture.md` (synced from `ARCHITECTURE.md`/`docs/cordis-redesign.md` 9d).
- `docs/src/SUMMARY.md` now includes Cordis chapters (mapping, remedies, capabilities, baseline, YAGNI, redesign) plus Architecture.
- mdBook GH-pages rebuilt for 0.8.0 (`gh-pages` branch `docs: rebuild gh-pages book for 0.8.0 Cordis`).

---

## 0.7.3

Previous release line (see git tags). Changes tracked in git history before changelog formalization.

## 0.6.3

**Multi-provider LLM, tenant agents, and enterprise metering.**

This release transforms ARES from a single-provider system into a full multi-provider LLM platform with enterprise-grade tenant management.

### Added

- **Multi-provider LLM routing**, Support for 4 providers (Groq, Anthropic, NVIDIA DeepSeek, Ollama) and 11 models through a unified API.
- **Model tier system**, `fast`, `balanced`, `powerful`, `deepseek`, and `local` tiers with automatic provider routing.
- **Tenant agent system**, Agents stored in the database per tenant. Template-based provisioning with full CRUD via admin API.
- **Agent templates**, Seed templates applied automatically on startup. New tenants receive a default agent set.
- **Usage metering**, `usage_events` table, `monthly_usage_cache`, and `daily_rate_limits` for tracking tokens, requests, and costs per tenant.
- **API key authentication**, `Authorization: Bearer ares_xxx` on `/v1/*` routes with tenant scoping.
- **Kasino enterprise agents**, 4 specialized agent templates (`kasino-classifier`, `kasino-risk`, `kasino-transaction`, `kasino-report`) for the first enterprise client.
- **Kasino API routes**, Both JWT-protected (`/api/kasino/*`) and API-key (`/v1/kasino/*`) endpoints.
- **Admin provisioning API**, Atomic tenant creation: schema + agents + API key in a single operation.

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

- **Server-Sent Events streaming**, `POST /v1/chat/stream` endpoint for real-time, token-by-token responses.
- **Stream handler**, Unified streaming across all providers with consistent SSE format.
- **Context continuation**, `context_id` parameter for maintaining conversation history across requests.

### Changed

- Response format standardized to `{"response", "agent", "context_id"}` across all endpoints.

---

## 0.6.1

**Tool calling and RAG foundations.**

### Added

- **Tool calling framework**, Define tools per agent. ARES manages the tool-call loop, execution, and response assembly.
- **RAG pipeline**, Retrieval-augmented generation with pluggable document stores.
- **Workflow engine**, Chain multiple agents into multi-step workflows with deterministic execution.

### Changed

- Agent configuration schema extended to support tool definitions and RAG settings.

---

## 0.5.0

**JWT authentication and user management.**

### Added

- **User registration and login**, `POST /api/auth/register`, `POST /api/auth/login`.
- **JWT token lifecycle**, 15-minute access tokens, refresh token rotation, logout/invalidation.
- **Role-based access**, User roles with permission checks on protected routes.
- **Admin authentication**, `X-Admin-Secret` header for internal administration endpoints.

### Changed

- All `/api/*` routes now require JWT authentication.
- Error responses standardized with `error` and `code` fields.

---

## 0.4.0

**PostgreSQL backend and multi-tenant schema.**

### Added

- **PostgreSQL integration**, Full migration from in-memory storage to PostgreSQL with `sqlx`.
- **Auto-migration**, `sqlx::migrate!()` runs on startup. No manual SQL required.
- **Tenant schema**, `tenants`, `tenant_agents`, and `api_keys` tables with foreign key relationships.
- **Tenant tiers**, Free, Dev, Pro, and Enterprise tiers with configurable limits.

### Changed

- All state persistence moved from in-memory structures to PostgreSQL.
- Connection pooling via `sqlx::PgPool` with configurable pool size.

---

For the complete commit history, see the [ARES repository on GitHub](https://github.com/dirmacs/ares).
