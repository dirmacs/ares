# Architecture (Cordis) 0.8.0

ARES 0.8.0 is a Cordis-informed redesign (Γ^∞ = μΓ. Γ × (Γ→Γ) × Σ, *A Programming Paradigm for Spatiotemporal Composability*, Aug 2026). See `docs/cordis-mapping.md` + `ARCHITECTURE.md` (synced from `docs/cordis-redesign.md` 9d) and `docs/cordis-redesign.md` handoff for the full spec, dependency graph, request lifecycle, and verification logs.

## Context, Γ^∞

```rust
pub struct Context {
    store:     RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>, // Σ coherent table
    isolate:   RwLock<HashMap<TypeId, Symbol>>,                     // realm label
    intercept: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>, // prototype override
    fiber:     Arc<Fiber>,
    parent:    Option<Arc<Context>>,
    root:      Weak<Context>,
}
impl Context {
    pub fn new_root() -> Arc<Context>;
    pub fn extend(&self) -> Arc<Context>;
    pub fn isolate<T: Service>(&self, label: &str) -> Arc<Context>;
    pub fn intercept<T: Service>(&self, val: T) -> Arc<Context>;
    pub fn provide<T: Service>(&self, svc: T) -> Arc<T>;  // witnessed effect → fiber.acc LIFO
    pub fn get<T: Service>(&self) -> Option<Arc<T>>;      // intercept → store → parent walk
}
pub trait Service: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn init(&self, ctx: &Arc<Context>) -> ServiceInitFuture<'_> { Box::pin(async { Ok(None) }) }
    fn check(&self) -> bool { true }
}
```

The store is the `TypeId`-keyed coherent table (Cordis Σ). Isolate creates tenant realms (`ctx.isolate::<dyn ToolService>("tenant:acme")`), intercept creates prototype-chain overrides (`ctx.intercept(ModelOverride{model:"gpt-4o-mini"})`).

## Witnessed effects, LIFO disposable

```rust
pub trait Disposable: Send + 'static { fn dispose(self: Box<Self>); }
```

`provide` pushes undo onto `fiber.acc: Vec<Box<dyn FnOnce() + Send>>`. Temporal composability (Thm 61): `fiber.dispose()` reverses all effects LIFO and recovers the context snapshot.

## Fiber, state machine and epoch :uid watch

```rust
pub enum FiberState { Inactive{error: Option<CordisError>}, Active{epoch: String}, Reloading, Unloading }
pub struct Fiber {
    state:   RwLock<FiberState>,
    inertia: Arc<tokio::sync::Mutex<()>>,
    acc:     Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    injects: RwLock<HashMap<TypeId, Symbol>>,
    epoch:   RwLock<String>, // ":uid:ver:..."
}
```

`Fiber::compute_epoch` sorts `HashMap<TypeId,Symbol>` into `":uid1:uid2:..."` monoid from `ctx.get_version(TypeId)`. `ReflectService{notifiers: HashMap<TypeId,watch::Sender<()>>, dependents: HashMap<TypeId,Vec<FiberId>>}` `notify(tid)` BFS walks dependents + `tokio::sync::watch` fan-out → `Fiber::refresh` (replaces 60 s `ArcSwap` polls).

## Events, 5 dispatch modes

`Dispatch::Emit/Parallel(JoinSet)/Serial/Bail/Waterfall` via `HashMap<EventId,Vec<Handler>>` with `broadcast` and `tower::Service` for waterfall.

## Loader, EntryTree reconcile (HMR)

```rust
pub struct Entry { id: String, plugin: String, config: Value, disabled: bool, isolate: Option<String>, intercept: HashMap<String, Value> }
pub struct EntryTree(pub Vec<Entry>);
pub enum LoaderAction { RebuildFiber{ id }, UpdateConfig{ id }, Retire{ id }, Begin{ id } }
pub fn reconcile(current: &EntryTree, desired: &EntryTree) -> Vec<LoaderAction>;
```

The loader persists per-field diffs to `config/entries.json` or `config/cordis-entries.toon` (`toon-format 0.4.1`), never to the `ares.toml` symlink. Confluence (Thm 73) holds. File watching in `crates/ares-cordis-core/src/watcher.rs` uses 500 ms debounce and calls `ReflectService::notify` to trigger `Fiber::refresh` (covers 90 percent of HMR, `libloading` remains behind `#[cfg(feature="hmr")]`).

## 8-plugin wiring via Context::plugin

Seventeen sequential `run_server` steps become eight `root_ctx.plugin(...).await` calls (single-source guard `duplicate provider for <TypeId>`):

```rust
let root_ctx = Context::new_root();
root_ctx.provide(Arc::new(RegistryService::new()));
root_ctx.provide(Arc::new(EventsService::new()));
root_ctx.plugin(ConfigService(config_manager.clone())).await?;
root_ctx.plugin(CatalogService(catalog.clone())).await?;
root_ctx.plugin(ProviderRegistryService(provider_registry.clone())).await?;
root_ctx.plugin(AuthServiceWrapper(auth_service.clone())).await?;
root_ctx.plugin(AgentServiceWrapper{ registry: agent_registry.clone(), .. }).await?;
root_ctx.plugin(ToolServiceWrapper{ registry: tool_registry.clone(), runtime_registry: runtime_tool_registry.clone() }).await?;
root_ctx.plugin(SchedulerService::new(db.clone(), execution.clone(), 60_000)).await?;
root_ctx.plugin(HealthJobService::default()).await?;
// + PipelineService / TriggerService / SkillsService / WorkflowService (no tick, inject AgentExecutionService)
let app = build_router(root_ctx);
```

`inventory::submit!{CordisInventory{name:"ConfigService"}}` static registration (preferred over `libloading` dev HMR).

## Unified services

- ToolService: `tenant runtime -> fleet runtime -> MCP bridge -> static`, `ctx.isolate(tenant)` gives disjoint sets.
- LlmService: breaker `Closed/Open/HalfOpen` (5/30 s) + `ModelOverride` via `ctx.intercept`.
- AgentResolverService: ordered `tenant DB -> community -> system`, `ctx.isolate`.
- AgentExecutionService: single `execute(req,ctx)` for chat, v1, scheduler, pipeline, and trigger.
- SchedulerService, PipelineService, TriggerService, SkillsService, and WorkflowService own their DB tables and inject `AgentExecutionService`; `build_routes(ctx)` merges `RouteSet`s.

## Handler migration, 177 State<Arc<Context>>

`src/lib.rs` removes `pub struct AppState{17-22 fields}` and replaces it with `pub type AppState = Arc<Context>`. Every handler moves from `State<AppState>` to `State<Arc<Context>>` and uses `ctx.get::<Service>()` via 18 wrappers in `src/context_services.rs`. `admin.rs` shrinks from 3059 to 165 lines in 15 files, `v1.rs` from 1074 to 161 in 5 files, and `cfg(feature)` is no longer used in handlers (replaced by `Service::check()`.

## Scheduler HMR and verification

`SchedulerService` has 361 lines, a `60_000` ms tick with catch-up (`cron` crate), and `select! tick+watch` with `NOTIFY/LISTEN`. File-watch logs `Configuration hot-reloaded successfully via Cordis watch` on random-port E2E `39476`/`39120` (`curl /health` returns 200). See `ARCHITECTURE.md` sections 6 through 9d and `docs/cordis-baseline.md` for the full verification matrix (`cargo check` both, `clippy -D warnings`, `rust-doctor` 86 to 86 Great, `cargo test` 15/15 + 193/193).
