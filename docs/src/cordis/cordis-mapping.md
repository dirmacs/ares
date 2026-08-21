# Cordis → rust mapping (Phase 0, step 5)

**Source:** DeepSeek Cordis paper ("A Programming Paradigm for Spatiotemporal Composability", Aug 2026, `cordiverse/cordis` + `cordiverse/paper`) and DeepSeek Harness (`deepseek-ai/deepseek-harness`, TS, ~60 packages, 12 layers).
**Target:** ARES `/opt/ares` (dirmacs/ares v0.7.3, 11 crates + `ares-server` root, Rust 1.91, Tokio/Axum).
**This doc is strategy only**, no code changes. Spike crate `crates/ares-cordis-core` (Phase 1) must prove the theorems before adoption.

---

## 1. core equation

Cordis: `Γ^∞ = μΓ. Γ × (Γ → Γ) × Σ`, unified context that lifts effect systems (revertible mutations) and coeffect systems (typed dependency declarations) to runtime.

Rust mapping:

```rust
pub struct Context {
    // Γ — value environment (store)
    store: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>, // Σ impl, see §3
    // Isolate table: TypeId → Symbol (scoped identity)
    isolate: RwLock<HashMap<TypeId, Symbol>>,
    // Intercept table: TypeId → override (prototype-chain)
    intercept: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    // Fiber that owns this context's lifecycle
    fiber: Arc<Fiber>,
    // Parent for hierarchical lookup (prototype chain)
    parent: Option<Arc<Context>>,
    // Root for epoch-computation reachability
    root: Weak<Context>,
}
```

- `Context::new_root() -> Arc<Context>` creates `Fiber::Inactive`.
- `Context::extend(&self) -> Arc<Context>` creates child with `parent = Some(self)` (lexical scope / request scope).
- `Context::provide::<T: Service>(&self, svc: T)` inserts `TypeId::of::<T>() → Arc<T>` into `store`; witnessed by effect.
- `Context::get::<T: Service>(&self) -> Option<Arc<T>>` walks `store` → `intercept` → `parent.store` (coeFFECT lookup).
- `Context::isolate::<T>(&self, label: &str) -> Arc<Context>` creates child whose `isolate[TypeId::of::<T>()] = Symbol(label)`.
- `Context::intercept::<T>(&self, override: T) -> Arc<Context>` creates child whose `intercept[TypeId::of::<T>()] = override`.

No `unsafe`. No `libloading` in spike (stubbed); YAGNI HMR deferred behind `#[cfg(feature = "hmr")]`.

---

## 2. witnessed effects, temporal composability

Cordis witnessed effect function: `(Γ → Γ) × (Γ → Γ)` pair (do + undo) with LIFO accumulator for revertible mutations. Guarantees: if fiber disposes, all effects it applied are reverted in reverse order.

Rust:

```rust
pub trait Disposable: Send + 'static {
    fn dispose(self: Box<Self>);
}

pub struct EffectGuard {
    // LIFO accumulator — Box<dyn FnOnce() + Send>
    acc: Vec<Box<dyn FnOnce() + Send>>,
}

impl Drop for EffectGuard {
    fn drop(&mut self) {
        while let Some(undo) = self.acc.pop() { undo(); } // reverse order
    }
}

pub trait Effect: Send + Sync + 'static {
    fn apply(&self, ctx: &Context) -> Box<dyn Disposable>;
}

// Helper on Context — mirrors Cordis Context::effect
impl Context {
    pub fn effect<E: Effect>(&self, eff: E) -> Box<dyn Disposable> {
        let guard = eff.apply(self);
        self.fiber.accumulator.lock().push({
            let ptr = /* capture undo closure */;
            Box::new(move || { /* undo */ })
        });
        guard
    }
}
```

**Spike verification (Phase 1, §8):** temporal composability test
```rust
#[tokio::test]
async fn temporal_composability() {
    let ctx = Context::new_root();
    let fiber = ctx.plugin(FooService::new(), FooConfig::default()).await;
    ctx.provide(BarService(42));
    assert_eq!(ctx.get::<BarService>().unwrap().0, 42);
    fiber.dispose().await;
    assert!(ctx.get::<BarService>().is_none());
    assert_eq!(ctx.snapshot(), pre_plugin_snapshot);
}
```
Must hold before Phase 2.

---

## 3. coeffect table Σ, spatial composability

Cordis Σ is a `TypeId`-keyed table of dependency declarations (`inject = ["foo"]`). Fiber recomputes epoch from dependency UIDs; if epoch unchanged, no reload.

Rust `anymap`/`typemap` equivalent (hand-rolled to avoid extra dep in spike):

```rust
pub struct CoeffectTable {
    // TypeId → (TypeId, Symbol, UID)
    injects: HashMap<TypeId, (Symbol, String)>, // String = epoch fragment ":uid"
}

impl CoeffectTable {
    pub fn declare<T: Service>(&mut self, label: Symbol) {
        self.injects.insert(TypeId::of::<T>(), (label, uid_for::<T>(label)));
    }
    pub fn epoch(&self) -> String {
        // Monoid over concatenation, per Cordis: ":uid1:uid2:..."
        let mut frags: Vec<_> = self.injects.values().map(|(_, uid)| uid).cloned().collect();
        frags.sort();
        frags.join(":")
    }
}
```

- `aranymap` crate is not needed; `HashMap<TypeId, Box<dyn Any + Send + Sync>>` with `TypeId::of::<T>()` suffices.
- `Symbol` is `Arc<str>` or `&'static str` for `isolate` labels (e.g., `tenant:abc`).
- Handlers currently take `State<AppState>` (17,22 fields, `src/lib.rs:230`). New handlers: `State<Arc<Context>>` + `ctx.get::<T>()` where `T` is declared as `inject`. Example:
 ```rust
  // Before (P0 god-struct):
  async fn chat(State(state): State<AppState>, ...) -> Response
  
  // After (decomposed Context):
  async fn chat(State(ctx): State<Arc<Context>>, ...) -> Response {
      let exec = ctx.get::<dyn AgentExecutionService>().expect("no execution service");
      exec.execute(req, &ctx).await
  }
  ```

**Spike verification:** spatial composability
```rust
#[tokio::test]
async fn spatial_composability() {
    let ctx = Context::new_root();
    let consumer = ConsumerService::new(inject: vec![TypeId::of::<FooService>()]);
    let fid = ctx.plugin(consumer, Config::default()).await;
    assert_eq!(ctx.fiber_state(fid), FiberState::Inactive); // dep missing
    ctx.provide(FooService);
    assert_eq!(ctx.fiber_state(fid), FiberState::Active); // auto-reload
    ctx.provide(FooService::v2()); // re-provide
    assert_eq!(ctx.fiber_epoch(fid).prev, ":foo_v1");
    assert_eq!(ctx.fiber_epoch(fid).current, ":foo_v2"); // reload triggered
}
```

---

## 4. isolate & intercept

### Isolate (spatial scoping)

Cordis `isolate("name", label)` creates realm where `provide`/`inject` are scoped.

Rust:
```rust
impl Context {
    pub fn isolate<T: Service>(&self, label: impl Into<Symbol>) -> Arc<Context> {
        let child = self.extend();
        child.isolate.write().insert(TypeId::of::<T>(), label.into());
        child
    }
}

// Usage: per-tenant tool isolation (P10, Phase 3)
let tenant_ctx = root_ctx.isolate::<dyn ToolService>("tenant:acme");
tenant_ctx.provide(TenantToolService::new(tenant_id));
// tenant_ctx.get::<dyn ToolService>() returns tenant-scoped service
// root_ctx.get::<dyn ToolService>() still returns fleet service
```

### Intercept (prototype-chain override)

Cordis `intercept("key", config)` overrides a coeffect without mutating the provider.

Rust:
```rust
impl Context {
    pub fn intercept<T: Service>(&self, override_val: T) -> Arc<Context> {
        let child = self.extend();
        child.intercept.write().insert(TypeId::of::<T>(), Arc::new(override_val) as Arc<dyn Any + Send + Sync>);
        child
    }
}

// Usage: per-request model pinning (P10, Phase 5)
let req_ctx = root_ctx.intercept(ModelOverride { model: "gpt-4o-mini".into() });
let llm = req_ctx.get::<dyn LlmService>().unwrap(); // sees override via prototype walk
```

Lookup order (must walk): `intercept → store → parent.intercept → parent.store → ... → root`.

---

## 5. fiber lifecycle (with inertial lock)

Cordis Fiber states: `Inactive → Reloading → Active → Unloading` plus `Inertia` lock to serialize transitions (Thm 63, guarded withdrawal: provider does not withdraw until dependents deactivate).

Rust:

```rust
pub enum FiberState {
    Inactive { error: Option<AppError> },
    Reloading { iter: Box<dyn EffectIterator>, acc: EffectAcc, committed: CommittedView },
    Active { acc: EffectAcc, committed: CommittedView },
    Unloading { acc: EffectAcc, committed: CommittedView, outcome: Option<AppError> },
}

pub struct Fiber {
    state: RwLock<FiberState>,
    inertia: Arc<tokio::sync::Mutex<()>>, // serialize transitions
    acc: Mutex<EffectAcc>, // Vec<Box<dyn FnOnce() + Send>>
    epoch: RwLock<String>, // computed epoch
    injects: CoeffectTable,
    committed: CommittedView, // snapshot for rollback
}

impl Fiber {
    pub async fn refresh(&self) {
        let _guard = self.inertia.lock().await; // Thm 63
        let new_epoch = compute_epoch(&self.injects);
        if *self.epoch.read() == new_epoch { return; } // no change
        self.reload().await;
    }

    async fn reload(&self) { /* iterate effects, recompute, commit or rollback */ }
    async fn dispose(self: Arc<Self>) { /* LIFO undo, state → Inactive */ }
}
```

- `EffectIter` is `Box<dyn Iterator<Item = Box<dyn Effect>> + Send>`, each `Service::init` yields effects.
- `CommittedView` is `HashMap<TypeId, Arc<dyn Any>>` snapshot taken at `Active` entry; used for rollback on failure.
- `notify` (see §7) triggers `Fiber::refresh()` via BFS over dependent fibers.

File placement: `crates/ares-cordis-core/src/fiber.rs` (spike) → later `crates/ares-context/src/fiber.rs`.

---

## 6. epoch, hash of dependency UIDs

Cordis epoch is monoid `":uid1:uid2:..."` (concatenation). Fiber skips reload if epoch unchanged.

Rust:

```rust
pub fn compute_epoch(injects: &CoeffectTable) -> String {
    // Monoid over concatenation per paper §4.3
    let mut frags: Vec<String> = injects.uids_sorted();
    if frags.is_empty() { return ":".into(); }
    format!(":{}", frags.join(":"))
}

// uid_for<T> = format!("{}:{}", std::any::type_name::<T>(), label)
// Example: epoch = ":ares_llm::LlmService:tenant_acme:ares_tools::ToolService:tenant_acme"
```

- Epoch is String, not hash, paper uses concatenation for debuggability; if perf matters, switch to `sha2` hash later.
- `Fiber::refresh` compares `self.epoch.read()` vs `compute_epoch(&self.injects)`; logs diff via `tracing::debug!`.

---

## 7. notify, reactive recomputation (tokio::sync::watch)

Cordis `notify` is fan-out to dependent fibers. In TS Harness it's `EventEmitter`; in Rust it's `tokio::sync::watch`.

```rust
pub struct ReflectService {
    // TypeId of changed service → watch channel sender
    notifiers: RwLock<HashMap<TypeId, watch::Sender<()>>>,
    // dependency graph: provider TypeId → [dependent FiberId]
    dependents: RwLock<HashMap<TypeId, Vec<FiberId>>>,
}

impl ReflectService {
    pub fn notify(&self, changed: TypeId) {
        // BFS walk dependents
        let deps = self.dependents.read().get(&changed).cloned().unwrap_or_default();
        for fid in deps {
            if let Some(sender) = self.notifiers.read().get(&changed) {
                let _ = sender.send(()); // fan-out, ignore closed receivers
            }
            // also trigger Fiber::refresh via task
            tokio::spawn({
                let fiber = self.fiber_for(fid);
                async move { fiber.refresh().await }
            });
        }
    }
}

// DB-backed source example (replaces 60s poll in runtime_registry.rs etc.):
// On Postgres NOTIFY (or polling fallback every 60s if no NOTIFY), call:
// ctx.get::<ReflectService>().unwrap().notify(TypeId::of::<RuntimeToolService>());
```

Replaces: `RuntimeToolRegistry::start_background_reload` (60s poll, `crates/ares-tools/src/runtime_registry.rs`), `ProviderRegistry` poll (`crates/ares-llm/src/provider_registry.rs`), `NvidiaCatalogCache::start_background_refresh` (`crates/ares-config/src/nvidia_catalog.rs`).

---

## 8. events, 5 dispatch modes

Cordis Events: `emit` / `parallel` / `serial` / `bail` / `waterfall` typed bus.

Rust:

```rust
#[derive(Clone, Copy, Debug)]
pub enum Dispatch {
    Emit,      // fire-and-forget, no return, no error propagation
    Parallel,  // tokio::JoinSet, collect all, fail-open (one handler error doesn't cancel others)
    Serial,    // sequential, fail-open
    Bail,      // sequential, fail-fast (first error aborts)
    Waterfall, // sequential, each handler receives previous handler's output (chained)
}

pub struct EventsService {
    handlers: RwLock<HashMap<EventId, Vec<Handler>>>, // Handler = Box<dyn Fn(Value) -> Future<Output=Result<Value>> + Send>
    bus: broadcast::Sender<EventEnvelope>, // tokio::sync::broadcast for cross-task fan-out
}

impl EventsService {
    pub fn on(&self, event: EventId, handler: Handler) -> Box<dyn Disposable> {
        self.handlers.write().entry(event).or_default().push(handler);
        // return Disposable that removes handler on dispose (LIFO undo)
        Box::new(RemoveHandler { event, idx: len - 1 })
    }

    pub async fn dispatch(&self, event: EventId, payload: Value, mode: Dispatch) -> Result<Value> {
        match mode {
            Dispatch::Emit => { self.bus.send(envelope(payload)); Ok(Value::Null) }
            Dispatch::Parallel => { /* JoinSet */ }
            Dispatch::Serial => { /* loop */ }
            Dispatch::Bail => { /* loop with bail */ }
            Dispatch::Waterfall => { /* chain payload through handlers */ }
        }
    }
}
```

Mapping from TS Harness (12 layers, ~60 packages), in Rust, one crate suffices; do not replicate layering ceremony. `EventsService` is a `Service` itself (`ctx.provide(EventsService::new())`), so any fiber can `ctx.get::<EventsService>().unwrap().on(...)`.

---

## 9. loader & config reconciliation (Declarative)

Cordis Loader: `Entry { id, plugin, config, disabled, isolate, intercept }` + `EntryTree(Vec<Entry>)` persisted to `config/entries.json` (or `config/cordis-entries.toon` via `toon-format 0.4.1`). `Loader::reconcile(current, desired)` diffs incrementally.

Rust (Phase 3, `crates/ares-context/src/loader.rs` or `crates/ares-config`):

```rust
#[derive(Serialize, Deserialize, Clone)]
pub struct Entry {
    pub id: String,               // fiber id, e.g. "tool:calculator"
    pub plugin: PluginId,         // e.g. "ares_tools::CalculatorService"
    pub config: serde_json::Value,// Plugin::Config serialized
    pub disabled: bool,
    pub isolate: Option<String>,  // e.g. Some("tenant:acme")
    pub intercept: HashMap<String, Value>,
}
pub struct EntryTree(pub Vec<Entry>);

impl Loader {
    pub fn reconcile(&self, current: &EntryTree, desired: &EntryTree) {
        // per-field dispatch (paper §5):
        // id/plugin change → rebuild fiber (dispose + new)
        // config change → fiber.update(new_config)
        // disabled toggle → fiber.retire() / fiber.begin()
    }
}
```

Persistence: `config/entries.json` (or `config/cordis-entries.toon`) separate from `ares.toml` symlink (`/opt/ares-config/ares.toml`), do not conflict (see Assumptions in plan). Reuse `toon-format` serialization.

---

## 10. Plugin & RegistryService

Cordis plugins are `FnOnce(&Context, Config) -> Result<Disposable>` or struct with `apply`. Registry enforces single-source discipline.

Rust (Phase 2, `crates/ares-cordis-core/src/registry.rs`):

```rust
pub trait Plugin: Send + Sync + 'static {
    type Config: Serialize + DeserializeOwned + Send + Sync;
    fn apply(&self, ctx: &Context, config: Self::Config) -> Result<Box<dyn Disposable>>;
}

pub struct RegistryService {
    fibers: RwLock<HashMap<FiberId, Arc<Fiber>>>,
}

impl RegistryService {
    pub fn plugin<P: Plugin>(&self, plugin: P, config: P::Config) -> Result<FiberId> {
        // check duplicate provider: no two fibers may provide same TypeId in same isolate realm
        // if violation: return Err(AppError::Configuration("duplicate provider for TypeId"))
        // else: create Fiber, store, return FiberId
    }
}
```

Static registration (preferred production): `inventory`/`linkme` (compile-time plugin set), real surface is `RegistryService::plugin` (single-source discipline); `inventory::submit!` / `linkme::distributed_slice` of `fn(&Arc<Context>) -> Result<FiberId, CordisError>` is the future shortcut once crate count stabilizes (see `crates/ares-cordis-core/src/lib.rs` HMR section and `Wiring` task). Dynamic HMR (dev only, behind `#[cfg(feature = "hmr")]` **off by default**): `libloading` path that `dlopen`s `.so` and calls `Plugin::apply` via `extern "C"`; if `libloading` ABI fragility blocks (Rust `1.91` toolchain coupling, `unsafe` soundness), fall back to file-watch + full fiber reload (re-read config/TOON), 90% of value per plan, see `watcher` fallback below.

### HMR YAGNI decision (plan Assumptions §Contingencies)

**Decision: DEFER `libloading` HMR, keep file-watch + `Fiber::reload` as production path.**

- Rationale: `libloading::Library::new` + `Symbol<extern "C">` requires `unsafe`, a stable `repr(C)` ABI boundary, and the `.so` to be built with the exact same Rust toolchain (`1.91`). ABI drift across patches, `rust-doctor` soundness flags, and `Box::leak` ownership hazards make `libloading` too brittle for a generic runtime. As plan contingency states: "If `libloading` HMR proves too complex for Rust (dynamic library ABI fragility, `unsafe` surface), fall back to file-watch + full fiber reload without dynamic code swapping … `file watcher still triggers Fiber::reload()` by re-reading config/TOON, which already covers 90% of self-evolution value. Dynamic code HMR can be deferred to a later phase behind `#[cfg(feature = "hmr")]` without blocking the core redesign."
- Fallback implemented: `crates/ares-cordis-core/src/watcher.rs` (`watch_many` / `watch_cordis_entries`) uses `notify::RecommendedWatcher` (debounced `500 ms` + `100 ms` settle, same as `AresConfigManager::start_watching`) to watch `config/agents/*.toon` (recursive) and `config/entries.json` (or `config/cordis-entries.toon` parent dir). On `Modify`/`Create` it calls `ReflectService::notify(tid)` which BFS-walks `dependents` and spawns `Fiber::refresh` (epoch recompute via `compute_epoch`). No restart, no `libloading`. Logs `Configuration hot-reloaded successfully via Cordis watch` (generalizes `AresConfigManager`'s `Configuration hot-reloaded successfully` which is already proven on random-port E2E `39476`/`39120`, see `docs/cordis-redesign.md` §9/9b).
- Stub preserved: `crates/ares-cordis-core/src/hmr.rs` is `#[cfg(feature = "hmr")]` (Cargo feature `hmr = ["dep:libloading"]`, off by default). It shows `libloading::Library::new` + `get::<HmrEntryFn>` + owned `HmrLibrary` holder (RAII, no `Box::leak`) calling `cordis_plugin_apply` (`extern "C"`). Enable with `cargo build --features hmr` and a `.so` built with the same toolchain. Not invoked by `src/main.rs` or `ReflectService`, `watcher` is the production path.
- `Cargo.toml`: `[features] hmr = ["dep:libloading"]` (`libloading 0.8` optional, `notify 8.2.0` always for `watcher`), `default = []`.

---

## 11. what is explicitly not ported in spike (updated)

Per YAGNI (Phase 1, §8) + HMR deferral above:

- ❌ `libloading` HMR **DEFERRED**, file-watch fallback `crates/ares-cordis-core/src/watcher.rs` (`notify` → `ReflectService::notify` → `Fiber::reload` via epoch) covers 90% value without dynamic code. Dynamic code swap remains as `crates/ares-cordis-core/src/hmr.rs` stub behind `#[cfg(feature = "hmr")]` (off by default, `libloading 0.8` optional). See HMR decision above and `lib.rs` HMR section.
- ❌ WASM, deferred.
- ❌ Visual layer package (~60 TS packages, 12 layers), in Rust, one crate; do not replicate ceremony.
- ❌ `ares.toml` symlink handling, keep `AresConfigManager::start_watching()` as-is for Phase 2; Loader is additive. `watcher` generalizes it to Cordis entries/TOON without touching `ares.toml` symlink (`/opt/ares-config/ares.toml`).

---

## 12. critical anchors (Reread before phases 2/4/5)

- `/opt/ares/src/main.rs` `run_server` (lines 296,889, 17 steps) → becomes `root_ctx.plugin(...).plugin(...).await` (5,8 lines). Every registry/pool/cache must migrate to a `Service`.
- `/opt/ares/src/lib.rs` `AppState` (230,274, 17,22 fields) + `base_router()` → `Arc<Context>`, `build_router(ctx: Arc<Context>)`.
- `/opt/ares/crates/ares-tools/src/runtime_registry.rs` `start_background_reload` (60s poll, `ArcSwap`) → epoch-driven `notify`.
- `/opt/ares/crates/ares-llm/src/provider_registry.rs` `ArcSwap<HashMap>` + `NvidiaCatalogCache` → `LlmService` with circuit breaker.
- `/opt/ares/src/api/handlers/admin.rs` 190 KB, 5,946 lines, split by domain in Phase 6.

---

## 13. consequences & alternatives

- If `async fn in trait` causes `dyn` issues, use `async_trait` only for that trait and document why (Rust 1.91 floor, 1.75+ stable for `async fn in trait`, but `dyn Service` may need `async_trait`, prefer `impl Future` return).
- If `TypeId` + `HashMap` proves too coarse (downcasting ergonomics), evaluate `anymap`/`typemap` crates, but hand-rolled `HashMap<TypeId, Box<dyn Any>>` is sufficient for spike.
- If epoch String concatenation bloats logs, switch to `sha2` digest and keep `:uid1:uid2` only in `tracing::debug!`.
