# ARES Cordis Redesign — Architecture Handoff (Phase 7, Step 25)

**Branch:** `cordis-redesign` (`f7cfa3b` + 6-phase subagent work, forked from `main` @ `e4f3bcc`)
**Spec:** `docs/cordis-mapping.md`, `docs/cordis-remedies.md`, `docs/cordis-capabilities.md`, `docs/cordis-baseline.md`, `docs/cordis-yagni.md`
**Spike:** `crates/ares-cordis-core` (leaf, zero ARES deps) — proves temporal & spatial composability.

---

## 1. Vocabulary

| Primitive | File | Rust |
|-----------|------|------|
| **Γ^∞ Context** | `crates/ares-cordis-core/src/lib.rs` `Context{store,isolate,intercept,fiber,parent,root}` | `Context::new_root()->Arc<Context>`, `extend`, `isolate::<T>(label)`, `intercept::<T>(val)`, `provide::<T:Service>(svc)->Arc<T>` (LIFO undo onto `fiber.acc`), `get::<T:Service>()->Option<Arc<T>>` (intercept→store→parent), `fiber()` |
| **Service** | same | `trait Service: Send+Sync+'static { fn name()->&'static str; fn init(&self,ctx:&Arc<Context>)->ServiceInitFuture<'_> {Box::pin(async{Ok(None)})} fn check()->bool{true} }` `ServiceInitFuture<'a>=Pin<Box<dyn Future<Output=Result<Option<Box<dyn Disposable>>,CordisError>>+Send+'a>>` (dyn-compatible, `type_complexity` alias) |
| **Fiber** | same | `enum FiberState::Inactive{error}|Active{epoch}|Reloading|Unloading`, `struct Fiber{state,inertia:Arc<tokio::sync::Mutex<()>>, acc:Mutex<Vec<Box<dyn FnOnce()+Send>>>, epoch, injects}` with `declare_inject::<T>()`, `compute_epoch(ctx)->String` `":uid:ver:..."` monoid, `refresh(ctx).await` (recomputes epoch from `injects` + `ctx.get_version`, `Active` if satisfied else `Inactive`), `dispose().await` (LIFO), `push_undo` |
| **Effect/Disposable** | same | `trait Disposable: Send+'static {fn dispose(self:Box<Self>)}` `impl<F:FnOnce()+Send> Disposable for F`, `EffectGuard{acc:Vec<Box<dyn FnOnce()+Send>>}` `Drop` reverses, `Context::effect<E:Effect>(E)->Box<dyn Disposable>` (via `root` weak) |
| **Events** | same | `enum Dispatch::Emit/Parallel(JoinSet)/Serial/Bail/Waterfall` `struct EventsService{handlers:RwLock<HashMap<EventId,Vec<Handler>>>, bus:broadcast}` `on(event,handler)->Box<dyn Disposable>` (LIFO stub), `dispatch(event,payload,mode)` |
| **Registry** | same `loader` mod + `lib.rs` | `trait Plugin{type Config:Serialize+DeserializeOwned; type Provides:Service; fn apply(&self,ctx:&Arc<Context>,config:Self::Config)->Result<Box<dyn Disposable>,CordisError>}` `struct RegistryService{fibers:RwLock<HashMap<FiberId,Arc<Fiber>>>, provided:RwLock<HashMap<TypeId,FiberId>>, next_id}` `plugin<P:Plugin>(ctx,plugin,config)->FiberId` enforces `duplicate provider for <TypeId>` (single-source, Thm 63), `inventory`/`linkme` static placeholder + `#[cfg(feature="hmr")]` `libloading` `dlopen` stub (file-watch fallback 90% value) |
| **Epoch** | same | `fn compute_epoch(inject:&HashMap<TypeId,Symbol>)->String` `":uid1:uid2:..."` sorted, `Fiber::compute_epoch` uses `ctx.get_version(tid)` (`versions:RwLock<HashMap<TypeId,u64>>` bumped on `provide`, walked via parent) |
| **Loader** | `crates/ares-cordis-core/src/loader.rs` | `struct Entry{id,plugin:String,config:Value,disabled,is_isolate/intercept}`, `struct EntryTree(Vec<Entry>)` `fn reconcile(current:&EntryTree, desired:&EntryTree)->Vec<LoaderAction>` (`RebuildFiber|UpdateConfig|Retire|Begin`) per-field diff (id/plugin→rebuild, config→update, disabled→retire/begin), persists `config/entries.json` / `config/cordis-entries.toon` (`toon-format 0.4.1`), never `ares.toml` symlink |
| **Reflect/Notify** | `crates/ares-llm/src/provider_registry.rs` + `crates/ares-tools/src/runtime_registry.rs` stubs | `struct ReflectService{notifiers:RwLock<HashMap<TypeId,watch::Sender<()>>>, dependents:RwLock<HashMap<TypeId,Vec<FiberId>>>}` `fn notify(&self,tid:TypeId)` BFS `Fiber::refresh`, replaces 60s `ArcSwap` poll (`start_background_reload` retained as shim with `// TODO` + `reflect_notify_stub`) |

---

## 2. How to Add a New Provider / Tool / Agent (before vs after)

**Before (17 steps in `src/main.rs:296-889`):** edit `AresConfig`, `ProviderRegistry::from_config`, `ToolRegistry::with_config`, `AgentRegistry::with_dynamic_config`, `AppState{17-22 fields}` construction, `base_router(state)` wiring, plus `runtime_registry.rs` 60s poll.

**After (5-8 `plugin` calls):**
```rust
let root_ctx = Context::new_root();
let registry = Arc::new(RegistryService::new());
root_ctx.provide(registry.clone());
root_ctx.provide(EventsService::new());
// Provider
struct NvidiaProviderPlugin;
impl Plugin for NvidiaProviderPlugin {
    type Config = NvidiaConfig; type Provides = LlmService;
    fn apply(&self, ctx:&Arc<Context>, cfg:Self::Config)->Result<Box<dyn Disposable>,CordisError> {
        let svc = Arc::new(LlmService::new(cfg));
        ctx.provide(svc.clone());
        Ok(Box::new(move || {}) as Box<dyn Disposable>)
    }
}
registry.plugin(&root_ctx, NvidiaProviderPlugin, nvidia_cfg).unwrap();
// Tool
struct CalculatorPlugin;
impl Plugin for CalculatorPlugin {
    type Config = CalculatorConfig; type Provides = dyn ToolService;
    fn apply(&self, ctx:&Arc<Context>, _:Self::Config)->Result<Box<dyn Disposable>,CordisError> {
        ctx.provide(CalculatorService::new());
        Ok(Box::new(|| {}) as Box<dyn Disposable>)
    }
}
registry.plugin(&root_ctx, CalculatorPlugin, CalculatorConfig::default()).unwrap();
// Agent
registry.plugin(&root_ctx, AgentResolverService::new(tenant_db, agent_registry), ()).unwrap();
let app = build_router(root_ctx.clone());
```
All 3 registries behind one `ToolService` (`tenant runtime → fleet runtime → MCP bridge → static`), `LlmService` with `Breaker{Closed/Open/HalfOpen}` + `ModelOverride` via `ctx.intercept`, `AgentResolverService` ordered `tenant_db → community → system` with `ctx.isolate("agent", tenant_label)`.

---

## 3. How to Add a New Admin Route Group

`src/api/handlers/admin.rs` (was 5,946 lines) decomposed via `#[path]` shim (avoids `admin.rs` vs `admin/mod.rs` E0761):
```
src/api/handlers/
  admin.rs          // shim: pub mod tenants; #[path="admin/tenants.rs"] etc., keeps original handlers
  admin/
    mod.rs
    tenants.rs      // pub fn routes()->Router { Router::new() } // TODO: ctx.plugin(AdminTenantsRoutes,...)
    agents.rs
    providers.rs
    tools.rs
    schedules.rs
    triggers.rs
    pipelines.rs
    billing.rs
    mcp.rs
    fleet_secrets.rs
    connectors.rs
    health.rs
    audit.rs
  v1.rs             // similar shim
  v1/
    chat.rs
    stream.rs
    agents.rs
  routes.rs         // added build_routes(ctx:&Arc<Context>)->Router merging RouteSets via ctx.get::<...>
```
Each sub-module `impl Service` + `provide(RouteSet)` via `ctx.plugin`; `routes.rs` becomes `fn build_routes(ctx:&Arc<Context>)->Router`. Same paths/auth (`X-Admin-Secret`) preserved — only file boundaries move. `crates/ares-agents/src/configurable.rs` shows `cfg`→`Service::check()` migration: `struct PostgresService; impl Service for PostgresService { fn check()->bool{cfg!(feature="postgres")} }` and handlers use `if ctx.get::<PostgresService>().is_some()` not `#[cfg]`.

---

## 4. Dependency Graph (leaf→root build order)

```
leaf (zero ARES deps)
  crates/ares-cordis-core  ─┐  (Context/Fiber/Service/Registry/Events/Loader/Reflect)
                              │
  crates/ares-types           │ cross-cutting
  crates/ares-vector (0.1.2)  │ pure HNSW
                              ▼
  crates/ares-config ───────┬─► crates/ares-db (23k LOC, traits)
                              │         │
  crates/ares-rag ────────────┘         ▼
                              crates/ares-llm ──► crates/ares-tools (CalculatorService, ToolService Unified)
                                                     │         │
  crates/ares-auth (merge → ares-core)                │         ▼
  crates/ares-memory (merge → ares-agents)            └─► crates/ares-mcp (bridge)
                                                                  │
  crates/ares-agents (execution.rs AgentExecutionService, resolver.rs AgentResolverService, scheduler/pipeline/trigger stubs)
                                                                  ▼
  ares-server root (src/lib.rs CordisAppState/AppState shim, build_router, health_context, src/main.rs _root_ctx, src/api/handlers/admin|v1 split, src/observability gated)
```

YAGNI: `ares-auth` + `ares-memory` merged into `ares-core`/`ares-agents` (decision `docs/cordis-yagni.md`); 9 crates kept.

---

## 5. Request Lifecycle Through New Context

```
Axum middleware (api_key_auth, usage) 
  → Request extension: ctx.extend().provide(UsageContext::from_headers(req.headers())).provide(TenantId)
  → Handler State(ctx: Arc<Context>)
    → ctx.get::<AgentResolverService>().resolve(name, tenant)   // isolate per-tenant
    → ctx.get::<AgentExecutionService>().execute(req, &ctx)     // single execution site
      → ctx.get::<dyn ToolService>().resolve(name) // precedence isolate chain
      → ctx.get::<LlmService>().find_model(CapabilityRequirements) // intercept per-request ModelOverride
      → ctx.get::<EventsService>().dispatch("agent:done", payload, Bail)
    → observability sink (run_history/agent_runs) + cost/usage + token budget
  → Fiber::refresh via ReflectService::notify(TypeId) when runtime_tools/runtime_providers/NvidiaCatalog change (watch fan-out, Thm 63)
```

Streaming: `async-stream` + `broadcast` preserved via `Dispatch::Parallel`.

---

## 6. Gates Enforced in CI

- **Per-module build gate:** after each sub-step `cargo check --no-default-features --features openai,postgres,mcp` must pass (now 0.41s) + `cargo check --no-default-features` (0.88s, 16 warnings, proves `cfg` cleanup)
- **Per-phase rust-doctor gate:** `npx rust-doctor@latest . --json --scope files --base main` must show 0 new P0/P1 (ceiling rule: one P0 caps to 40, P1 to 65). Baseline `main@e4f3bcc`: `score 86 Great worst P2 (53 P2,0 P1,0 P0)`, redesigned `cordis-redesign`: `score 86 Great worst P2 (38 P2,971 P3,548 unknown,0 P0/P1)` — **no regression**, `dimensions security 100 reliability 75 maintainability 70 performance 99 dependencies 75`, 590 diagnostics total (admin stubs add `missing_docs` P3, expected). Spike file-scope: `90 Great worst P2 (47 P2,5 P3)` and `88 Great worst P2 (38 P2,155 P3,397 unknown)` — all **passed, 0 P0/P1**.
- **Spike correctness:** `cargo test -p ares-cordis-core` **12 passed** (temporal + spatial + isolate + events + epoch + inertia + registry_single_source + 5 loader round-trip/reconcile)
- **Full verification matrix (Phase 7, step 22):** `cargo check` (both feature sets) **PASS**, `cargo test -p ares-cordis-core --lib` **12/12**, `cargo test -p ares-tools --lib --features postgres,mcp calculator` **11/11**, `cargo clippy -p ares-cordis-core -- -D warnings` **PASS** (after `ServiceInitFuture` alias fixing `type_complexity`), `cargo test --doc` 0/10 ignored. Full `cargo clippy -- -D warnings` still shows baseline `dead_code` (3) + `should_implement_trait` (1) — **pre-existing, not new, tracked in `docs/cordis-baseline.md`**, not blocking per ceiling rule.
- **Capability proof (Phase 7, step 24):** 7 checklist rows (health, chat, stream, admin CRUD isolation, scheduler 70s, hot-reload TOON/DB, multi-tenant `ToolService::list` invisibility) — redeploy `cargo run --release --no-default-features --features openai,postgres,mcp` + `hurl/` + `curl` against `localhost:3000` (see `docs/cordis-capabilities.md`).

---

## 7. Evaluation for Intern/Hire (Shakedown)

- **Foundations (Phases -1 to 1)** are independently shippable: baseline+YAGNI+docs+spike prove theorems before touching business logic.
- **Phases 2-3** (Registry+AppState shim, Loader+hot-reload) can merge without 4-6 (old `AppState` paths remain deprecated shims).
- **Phases 4-6** land incrementally; each `cargo check` gate prevents breakage.
- **If `libloading` HMR too complex** (ABI fragility, `unsafe` surface) → file-watch + `Fiber::reload()` via config/TOON re-read covers 90% value, defer dynamic code behind `#[cfg(feature="hmr")]` (documented in `docs/cordis-mapping.md §11`).

---

## 8. Remaining Explicit TODOs (for next `cargo check`-gated commits)

- Wire `CalculatorService` into `ConfigurableAgent::inject_tool_service` and `src/api/handlers/chat.rs` `AgentResolverService` (shim retained, delete old `ToolRegistry` after one green commit).
- `Loader::reconcile` BFS walk `ReflectService::notify` for DB `NOTIFY/LISTEN` (Postgres) + polling fallback.
- `AgentExecutionService::execute` dedup body from 5 sites (copy `chat.rs:execute_agent` verbatim then delete scattered branches).
- `UnifiedToolService/McpRegistry` precedence final wiring + `execute_for_tenant` deletion.
- `ClientPool` breaker `Closed/Open/HalfOpen` thresholds + `check()` health.
- Admin `build_routes` merge of 13 `RouteSet`s and removal of `admin.rs` shim after one release with `#[deprecated]`.
