# Changelog

All notable changes to ARES are documented here. This project follows [Semantic Versioning](https://semver.org/).

---

## 0.10.0 - 2026-08-26

**Reactive fiber lifecycle, kernel interception points, guarded file writes, and opt-in intelligence controls.**

### Added

**Kernel (`ares-cordis`)**

- Reactive `Pending` fiber state: an `Active` fiber whose dependency is genuinely withdrawn disposes its effects (LIFO) and rests `Pending`; it reactivates through `Loading` when the provider returns. Apply errors stay terminal `Failed`; peer-version refusals over a live provider still rest `Inactive`. `Pending` fibers reserve their registry key and survive `prune_disposed`
- `EventOptions { prepend, global }` on the new `on_with` / `once_with` listener registrations, plus `emit_filtered`: per-dispatch filtering where non-global listeners are offered to a filter predicate and `global` listeners always join. Existing `on` / `once` / `emit` signatures are unchanged
- `Context::get_relaxed::<T>()`: like `get`, but serves a locally-owned value while its provider fiber transitions (`Active` / `Loading` / `Reloading` / `Unloading` / `Pending`); disposed and `Failed` owners stay refused
- Fiber state observers: `Fiber::subscribe_state` delivers every lifecycle transition to synchronous observers with panic isolation; the returned handle cancels the subscription
- Kernel intercept meta-events: listeners on `internal/get`, `internal/set`, `internal/config`, `internal/update`, and `internal/listener` veto or rewrite the matching kernel operation, and `internal/dispatch` observes every non-internal dispatch with its `(mode, name, args)`. `internal/get` can replace a strict read's value, refuse the lookup (`refuse: true`), or redirect it to the parent frame; an erroring chain refuses the read. `internal/set` errors veto the provider write with the previous binding left fully intact. `internal/config`'s non-null terminal IS the effective config for that apply pass; an erroring chain rests the fiber terminal `Failed` (unchanged semantics). An `internal/update` bail skips the restart and keeps the current application, with the deferred config visible as `vetoed_config`. An `internal/listener` bail (or chain error) cancels the registration and returns an inert handle. Every consult short-circuits at map-lookup cost when no listener is registered, a re-entrancy fence keeps reads inside a chain un-intercepted, and the synchronous bridges fall open (warn + allow) on tokio flavors that cannot `block_in_place`
- Target-carrying dispatch family: `bail_from` / `waterfall_from` / `waterfall_async_from` run the Bail / Waterfall / around-waterfall chains with an optional per-dispatch `ListenerFilter`; a filtered-out listener skips that one dispatch and stays registered
- Readiness barriers: `register_with_readiness` takes a composable `ReadinessBarrier` — `ReadinessBarrier::new(pred)`, `.and(..)` / `with_readiness([a, b])` AND-composition (empty is vacuously ready), `.watching([TypeId])` re-kicks the gated fiber when those providers settle through the `ReflectService` fan-out. While the gate is closed the fiber rests inspectable `Pending` — quiet waiting that never becomes `Failed` — with the factory run once up front and strict `get` keeping the service out of consumer reach. Complements (does not replace) availability predicates, which still fail loudly to `Failed`
- Cascade batching: concurrent config updates against one provider collapse to a single dependent convergence wave. Providers mid-reapply are marked in an in-flight ledger; dependents defer during the window and converge once per settled batch instead of once per patch
- Name-keyed computed properties: `Context::register_accessor(name, Accessor::{read_only, read_write, setter_only})` installs a computed property beside the TypeId service store and returns an `EffectHandle` whose disposal removes the declaration and every alias. `Context::alias` binds an alternate name through the same registration. Typed reads surface `PropertyTypeMismatch` instead of a silent `None`; writes to a read-only property are refused with `ReadOnlyProperty`; duplicates (including alias collisions) are rejected. Accessor traffic BYPASSES the `internal/get` / `internal/set` intercept waterfalls entirely
- Layered intercept chains: intercept layers per TypeId form an ordered outermost..innermost sequence; new registrations APPEND, so the innermost layer stays effective for all existing getters (no caller breakage). `Context::intercept_chain` returns every layer in dispatch order, and `Context::chains_structurally_equal` compares two chains by shared-instance identity for restart-decision checks
- Lifecycle riders: `Fiber::update` returns `Result<(), CordisError>` — an error on the restart path propagates to the caller and the fiber stays `Active` serving its OLD configuration. An `internal/update` veto parks the deferred config in `Fiber::vetoed_config` and returns `Ok`. The `internal/config` waterfall now also covers the activation path, so rewrites apply on first activation, not only on re-applies
- Module graph fan-out (opt-in): `cordis::module_graph::ModuleGraph` maps module keys to their dependencies and, given a `ModuleReload` implementation, `change_many` computes the TRANSITIVE affected plugin set read-only FIRST, then reloads each affected plugin exactly once per transaction; a failing reload rolls back that plugin while successfully reloaded siblings stay `Active`. When a `ModuleGraph` is registered on the context, the file watcher's debounced batch fans through it; without one, watcher behavior is unchanged

**Logger**

- In-kernel `LoggerService`: bounded ring (default 1000 records) with monotonic sequences, fan-out to effect-owned `Exporter` sinks (registration returns a `Disposable` that removes the sink), and per-name level routing (`set_level`) with a service-wide fallback (`set_default_level`, default `Debug`). `enabled` gates writes before argument assembly. `Message::render` applies printf placeholders `%s %d %i %f %o %O %c %C %%` (`%o`/`%O` render compact/pretty JSON; unknown specifiers stay literal); `%c` colorizes over the ANSI16 palette by an FNV-1a hash of the logger name, `%C` adds bold. `hyphenate` / `derived_name` turn type names into `kebab-case` logger names (`HTTPServer` → `http-server`). `LoggerIntercept` overrides thresholds per fiber through `ctx.intercept` (resolved via the relaxed read); the `Context` facade (`ctx.info`, …) is a no-op when no logger is provided

**Timers**

- `cordis::timer`: six fiber-scoped primitives — `timeout`, `sleep`, `interval`, `interval_stream`, `debounce`, `throttle` — std-only on a shared wheel thread (min-heap deadlines drained under one short critical section, callbacks outside it, panics caught). Registrations under `with_current_fiber` push labeled undos onto the owning fiber, so dispose or a reactive unload cancels them; dropping a handle does NOT cancel. A disposed `Interval` stream yields exactly one final `Err(InactiveEffect)` and closes; `debounce` collapses bursts to one trailing delivery, `throttle`

**Admin HTTP**

- Structured validation errors on the same PATCH endpoint: a config pre-flight can reject with `ValidationIssue { message, path }` items aggregated in a `ValidationError`; the 4xx body then carries a machine-readable `issues` array beside the legacy `error` string (success carries none, and a failed trial leaves no stale slot behind)
- Entry moves: PATCH accepts optional `parent` / `position` (`EntryPosition`) applied move-THEN-update — an invalid placement answers 409 without touching the file or the live tree. `POST /admin/cordis/entries/{id}/move` relocates an entry together with its whole `{id}:*` subtree in one rename cascade. A valid move preserves fiber identity through in-place refresh, so consumers never observe a dispose/recreate window

**Tools fence**

- Layer 3 write guards on `Fence`: writes require a prior `fence_read` observation unless the mode allows blind writes (`FS_NOT_OBSERVED`); `CreateIfAbsent` refuses existing paths (`FS_EXISTS`); `ReplaceIfVersion` compares the `mtime ^ size` fingerprint captured at read time (`FS_VERSION_CONFLICT`). Bytes land through a sibling temp file renamed into place, new files get `0600` on unix, errors carry structured `FS_*` codes, and a bounded 200-entry audit ring is readable through `audit_log`. Layers 0-2 behave exactly as before

**LLM**

- Retry-before-salvage JSON policy: the micro engine re-requests a malformed-JSON answer identically up to `json_retries` times (default 2) before the substring-salvage fallback runs
- Per-provider concurrency governor: optional pool setting `max_in_flight` caps simultaneous dispatches per provider, with `governor_acquire_timeout` (default 30 s) bounding the wait. Permits release only at terminal stream items, and saturation fails closed. Without the setting, behavior is unchanged and no wrappers install
- Model-profile catalog: one cross-provider `ModelProfile` table (capabilities, context window, speed tier, cost) merging the static tables with runtime catalog entries. `lean_hint` renders the whole catalog for prompt injection in well under 50 tokens, `describe_full` prints one record, and `route` picks the cheapest capable model for a modality. The catalog is opt-in; nothing wires it into default model selection
- Guided-output grammar hints: `GenerationHints::guided_grammar` carries a schema-shaped value (JSON object with a `"type": "object"` root) as `response_format` `json_schema` on every OpenAI-compatible path; raw GBNF/EBNF-style text rides the provider-specific `guided_grammar` extension field on non-streaming OpenAI-compatible requests instead. Providers without a channel silently ignore the hint, and an ABSENT hint leaves the wire byte-identical
- Micro-call response cache: deterministic-class micro outcomes are served from a bounded least-recently-used map keyed by a content hash over `(model, system template, input)` — default 256 entries with a 15-minute TTL and a master switch via `MicroCacheConfig`. Hits skip the network entirely, report `latency_ms: 0`, and carry a `cache_hit` telemetry flag. Answers reached through retries or the salvage fallback are NEVER cached

**Agent**

- Per-subtask cancellation: delegated subtasks register sticky cancel tokens keyed by run/skill id; `SkillEngine::cancel_subtask()` flips a token exactly once and is honored at step boundaries alongside the existing `EmergencyStop` hook. An aborted subtask integrates nothing into the parent context
- Quote-aware delegation arguments: double-quoted segments parse as single tokens that may contain spaces and `|` separators; `--parallel` latches split-per-token mode (separators ignored), `--model` consumes exactly one token, and `--tools` enables the inner tool loop for delegated tasks. Precedence is flags > profile > global

**RAG**

- Embedding dedup per request: duplicate inputs collapse by whitespace-normalized SHA-256 content hash before the backend call on both the local and HTTP embedding paths; computed vectors fan back to every duplicate slot, so callers receive full-length results while identical texts cost exactly one backend call

**Skills**

- Delegated-result review gate (opt-in `review_delegated_results`): nested `SkillCall` results pass a fixed-template consistency and task-fit review before integration. A rejection replaces the result with a structured rejection that keeps the original for re-dispatch; a reviewer outage passes the result through unchanged. Off by default
- Self-check critique rounds (opt-in `SkillEngine::with_self_check_rounds`): nested `SkillCall` results pass up to N LLM critique rounds over a cache-stable template before integration; a verbatim reply ends the loop, and an LLM failure keeps the last good answer silently. Off by default
- Delegation hygiene: delegated sub-workflows accept only allowlisted step kinds (`delegated_step_not_allowed:`), nested tool rounds hard-cap at three (`tool_round_cap_exceeded:` aborts after exactly three rounds), and slash-command chatter lines are stripped from delegated result text before it enters the parent context

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
