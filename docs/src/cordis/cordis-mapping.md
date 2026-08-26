# Cordis → rust mapping (Phase 0, step 5)

**Source:** DeepSeek Cordis paper ("A Programming Paradigm for Spatiotemporal Composability", Aug 2026, `cordiverse/cordis` + `cordiverse/paper`) and DeepSeek Harness (`deepseek-ai/deepseek-harness`, TS, ~60 packages, 12 layers).
**Target:** ARES `/opt/ares` (dirmacs/ares v0.7.3, 11 crates + `ares-server` root, Rust 1.98, Tokio/Axum).
**This doc is strategy only**, no code changes. Spike crate `crates/cordis` (Phase 1) must prove the theorems before adoption.

Phase 2 crate graph (`crates.io` already has `cordis` 0.0.0, so the Cargo package is `ares-cordis` with `[lib] name = "cordis"`; workspace dep key stays `cordis`):

| Path | Package | Rust crate |
|------|---------|------------|
| `crates/cordis` | `ares-cordis` | `cordis` |
| `crates/ares-agent` | `ares-agent` | `ares_agent` |
| `crates/ares-store` | `ares-store` | `ares_store` |

Root package stays `ares-server` with lib name `ares`. Domain config lives in plugin crates; Overlay (`src/overlay.rs`) owns `ares.toml` after Phase 4.

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
- `Context::get_relaxed::<T: Service>(&self) -> Option<Arc<T>>` — same walk, but a locally-owned provider whose owner fiber rests mid-transition (`Active`, `Loading`, `Reloading`, `Unloading`, reactive `Pending`) still resolves. Strict `get` refuses those so consumers never observe mid-transition values; disposed owners (undos already ran) and terminal `Failed{error}` owners stay refused even relaxed.
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


### Kernel intercept meta-events (beyond the prototype chain)

Five kernel operations expose listener-driven veto points on reserved events (`internal/get`, `internal/set`, `internal/config`, `internal/update`, `internal/listener`), plus an `internal/dispatch` observer. These sit outside the product event catalog on purpose.

- `EventsService::intercept_get/set/config/update/listener` implement the semantics; `internal/dispatch` reports every NON-internal dispatch as `(mode, name, args)` and exempts itself from observation.
- `internal/get` consults a Bail chain on every strict `Context::get`: a non-null terminal replaces the returned value, `{"refuse": true}` fails the lookup, a redirect verdict continues at the parent frame, null passes through, and a chain error refuses the read.
- `internal/set` errors veto the provider write; the previous binding stays fully intact.
- `internal/config`'s non-null terminal IS the effective configuration for one apply pass (staged on the fiber and consumed by the registry runner); a chain error rests the fiber terminal `Failed`.
- `internal/update` bails skip the scheduled restart entirely — the fiber keeps serving its current application and the deferred config stays readable via `Fiber::vetoed_config`.
- `internal/listener` bails cancel the registration; the caller receives an inert handle and neither registry sees the listener.

Zero-cost gating: every helper short-circuits on `listener_count == 0` before doing anything else, so the default path is two map lookups. Synchronous call sites bridge through `block_in_place` on multi-thread tokio runtimes; single-thread flavors fall OPEN (warning + allow), matching historical no-listener behavior. A thread-local re-entrancy fence keeps operations made inside a chain un-intercepted. The `*_from` dispatch family — `bail_from`, `waterfall_from`, `waterfall_async_from` — exposes the same chains for product code, adding an optional per-dispatch `ListenerFilter`; filtered listeners skip one dispatch and remain registered.

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

File placement: `crates/cordis/src/fiber.rs` (spike) → later `crates/ares-context/src/fiber.rs`.

The fiber lifecycle adds `Loading` and `Failed`: `Loading` marks a fiber mid-instantiation and `Failed` records a terminal error from a plugin activation. A failed fiber remains observable via the registry so a loader or admin tool can report why a registration did not become `Active`; a later successful re-registration starts a fresh fiber that reaches `Active`. In 0.9.0, `Fiber::refresh` still compares epochs, then undoes prior effects and reruns the registered plugin `apply` when a reload is required.

### Pending rest state (reversible withdrawal)

A fifth state completes the machine: an `Active` runner whose dependency is genuinely withdrawn disposes its effects LIFO under `Unloading`, then rests `Pending` instead of dying or going `Inactive`. While `Pending`, the fiber keeps its registry key (it survives `prune_disposed`) and reactivates through `Loading` when the provider returns.

- `Pending` is reserved for reactive waiting only: apply errors still rest terminal `Failed{error}`, and a peer-version constraint refusal over a live provider rests `Inactive` (the provider is still available; the refusal is policy, not loss).
- Eligibility needs one fully-satisfied refresh pass first — registration alone cannot mark a fiber eligible because its declares may still be unserved.

### Readiness barriers vs availability predicates

`RegistryService::register_with_readiness(ctx, plugin, config, ready_when)` installs a `ReadinessBarrier` consulted before every activation pass. While the gate reports not-ready the fiber rests inspectable `Pending` — quiet waiting that never becomes `Failed` — with the factory run once up front and strict `get` refusing the non-Active owner, so the service stays out of consumer reach until the observed environment turns ready.

- `ReadinessBarrier::new(pred)` wraps one `Fn(&Arc<Context>) -> bool`; `.and(other)` AND-composes; `with_readiness([a, b, c])` folds any number of barriers (an empty list is vacuously ready).
- `.watching([TypeId])` unions the provider keys whose settlements re-kick the gated fiber through the `ReflectService` fan-out: an external provide or withdrawal re-evaluates the gate without anyone touching the fiber.

This complements rather than replaces availability predicates (`Service::check`): a rejected availability predicate rests `Failed{error: "availability predicate rejected service"}` — loud and terminal per the rules above — while a closed readiness gate is quiet, reversible waiting. Use the predicate when the factory cannot produce a valid service; use the barrier when production succeeds but serving should hold.

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

- The epoch type is `String`, not a hash. The paper uses concatenation for debuggability; if perf matters, switch to `sha2` hash later.
- `Fiber::refresh` compares `self.epoch.read()` vs `compute_epoch(&self.injects)`; logs diff via `tracing::debug!`.

---

## 7. notify, reactive recomputation (tokio::sync::watch)

Cordis `notify` is fan-out to dependent fibers. In the TS Harness, notify uses `EventEmitter`; in Rust, it uses `tokio::sync::watch`.

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

Replaces: `RuntimeToolRegistry::start_background_reload` (60s poll, `crates/ares-tools/src/runtime_registry.rs`), `ProviderRegistry` poll (`crates/ares-llm/src/provider_registry.rs`), `NvidiaCatalogCache::start_background_refresh` (`crates/ares-llm/src/nvidia_catalog.rs`).

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

### Dispatcher parity (shipped)

The `dispatch` implementation follows the five modes exactly:

- `Emit`: every handler is spawned and not awaited (fire-and-forget); `dispatch` returns JSON `null` after broadcasting the event and payload on the bus. A caller that needs completion can listen on the bus or use a oneshot channel, not await this call.
- `Parallel`: handlers run concurrently via `tokio::task::JoinSet`; successful dispatch returns JSON `null` (handler values are discarded). The first error observed is propagated (a joined panic surfaces as `CordisError::Fiber`).
- `Serial`: handlers run in registration order with the original payload; the first non-null result bails and is returned. An all-null chain returns the original payload. `Serial` and `Bail` share this path.
- `Bail`: stops at the first handler that returns a non-null result and returns that value without running later handlers; a null result means not bailing and the chain continues with the original payload.
- `Waterfall`: each handler transforms the payload and passes the result to the next; a handler short-circuits by returning an object whose `waterfall_stop` field is `true`. This is the Rust static-dispatch analogue of the TS `next()` closure: instead of passing a `next` function, a handler opts out by returning the sentinel.

### Dispatch participation knobs (EventOptions / emit_filtered)

Flat listeners can register through `on_with` / `once_with` with `EventOptions { prepend, global }`:

- `prepend: true` inserts the listener at the FRONT of the dispatch-order list, so it runs before previously registered listeners of the same event.
- `global: true` marks the listener realm-agnostic: `emit_filtered(event, args, filter)` offers every non-global listener to the filter predicate first and excludes it from that one dispatch on a `false` verdict — without unregistering it. Global listeners bypass the filter entirely.
- The historical `on` / `once` / `emit` signatures delegate with default options (`false`/`false`), so existing registrations are byte-compatible. The broadcast bus fan-out is not filtered; only registered handlers participate.

### Event-first skill execution (0.9.0)

Each skill receives the request `Context`. Tool steps run on a tenant-scoped context created with `ctx.isolate::<Tools>(tenant_id)` and invoke `Tools::execute` through `tools.execute`. Every Skill `LlmCall` step uses strict `Llm::complete` through `llm.complete`; `SkillEngine` and `SkillsService` do not call providers directly or fall back to `generate_with_history`. When `EventsService` is present, `waterfall_around` wraps these capability calls. `Tools`, `Llm`, and `Execute` public methods stay on the same event-first path.

---

## 9. loader & config reconciliation (Declarative)

Cordis Loader: `Entry { id, plugin, config, disabled, isolate, intercept }` + `EntryTree(Vec<Entry>)` persisted to `config/entries.json` (or `config/cordis-entries.toon` via `toon-format 0.4.1`). `Loader::reconcile(current, desired)` diffs incrementally.

Rust (Phase 3, `crates/cordis/src/loader.rs`):

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

### Loader journal (shipped)

`LoaderJournal` makes the `UpdateConfig` and `Retire` arms real. It stores a `JournalRecord` per entry: the plugin label owning the entry, the last applied config, the live fiber id when known, and a monotonically increasing generation counter (every mutation bumps it). It is the single source of truth for "is this entry live, with which fiber, at what config/version".

- `Loader::instantiate` and the `RebuildFiber` arm call `journal.upsert(id, plugin, config, Some(fid))` after a successful factory invocation.
- `UpdateConfig` reads the recorded fiber id, resolves it via `RegistryService::get_fiber`, and calls `Fiber::update` when a live fiber is known (running `block_in_place` on a multi-thread runtime, journal-only on a current-thread runtime or no runtime); it then calls `journal.update_config(id, new_config, recorded)`, bumping generation.
- `Retire` calls `journal.retire(id)`, clearing the record.

It is provided as a service with `ctx.provide(LoaderJournal::new())`, so `Context::get::<LoaderJournal>` returns the shared handle. When absent, `Loader::execute_action` and `Loader::instantiate` degrade to log-only.

---

## 10. Plugin & RegistryService

Cordis plugins are `FnOnce(&Context, Config) -> Result<Disposable>` or struct with `apply`. Registry enforces single-source discipline.

Rust (Phase 2, `crates/cordis/src/registry.rs`):

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

Static registration (preferred production): `inventory`/`linkme` (compile-time plugin set), real surface is `RegistryService::plugin` (single-source discipline); `inventory::submit!` / `linkme::distributed_slice` of `fn(&Arc<Context>) -> Result<FiberId, CordisError>` is the future shortcut once crate count stabilizes (see `crates/cordis/src/lib.rs` HMR section and `Wiring` task). Dynamic HMR (dev only, behind `#[cfg(feature = "hmr")]` **off by default**): `libloading` path that `dlopen`s `.so` and calls `Plugin::apply` via `extern "C"`; if `libloading` ABI fragility blocks (Rust `1.98` toolchain coupling, `unsafe` soundness), fall back to file-watch + full fiber reload (re-read config/TOON), 90% of value per plan, see `watcher` fallback below.

### HMR YAGNI decision (plan Assumptions §Contingencies)

**Decision: DEFER `libloading` HMR, keep file-watch + `Fiber::reload` as production path.**

- Rationale: `libloading::Library::new` + `Symbol<extern "C">` requires `unsafe`, a stable `repr(C)` ABI boundary, and the `.so` to be built with the exact same Rust toolchain (`1.98`). ABI drift across patches, `rust-doctor` soundness flags, and `Box::leak` ownership hazards make `libloading` too brittle for a generic runtime. As plan contingency states: "If `libloading` HMR proves too complex for Rust (dynamic library ABI fragility, `unsafe` surface), fall back to file-watch + full fiber reload without dynamic code swapping … `file watcher still triggers Fiber::reload()` by re-reading config/TOON, which already covers 90% of self-evolution value. Dynamic code HMR can be deferred to a later phase behind `#[cfg(feature = "hmr")]` without blocking the core redesign."
- Fallback implemented: `crates/cordis/src/watcher.rs` (`watch_many` / `watch_cordis_entries`) uses `notify::RecommendedWatcher` (debounced `500 ms` + `100 ms` settle, same as `AresConfigManager::start_watching`) to watch `config/agents/*.toon` (recursive) and `config/entries.json` (or `config/cordis-entries.toon` parent dir). On `Modify`/`Create` it calls `ReflectService::notify(tid)` which BFS-walks `dependents` and spawns `Fiber::refresh` (epoch recompute via `compute_epoch`). No restart, no `libloading`. Logs `Configuration hot-reloaded successfully via Cordis watch` (generalizes `AresConfigManager`'s `Configuration hot-reloaded successfully` which is already proven on random-port E2E `39476`/`39120`, see `docs/cordis-redesign.md` §9/9b).
- Stub preserved: `crates/cordis/src/hmr.rs` is `#[cfg(feature = "hmr")]` (Cargo feature `hmr = ["dep:libloading"]`, off by default). It shows `libloading::Library::new` + `get::<HmrEntryFn>` + owned `HmrLibrary` holder (RAII, no `Box::leak`) calling `cordis_plugin_apply` (`extern "C"`). Enable with `cargo build --features hmr` and a `.so` built with the same toolchain. Not invoked by `src/main.rs` or `ReflectService`, `watcher` is the production path.
- `Cargo.toml`: `[features] hmr = ["dep:libloading"]` (`libloading 0.8` optional, `notify 8.2.0` always for `watcher`), `default = []`.

---

## 11. what is explicitly not ported in spike (updated)

Per YAGNI (Phase 1, §8) + HMR deferral above:

- ❌ `libloading` HMR **DEFERRED**, file-watch fallback `crates/cordis/src/watcher.rs` (`notify` → `ReflectService::notify` → `Fiber::reload` via epoch) covers 90% value without dynamic code. Dynamic code swap remains as `crates/cordis/src/hmr.rs` stub behind `#[cfg(feature = "hmr")]` (off by default, `libloading 0.8` optional). See HMR decision above and `lib.rs` HMR section.
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

- If `async fn in trait` causes `dyn` issues, use `async_trait` only for that trait and document why (Rust 1.98 floor, 1.75+ stable for `async fn in trait`, but a `dyn Service` can require `async_trait`; prefer an `impl Future` return).
- If `TypeId` + `HashMap` proves too coarse (downcasting ergonomics), evaluate `anymap`/`typemap` crates, but hand-rolled `HashMap<TypeId, Box<dyn Any>>` is sufficient for spike.
- If epoch String concatenation bloats logs, switch to `sha2` digest and keep `:uid1:uid2` only in `tracing::debug!`.

---

## 14. Current architecture (0.9.0)

This section describes the shipped 0.9.0 runtime. Earlier sections remain the Phase 0 mapping and 0.8 spike record.

### Fiber refresh

`Fiber::refresh` recomputes the dependency epoch. When the epoch changes, or the fiber is not already Active with dependencies satisfied, it undoes prior effects and reruns the registered plugin `apply`. Dispose still LIFO-undoes and passes through Unloading.

Wave-1 refinements: a working runner whose dependency genuinely vanishes rests reversible `Pending` (see §5) instead of dying, and reactivates when the provider returns; `Context::get_relaxed` lets lifecycle code read locally-owned values during those transitions while strict `get` stays conservative; `Fiber::subscribe_state` exposes every transition to panic-contained synchronous observers.

### EventsService dispatch

Shipped behavior in `crates/cordis/src/events.rs`:

- `Emit`: fire-and-forget; `dispatch` returns JSON `null`.
- `Parallel`: handlers run on a `JoinSet`; successful dispatch returns JSON `null`. Handler return values are not collected. The first join or handler error is propagated.
- `Serial`: handlers run in registration order with the original payload. The first non-null result bails and is returned. An all-null chain returns the original payload. `Serial` and `Bail` share this path.
- `Waterfall`: around-middleware with `next`; skipping `next` skips later handlers and core.

### Event-first product path

`Tools`, `Llm`, `Execute`, and skills stay event-first. Public methods go through `EventsService::waterfall_around` when the bus is on ctx (`tools.execute`, `llm.complete`, `agent.run`). Skill tool and LLM steps use those same events on a tenant isolate. They do not call providers or the tool registry directly.

### agent.admit

`agent.admit` (`Dispatch::Bail`) is the shared quota gate. `Execute::run`, JWT chat, API-key middleware, and MCP all dispatch it before work. The default handler denies monthly or daily quota for non-enterprise tenants. HTTP maps deny to 429; MCP maps deny to a tool error.

### Store, Overlay, TOON

The Store loader factory connects, runs `sqlx` migrations, and seeds default agent templates, then provides `Store`. Overlay fills empty loader `entry.config` from `ares.toml` sections and leaves non-empty configs unchanged. TOON file changes notify `TypeId::of::<Tools>()` and `TypeId::of::<Execute>()` so those fibers refresh.

### Isolate vs intercept

For the same `TypeId`, an isolate label wins: `get` skips intercept and walks the isolated store/parent. Unlabeled types still intercept, so request `TenantContext` and `ModelOverride` keep working on a Tools/Execute realm.

### TenantRealms

Request paths open the tenant realm then intercept `TenantContext` (`open` then `with_intercept`), including JWT middleware (tenant claims), JWT chat/research, and v1 chat/stream/agents. JWT `user:` isolate does not invent a dummy Free tenant. Background jobs open or isolate only and do not attach a request intercept. Admin tenant delete calls `TenantRealms::dispose` before SQL delete.

### Facade

The default `ares` crate has no axum. `Context`, `Execute`, `Tools`, `Llm`, and `register_plugins` are enough to run an agent. Enable feature `http` to pull `ares-http`. `ProviderRegistry` is not re-exported from `ares`; construction tests import it from `ares-llm`.

### Honest residuals

- `ProviderRegistry` still exists on `ares-llm` because `Llm::new` / `AgentRegistry::from_config` still take one during construction.
- `run_server` still instantiates Overlay first, fills empty loader configs, then instantiates remaining `config/cordis-entries.toml` entries.
- Scheduler, pipeline, and trigger domain loops remain native ARES engines. They inject `Execute` and run behind it; they are not a second public agent API.
- Root `ares-server` is a binary. Overlay lives in `crates/ares-http/src/overlay.rs`; the server still registers the Overlay factory.
- Overlay / optional `ServerRuntime` provide host extras (ActiveRuns, SkillEngine, MCP). `Execute` is registered once, by `ares-agent`.

---

## 15. Round 4 (0.9.x): policies, HMR finish, static factories, metrics, atomic saves

### Typed event payloads

All 22 catalog events in `crates/cordis/src/events_catalog.rs` (`CONTRACTS`, `contract_for`) carry typed payload structs in `crates/cordis/src/events_payload.rs`. Each implements the `TypedEvent` trait (`type Payload: Serialize + DeserializeOwned + Clone + Send + Sync`, plus `NAME`/`MODE`/`AROUND` const-linked to the catalog). `EventsService::dispatch_typed<E>` serializes and dispatches with the declared mode; `on_typed` / `on_typed_waterfall` deliver deserialized payloads to listeners, skipping malformed ones with a warning. A consistency test asserts every binding matches `CONTRACTS` and that no event is unbound. The raw `Value` API remains for kernel and dynamic cases.

### Engine choreography

Scheduler admission is scriptable through two waterfall events: `scheduler.before_run` output overrides `agent_name`/`message` on the executed request (audit identity stays schedule-tied); a Bail denial emits `{ok:false, denied:true}` and returns without running — due-pass callers solely advance `next_run`. Boundary events `SCHEDULER_TICK`, `SCHEDULER_SCHEDULE_DISPATCHED`, `PIPELINE_STEP_STARTED/FINISHED`, `PIPELINE_FANOUT_COMPLETED`, and `TRIGGER_FIRED` are emitted from scheduler/pipeline/trigger engines, completing full-catalog adoption.

### RhaiPolicy scripting (default-on)

`RhaiServiceConfig.listen: Vec<RhaiListenerConfig { event, fn_name }>` lets a declarative entry attach sandboxed Rhai functions to any catalog event:

```toml
[[entry]]
id = "policy-example"
plugin = "RhaiPolicy"
[entry.config]
script = "fn gate(p) { if p.tenant_id == \"banned\" { #{deny: \"banned\"} } else { () } }"
[[entry.config.listen]]
event = "scheduler.admit"
fn_name = "gate"
```

Semantics: the payload arrives as an object map (plain property access). Returning `()`/null passes through (Bail) or delegates via `next` (waterfall); any other value becomes the dispatch result — deny marker or short-circuit. Script runtime errors log a warning and pass through/delegate. Init validates each event against the catalog (unknown ⇒ Configuration error, recorded per-entry at boot). All listener disposables combine with the init guard so fiber dispose unregisters them. Enabled by default through root feature `rhai-policy`; factory key `"RhaiPolicy"` registered under the `rhai` feature.

### Static registration (inventory-collected factories)

`cordis::CordisPluginFactory { name, make }` (make is a plain `fn(&Arc<Context>, &Value) -> Result<FiberId, CordisError>`) is inventory-collected; `cordis::register_inventory_factories(reg)` installs every submission and is the server's primary boot path (`src/main.rs`). The hand-written `register_plugins` chains remain compiled as the `--no-default-features` fallback and for tests. Each capability crate declares an optional `inventory` feature forwarded from root/facade; because inventory nodes live in linker sections, a crate with no otherwise-referenced code loses its submissions — tests force linkage by calling the manual chains before collecting. Parity proofs: `crates/ares/tests/inventory_parity.rs` (library crates) and `tests/server_inventory_probe.…`

### Admin surface

- `POST /admin/cordis/services/{name}/retire` / `provide` — runtime retire/re-provide.
- `GET|PUT /admin/cordis/entries`, `DELETE /admin/cordis/entries/{id}`, `POST /admin/cordis/entries/{id}/toggle` — declarative entries management (Null configs normalize to `{}`), 503 when loader state is absent.

- `PATCH /admin/cordis/entries/{id}` — typed partial update via `cordis::loader::EntryUpdate` (`config`, `disabled`, `isolate`, `intercept`; `id`/`plugin` deliberately not patchable). Only present fields change, `{}` is a validated no-op that still persists and re-applies; replies with the post-patch entry and per-action outcomes, 404 on unknown ids. A failed config pre-flight attaches a machine-readable `issues` array (`[{message, path}, …]`) beside the legacy `error` string.
- `POST /admin/cordis/entries/reload` — reload from disk through the shared apply flow.
- `GET /admin/cordis/events` — per-event dispatch counters `{total_dispatched, by_event}` from `EventsService::dispatch_snapshot()` (counts every mode via the single `dispatch` choke point).

### Atomic entries persistence

`EntryTree::save_to_toml_file` writes `<name>.tmp-<pid>` then renames over the target (atomic on POSIX), preserving leading comment headers verbatim; the temp file is removed on failure so crashes mid-write leave either the previous or the new file, never a truncated one.

### HMR resolution

The §10/§11 "defer" decision above is superseded. The dylib path is finished and correct, still opt-in: `apply_plugin_so` copies the library to a process-unique sibling (`<stem>.<pid>.<seq>.hmr-load`) before dlopen — glibc caches handles by path, so a rebuilt `.so` never swaps without this copy — and `HmrLibrary::drop` removes the copy after dlclose (best effort). Watcher hook `apply_plugin_so_if_dylib` benefits automatically. Production reload stays watcher + TOML reconcile; enable dylib loading with `--features hmr` only with same-toolchain cdylibs.

---

## 16. Round 5–6 (0.9.x): policy activation, effect removal, lifecycle hardening, composition

### Round 5 — RhaiPolicy production activation

`RhaiListenerConfig` gained `on_error: passthrough | deny` (serde default = passthrough, byte-compatible with round-4 configs). `deny` is fail-closed: a flat-listener script error returns the built-in deny-marker shape instead of the payload; a waterfall-listener error vetoes without calling `next`. Multi-instance policies must use isolate realms (a pass-through handler on a shared Bail chain terminates every dispatch); each realm keeps its own `EventsService`. `config/cordis-entries.toml` ships an ACTIVE `policy-admission-audit` entry logging `scheduler.admit` agent names. The paper's witnessed-effect primitive (`Effect`, `EffectGuard`, `Context::effect`) was DELETED in round 5 — zero users ever; revertibility lives entirely in `Disposable` + Fiber LIFO a…

### Round 6 — Guarded withdrawal (§4.3.1 reliedₙ)

`RegistryService::reliance_count(&(TypeId, label))` derives (never seeds) how many ACTIVE fibers other than the provider resolve a key at its isolate label and declare an inject on it — always current under late `declare_inject`, no stale rows. A per-fiber realms ledger (`FiberId -> Weak<Context>`) captured at registration supplies context for label resolution. `Context::remove<T>` refuses while consumers remain ("guarded withdrawal: N active consumer(s)…"); internal rollback uses `remove_forced` (undo never blocks). Admin retire maps refusal to 409 `{retired:false, reason:"guarded", consumers:N}`.

### Round 6 — Verified hot-swap

`LoaderAction::RebuildFiber` applies the new plugin OUT-OF-BAND on a scratch child context first: failure ⇒ old provider untouched. Success bridges via intercept (new values win instantly), disposes old while the bridge covers stale store rows, promotes peeked values store-first, then drops the bridge — `get::<T>` resolves at every instant (test probes from a concurrent task). Falls back to classic dispose+instantiate (reported `verified=false`) for untracked old fibers, isolated entries, or side-effectful factories (Store migrations run twice across trial+promotion). AppliedAction carries `verified` through admin PUT responses.

### Round 6 — Typed listeners adopted

Scheduler's three prod listeners (`agent.completed` observability, `agent.failed` runtime-control, legacy shim) use `on_typed::<AgentCompletedEvent/AgentFailedEvent>`. rhai_service intentionally stays on the raw Value API (dynamic script maps are its purpose).

### Round 6 — Dependency-cycle detection

`cordis::cycles::{find_dependency_cycle, DependencyGraph}` — colored-DFS over the fiber inject graph with deterministic node ordering; `CycleLedger` maps (TypeId,label)→provider-fiber and entry-id↔fiber so loader-side callers can reconstruct edges without registry internals. Unit-tested for self-loops, 2/3-cycles, nested-behind-prefix, disconnected components, cross-edge false positives.


### Round 6 — Intercept meta-events, readiness barriers, cascade batching, logger, timers

**Intercept meta-events + `*_from` dispatch.** Five veto points (`internal/get`, `internal/set`, `internal/config`, `internal/update`, `internal/listener`) plus the `internal/dispatch` observer (see §4) ride `EventsService::bail_from`; product code gets the same chains through `bail_from` / `waterfall_from` / `waterfall_async_from` with an optional per-dispatch `ListenerFilter`. Veto semantics: a config-rewrite terminal becomes the effective config for that apply pass; an erroring config chain rests the fiber `Failed`; an update veto skips the restart and parks the deferred config in `Fiber::vetoed_config`; a listener bail cancels registration and returns an inert handle. Every gate is zero-cost when unregistered, and the synchronous bridges fall open on single-thread tokio flavors.

**ReadinessBarrier.** `ReadinessBarrier::new(pred)` + `.and(..)` / `with_readiness([...])` + `.watching([TypeId])`, installed via `RegistryService::register_with_readiness` (see §5): closed gates hold fibers at inspectable `Pending` and re-kick through the ReflectService fan-out; availability predicates stay the loud `Failed` path.

**Cascade batching.** An in-flight ledger around loader re-applies collapses concurrent provider config updates to ONE dependent convergence wave per settled batch (`CASCADE_INFLIGHT`, consulted from the kernel refresh path).

**LoggerService (`cordis::logger`).** Bounded ring (default 1000) of `Message`s; effect-owned `Exporter` sinks (registration returns the removing `Disposable`) with per-sink `ExporterConfig` (per-name levels, `max_length` truncation); per-name thresholds via `set_level` with `set_default_level` fallback; zero-cost `enabled` gate before argument assembly; printf rendering `%s %d %i %f %o %O %c %C %%` with ANSI16 name-hashed colors (`%c`) and bold (`%C`); `hyphenate`/`derived_name` kebab-case logger names; `LoggerIntercept` per-fiber threshold overrides resolved through the relaxed read channel; `Context::log/info/warn/debug/error` facade no-ops when no logger is provided.

**Timers (`cordis::timer`).** Six primitives — `timeout`, `sleep`, `interval`, `interval_stream`, `debounce`, `throttle` — std-only, fiber-scoped via `with_current_fiber` labeled undos, running on one shared wheel thread; dispose yields exactly one final `Err(InactiveEffect)` on streams then closes them; dropping handles never cancels.

---

## 17. Round 7 (0.9.x): wiring composition and detection, drain-and-shift

### Cycle detection wired

`Loader::apply` now reconstructs the post-apply inject graph via the CycleLedger (fresh-provide diff at `instantiate_entry` records providers; `fiber.injected_type_ids()` + `ctx.isolate_label()` resolve edges) and runs `find_dependency_cycle`. Detection never fails the apply — a found ring logs a warning naming entry ids; `Loader::detect_cycle_entry_ids(&ctx)` queries it, and `GET /admin/cordis/entries` carries an additive `dependency_cycles` key (entry-id rings, empty when healthy).

### Composition wired at boot + reload

Both entry-load sites (boot_loader_program parse, watcher/poll reload via `reload_current(ctx, path, &mut current, desired_composed, &journal)` — callers own parsing+composition now) run `cordis::compose_all` (@include splice → @group flatten → `${rhai: …}` interpolation) with the entries file's parent as base dir. Fail-open: composition errors log loudly naming path+error and proceed with the RAW entries. Interpolation scope variable is `entry` (rhai reserves `$`). Commented examples live in `config/cordis-entries.toml`.

### Drain-and-shift provider replacement

`Loader::replace_provider(ctx, plugin_name, config, journal)`: trial new provider out-of-band → intercept bridge → dispose old → promote store-first → fresh Active fiber journaled. Zero absence window (concurrent-get probe test). Shared tail extracted into SwapPromotion used by both verified rebuild and replace; fixing it closed a latent double-swap bug (promoted values had no undo, blocking any second swap). replace_provider bypasses Context::remove's guard legitimately — the bridge guarantees continuous resolution, which is exactly what the guard protects; genuine retire keeps the guarded path. Root-realm only; no dispose-then-rebuild fallback.

---

## 18. Round 8 (0.9.x): composed program, operational replace, metatheory smoke

### Entries program split (composition load-bearing)

The production `config/cordis-entries.toml` now exercises the round-6 compose pipeline for real: events/overlay/store inline at the head, tools..probe spliced verbatim from `config/cordis-entries-shared.toml` via `@include`, http inline. Parity-proven: simulated composition of the new layout deep-equals the old flat 15-entry program, boot order unchanged. A second ACTIVE policy joined the program: `policy-emergency-gate` (isolate `policy-emergency`) denies any scheduled agent named `EMERGENCY-HALT` with `on_error="deny"` — a fail-closed kill-switch; `policy-admission-audit` remains last.

### Operational provider replacement

`POST /admin/cordis/services/{name}/replace` (body `{"config": <Value>}`) exposes round-7's `Loader::replace_provider`: trial out-of-band → bridge → dispose old → promote → fresh fiber journaled. 200 `{replaced:true, plugin, fiber_id}` on success; 409 `{replaced:false, reason}` when replacement is refused (unknown label, untracked/isolated/failing trial — old provider untouched by design); 400 malformed body; 503 without journal. Providers can now be swapped under live traffic from outside the process.

### Metatheory property smoke tests

`cordis::metatheory` encodes paper guarantees as executable checks over the public API:

- **quiescence_after_every_op** (Thm 66 progress): 24-op deterministic schedule; no fiber rests in transitional states; Active fibers always have injects available. HELD — inertia keeps transitions unobservable at rest.
- **order_confluence_of_registrations** (Cor 21 / Thm 73): consumer-first vs providers-first converge to identical epochs and projections. HELD.
- **dependent_never_active_without_provider** (§spatial reactive invariant): HELD with two documented deltas — (1) a `declare_inject` can land on an Active fiber outside the state machine until refresh; (2) factories whose `apply()` errors become Failed *without* ReflectService wiring, where the paper expects permanently-Inactive dependents. Both are future-round deliverables.
- **lifo_dispose_restores_store** (Thm 16): exact LIFO undo order, disposables fire exactly once, store restored.

---

## 19. Round 9 (0.9.x): eager reconciliation, failed-factory wiring, peer versioning

### Metatheory deltas resolved (round-8 follow-ups)

Both documented deltas are now resolved behavior:

1. **Eager declaration**: `declare_inject` on a fiber resting Active reconciles immediately — inertia try-lock fast path updates epoch+state in place (satisfied) or drives the refresh transition to Inactive{missing-dependency} (unsatisfied). A `pending_declare` flag folded into refresh's recompute loop makes declare-vs-refresh races lossless.
2. **Failed-factory wiring**: both `RegistryService::register` failure paths wire the fiber into ReflectService (`register_fiber` + notify on the type key), so dependents observe provider-loss and rest Inactive at quiescence. `Failed{error}` remains the terminal visible state (deliberate divergence from the paper's permanently-Inactive expectation — operationally more useful); the provided slot stays vacant and re-registration supersedes with a fresh fiber id.

Bonus fix: a latent deadlock in `register` itself — an if-let scrutinee held the provided-map read guard across stale-slot cleanup that took the write lock. Exposed by metatheory leg F; fixed by deciding staleness under the read guard before any write.

### Peer-dependency versioning

Addresses the paper's open problem (§discussion):

- **Provide side**: `Context::provide_versioned<T>(value, version)`; legacy `provide()` = version 0. Versions live in a parallel map with LIFO-undo restore (disposal restores the prior version exactly).
- **Inject side**: `Fiber::declare_inject_versioned<T>(min_compatible: Option<u64>)`. Satisfaction rule: provider exists AND major(provider) == major(requirement) AND provider >= requirement, where major(v) = v / 100_000. Mismatch ⇒ dependent stays Inactive — never binds incompatible versions.
- **Reactivity**: constrained epoch fragments fold in "v<major>@<floor>:<version>", so same-major upgrades flip the epoch and reactively reactivate dependents (test: v200_001 → Inactive → compatible v100_005 re-provide → Active with new projection). Unconstrained fragments byte-identical to the prior scheme.
- **Deliberate open point kept**: structural interface compatibility (paper's full problem) is NOT attempted — majors-only buckets with explicit floors cover the practical case; deeper structural checks remain future work.

### Status vs paper guarantees

Quiescence (Thm 66): held. Order confluence (Cor 21/Thm 73): held. LIFO disposal (Thm 16): held. Reactive invariant (§spatial): now fully held — eager declarations close the last observable gap. Terminal-state divergence (Failed vs Inactive) is documented and deliberate.

## 20. Round 10 (0.9.x): accessors, layered chains, lifecycle riders, module graph, entry moves

### Accessor registry (name-keyed computed properties)

`Context::register_accessor(name, Accessor::{read_only, read_write, setter_only})` installs a computed property beside the TypeId service store and returns an `EffectHandle` whose disposal removes the declaration AND every alias. `Context::alias(alias, target)` binds an alternate name through the same registration slot; duplicates (including alias collisions) are rejected with `DuplicateProvider`. Typed reads surface `PropertyTypeMismatch` instead of a silent `None`; writes against a read-only property are refused with `ReadOnlyProperty`. Crucially, accessor traffic BYPASSES the `internal/get` / `internal/set` intercept waterfalls entirely — resolving an accessor never consults or re-enters a veto chain (accessors are policy plumbing, not provider state). Anchors: `crates/cordis/src/context.rs`.

### Layered intercept chains

Intercept layers per TypeId are an ordered outermost..innermost sequence; NEW registrations APPEND rather than replace, so the innermost layer stays effective for all existing getters — no caller breakage when another policy layer joins. `Context::intercept_chain(tid)` returns every layer in dispatch order; `Context::chains_structurally_equal(a, b)` compares two chains by shared-instance identity (`Arc::ptr_eq` per layer pair) — erased values carry no comparable contract, so freshly-built values compare unequal by design, which is the honest test for restart decisions.

### Lifecycle riders

`Fiber::update` now returns `Result<(), CordisError>`: a restart-path error propagates to the caller while the fiber stays `Active` serving its OLD configuration (effects never unwind on a failed restart). An `internal/update` veto parks the deferred config in `Fiber::vetoed_config()` and returns `Ok` — the skip is observable without being destructive. The `internal/config` waterfall consult now also covers the ACTIVATION path in the registry, so config rewrites apply on first activation identically to re-applies.

### Module graph fan-out (opt-in)

`cordis::module_graph::ModuleGraph` maps module keys to their dependencies. With a `ModuleReload` implementation installed (`with_reloader` / `set_reloader`), `change_many(ctx, keys)` runs in two phases: phase 1 computes the TRANSITIVE affected plugin set READ-ONLY across the dependency closure; phase 2 reloads each affected plugin EXACTLY ONCE per transaction through `ModuleReload`, classified into a `ChangeOutcome`. A failing reload rolls back that plugin only — successfully reloaded siblings stay `Active`. The file watcher's debounced batch fans through a registered `ModuleGraph` when one is provided on the context; WITHOUT one, watcher behavior is byte-identical to before. Anchors: `crates/cordis/src/module_graph.rs`, wiring in `crates/cordis/src/watcher.rs`.

### Entry moves (move_entry surface)

Entries gain hierarchy: `EntryPosition { parent, position }` rides on `Entry`. PATCH `/admin/cordis/entries/{id}` accepts optional `parent` / `position` applied move-THEN-update — an invalid placement (e.g. moving an entry into its own descendant) answers 409 before ANY mutation of file or live tree. `POST /admin/cordis/entries/{id}/move` relocates an entry together with its whole `{id}:*` subtree in one rename cascade. A valid move preserves fiber identity: the live instance refreshes IN PLACE under the new key, so consumers never observe a dispose/recreate window. Disabled groups move suppressed-then-restored. Anchors: `EntryPosition`, `EntryTree::move_entry`, `subtree_ids`, `Loader::move_entry` in `crates/cordis/src/loader.rs`; handlers in `crates/ares-http/src/api/handlers/admin/cordis.rs`.
