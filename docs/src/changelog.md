# Changelog

All notable changes to ARES are documented here. This project follows [Semantic Versioning](https://semver.org/).

---

## 0.9.0, 2026-08-22

**Service architecture, unified execution, wrapper removal.**

### Added

- `AgentExecutionService::execute_agent` with full resolve-create-execute pipeline and `RunTracker` observability
- `ExecutionResult` return type with resolution metadata (source tier, run ID)
- `RunTracker` trait extracted to `ares-agents` for decoupled run observability
- `Service` impl directly on `AgentRegistry` and `ConfigBasedLLMFactory` (no wrappers needed)
- `agent_config_from_user_agent` helper in `ares-agents::configurable`

### Changed

- All 5 execution sites (chat, v1, scheduler, trigger, pipeline) now delegate to `AgentExecutionService`
- `resolve_agent` delegates to `AgentResolverService` when available (legacy fallback retained)
- Removed `AgentRegistryService` and `LlmFactoryService` wrappers (consumers use types directly)
- Deleted deprecated `start_background_reload` function and its test
- Calculator tool registered via `ctx.plugin(CalculatorService)` in addition to legacy path
- Version bump to 0.9.0

---

## 0.8.0, 2026-08-21

**Service-based architecture with dependency injection.**

### Added

- `ares-cordis-core` crate: typed `Context` container, `Fiber` lifecycle, `Service` trait, `RegistryService` with plugin pattern, `Loader` with config reconciliation, `EventsService` with 5 dispatch modes, `ReflectService` for hot-reload coordination.
- Unified services: `UnifiedToolService` (merges static, runtime, and MCP tools), `LlmService` (circuit breaker with failover), `AgentResolverService` (3-tier resolution: tenant, community, system).
- Handler migration: 177 handlers moved from `State<AppState>` to `State<Arc<Context>>` with `ctx.get::<T>()`.
- Admin API split from single 190KB file into 15 domain-specific modules.
- V1 API split from 73KB file into 5 modules.
- File-watch hot-reload with 500ms debounce (replaces 60s polling).
- Rhai scripting support for custom tools and services.

### Changed

- `AppState` god-struct (17-22 fields) replaced by `pub type AppState = Arc<Context>`.
- `build_router(ctx)` is the primary router constructor; `base_router` retained as deprecated shim.
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
