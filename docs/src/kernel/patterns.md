# Kernel Patterns in Rust

This chapter is a cookbook for the Cordis kernel application programming
interface (API). Every snippet is a light adaptation of a real in-tree
test. The citation names the test and file, so you can read the full
context there.

## Provide and Consume Typed Services

Provide a service on a context; read it back typed with `get`. Child
contexts inherit through the parent walk. `intercept` creates a child
where one type resolves to an override; the parent stays untouched.

```rust
use cordis::{Context, Service};

struct Greeting(String);
impl Service for Greeting {}

let root = Context::new_root();
root.provide(Greeting("hello".into()));
assert_eq!(root.get::<Greeting>().unwrap().0, "hello");

// Per-request override: innermost intercept wins.
let req = root.intercept(Greeting("override".into()));
assert_eq!(req.get::<Greeting>().unwrap().0, "override");
assert_eq!(root.get::<Greeting>().unwrap().0, "hello");
```

Source: `isolate_and_intercept` in `crates/cordis/src/lib.rs`.

## Listeners: `on_with`, `once_with`, and `emit_filtered`

`on` and `once` delegate to `on_with` / `once_with` with default
options. The explicit forms take `EventOptions`: `prepend: true`
inserts at the front of the dispatch order, `global: true` exempts the
listener from context filters.

```rust
use cordis::events::{EventOptions, EventsService};
use std::sync::Arc;

let svc = EventsService::new();
let order = Arc::new(parking_lot::Mutex::<Vec<String>>::new(Vec::new()));

for name in ["first", "second"] {
    let slot = order.clone();
    svc.on("prepend.test".into(), move |_p| {
        let slot = slot.clone();
        async move {
            slot.lock().push(name.to_string());
            Ok(serde_json::Value::Null)
        }
    });
}

// Prepend lands in front of both default registrations.
let prepended = order.clone();
svc.on_with(
    "prepend.test".into(),
    EventOptions { prepend: true, global: false },
    move |_p| {
        let slot = prepended.clone();
        async move {
            slot.lock().push("prepended".to_string());
            Ok(serde_json::Value::Null)
        }
    },
);
```

Sources: `prepend_ordering_observed` and `once_with` usage in
`crates/cordis/src/events.rs`.

`emit_filtered` dispatches to listeners whose registration options pass
the filter. Exclusion is per dispatch: nobody is unregistered, and a
later unfiltered emit runs everyone again. Global listeners bypass the
filter entirely.

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

let ran_a = Arc::new(AtomicUsize::new(0));
let a = ran_a.clone();
svc.on_with("filtered.test".into(), EventOptions::default(), move |p| {
    let a = a.clone();
    async move {
        a.fetch_add(1, Ordering::SeqCst);
        Ok(p)
    }
});

// A rejecting filter excludes non-global listeners for THIS dispatch.
svc.emit_filtered(
    "filtered.test".into(),
    serde_json::json!({ "tenant": "a" }),
    Box::new(|_opts| false),
)
.unwrap();

// An unfiltered dispatch runs both listeners again.
svc.dispatch("filtered.test".into(), serde_json::json!({}), cordis::Dispatch::Emit)
    .await
    .unwrap();
```

Source: `filter_excludes_nonmatching_contexts` in
`crates/cordis/src/events.rs`.

## Interceptor on `internal/set`

The kernel exposes six meta-events as veto points.
`internal/set` runs before every service write. No listener means zero
cost. Null or pass-through allows the write. A chain error vetoes the
write and the previous value stays.

```rust
use cordis::events::{EventsService, INTERNAL_SET_EVENT};
use cordis::CordisError;

let svc = EventsService::new();
assert!(svc.intercept_set("Svc", None).await.is_ok());

// Freeze writes while this gate lives.
let d = svc.on(INTERNAL_SET_EVENT.into(), |_payload| async move {
    Err::<serde_json::Value, CordisError>(CordisError::Configuration(
        "writes are frozen".into(),
    ))
});
assert!(svc.intercept_set("Svc", None).await.is_err());

d.dispose();
assert!(svc.intercept_set("Svc", None).await.is_ok());
```

Source: `set_interceptor_vetoes_write_leaves_old_value` in
`crates/cordis/src/events.rs`.

## Readiness Gates: `register_with_readiness` and `.watching`

A readiness barrier holds a fiber out of service while it reports
`false`. The fiber rests in an inspectable `Pending` state; this is
quiet waiting, not failure. `.watching(..)` declares which service types
re-kick the gate when their providers settle, so external provides and
withdrawals re-evaluate it without polling.

```rust
use cordis::{Context, RegistryService, Service};
use cordis::registry::ReadinessBarrier;
use std::any::TypeId;

struct Dependency;
impl Service for Dependency {}

struct ConsumerPlugin;
impl cordis::Plugin for ConsumerPlugin {
    type Config = ();
    type Provides = Dependency;
    fn apply(
        &self,
        _ctx: &std::sync::Arc<Context>,
        _cfg: (),
    ) -> Result<std::sync::Arc<Dependency>, CordisError> {
        Ok(std::sync::Arc::new(Dependency))
    }
}

let ctx = Context::new_root();
let registry = RegistryService::new();

// Gate observes a plain context fact: is Dependency provided?
let fid = registry
    .register_with_readiness(
        &ctx,
        ConsumerPlugin,
        (),
        ReadinessBarrier::new(|ctx: &std::sync::Arc<Context>| {
            ctx.get::<Dependency>().is_some()
        })
        .watching([TypeId::of::<Dependency>()]),
    )
    .expect("fact-gated registration");
```

The factory still runs once at registration, so configuration errors
surface immediately. Strict `ctx.get` refuses values owned by
non-`Active` fibers, so consumers never see a gated service early.

Source: the fact-gated leg of the readiness tests in
`crates/cordis/src/registry.rs` (barrier over
`ctx.get::<Dependency>()` plus `.watching([TypeId::of::<Dependency>()])`).

## Accessors and Aliases

An accessor is a name-keyed computed property. Reads and writes bypass
the `internal/get` / `internal/set` waterfalls by design. Disposing the
handle removes the declaration AND every alias bound to it.

```rust
use cordis::{Accessor, Context, CordisError};
use parking_lot::Mutex;
use std::any::Any;
use std::sync::Arc;

#[derive(Debug, PartialEq)]
struct PropValue(pub u64);

let ctx = Context::new_root();
let cell = Arc::new(Mutex::new(5u64));
let read_cell = cell.clone();
let write_cell = cell.clone();

let handle = ctx
    .register_accessor(
        "primary",
        Accessor::read_write(
            move |_ctx| {
                Ok(Some(Arc::new(PropValue(*read_cell.lock()))
                    as Arc<dyn Any + Send + Sync>))
            },
            move |_ctx, value: Arc<dyn Any + Send + Sync>| {
                *write_cell.lock() =
                    value.downcast::<PropValue>().unwrap().0;
                Ok(())
            },
        ),
    )
    .unwrap();

// Bind an alternate name resolving through the SAME registration.
ctx.alias("nick", "primary").expect("alias binds");
ctx.write_property("nick", Arc::new(PropValue(6))).unwrap();
assert_eq!(*cell.lock(), 6);

// Disposal removes both names at once.
assert!(handle.dispose());
assert!(ctx.read_property("nick").unwrap().is_none());
```

Collisions and unknown alias targets return errors:
`DuplicateProvider` and `ServiceNotFound`. A typed read that fails to
downcast returns `PropertyTypeMismatch`, never a silent `None`.

Source: `alias_resolves_same_value` and `accessor_read_write_roundtrip`
in `crates/cordis/src/context.rs`.

## Programmatic Tree Moves: `Loader::move_entry`

`Loader::move_entry` relocates a subtree in the live entry tree and
makes the running kernel agree with it. Validation happens first; a
refusal leaves the tree untouched. Renamed descendants follow the
`{parent}:` id convention (`svc` under `grp` becomes `grp:svc`).

For a pure structural move — same plugins, configs, disabled flags, and
isolates on both sides — the contexts-equivalence gate takes the noop
path. Every journaled record re-keys old to new while KEEPING its fiber
id. Consumers keep resolving the same live instances; nothing disposes
or re-creates.

```rust
use cordis::{Context, EntryTree, Loader};

// current: tree loaded from config; journal: the LoaderJournal service.
// Both come from the normal loader bootstrap.
let outcome = Loader::move_entry(&ctx, &mut current, &journal, "svc", Some("grp"), 0)
    .await
    .expect("move succeeds");

assert!(outcome.noop, "pure structural move takes the noop path");
assert_eq!(
    outcome.renamed,
    vec![("svc".to_string(), "grp:svc".to_string())]
);
```

Refusals include unknown ids, moving an entry under itself, and moves
under its own descendant. When mixed edits rode along so composition
differs, the call falls back to a full reconcile apply instead of the
noop path.

Source: the structural-move test in `crates/cordis/src/loader.rs`
(`Loader::move_entry` with `out.noop` asserted against a two-entry
tree).

## Consuming `interval_stream`

Poll the stream yourself; ticks queue while nobody polls. After the
owning fiber disposes, the stream yields exactly ONE final
`Err(InactiveEffect)`, then terminates. Live ticks queued before the
disposal are discarded.

```rust
use cordis::timer::{interval_stream, with_current_fiber, InactiveEffect};
use cordis::{Fiber, timer::Stream};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

let fiber = Arc::new(Fiber::new());
let mut stream =
    with_current_fiber(&fiber, || interval_stream(Duration::from_millis(10)));

// Collect two live ticks (Ok items).
let mut live_ticks = 0u32;
while live_ticks < 2 {
    // poll_stream_once wraps Stream::poll_next with a noop waker.
    match poll_stream_once(&mut stream) {
        Some(Ok(())) => live_ticks += 1,
        Some(Err(_)) => unreachable!("not disposed yet"),
        None => std::thread::sleep(Duration::from_millis(2)),
    }
}

// Dispose through the owning fiber.
fiber.dispose().await.expect("dispose ok");

// Exactly ONE final error, then end-of-stream.
assert_eq!(poll_stream_once(&mut stream), Some(Err(InactiveEffect)));
assert_eq!(poll_stream_once(&mut stream), None);
```

Where `poll_stream_once` is the small helper from the source test:

```rust
fn poll_stream_once(stream: &mut cordis::timer::Interval) 
    -> Option<cordis::timer::TickResult> 
{
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(&waker);
    match Stream::poll_next(Pin::new(stream), &mut cx) {
        Poll::Ready(item) => item,
        Poll::Pending => None,
    }
}
```

Dropping the `Interval` also stops scheduling, so ownership without a
fiber stays safe.

Source: `interval_stream_final_err_on_dispose` and
`interval_stream_discards_stale_live_ticks_before_final_err` in
`crates/cordis/src/timer.rs`.

## `emit_filtered` with a Global Bypass

Register an audit listener with `global: true` when it must observe every
dispatch, even filtered ones. The filter excludes only non-global
listeners; the global one always runs. Exclusion is per dispatch: nobody
is unregistered, and a later unfiltered emit runs everyone again.

```rust
use cordis::events::{EventOptions, EventsService};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

let svc = EventsService::new();
let ran = Arc::new(AtomicUsize::new(0));

// Non-global listener for tenant "b".
let b = ran.clone();
svc.on_with("global.test".into(), EventOptions::default(), move |payload| {
    let b = b.clone();
    async move {
        if payload["tenant"] == "b" {
            b.fetch_add(1, Ordering::SeqCst);
        }
        Ok(payload)
    }
});

// Global listener: exempt from every filter verdict.
let g = ran.clone();
svc.on_with(
    "global.test".into(),
    EventOptions { prepend: false, global: true },
    move |payload| {
        let g = g.clone();
        async move {
            if payload["tenant"] == "b" {
                g.fetch_add(10, Ordering::SeqCst);
            }
            Ok(payload)
        }
    },
);

// A filter that admits NOTHING still lets the global listener run.
svc.emit_filtered(
    "global.test".into(),
    serde_json::json!({ "tenant": "b" }),
    Box::new(|_opts| false),
)
.unwrap();
assert_eq!(ran.load(Ordering::SeqCst), 10);
```

Use this for security audit trails, metrics, and tracing sinks. Anything
that must not miss an event rides `global: true`.

Source: `global_bypasses_filter` in `crates/cordis/src/events.rs`.

## Inspecting Values Mid-Transition with `get_relaxed`

Strict `Context::get` refuses values owned by fibers resting in
`Loading`, `Reloading`, `Unloading`, or reactive `Pending`. Lifecycle
and observer code needs exactly those values. `get_relaxed` resolves a
locally-owned value while its owner transitions; terminal `Failed` and
disposed owners stay refused in relaxed mode too.

```rust
use cordis::{Context, Fiber, Service};
use std::sync::Arc;

struct TransitionProbe(u32);
impl Service for TransitionProbe {}

let ctx = Context::new_root();
let fiber = Arc::new(Fiber::new());
fiber.set_reload_context(&ctx);
fiber.set_id(96_001);

// Provide ON the registration fiber so the owner link exists.
ctx.provide_on_fiber(Arc::new(TransitionProbe(7)), &fiber);

// Strict get refuses non-Active owners...
assert!(ctx.get::<TransitionProbe>().is_none());

// ...but relaxed reads serve the transitioning value itself.
fiber.set_state(cordis::FiberState::Pending);
assert_eq!(ctx.get_relaxed::<TransitionProbe>().unwrap().0, 7);
```

The setup calls `set_reload_context`, `set_id`, `provide_on_fiber`, and
`set_state` are crate-internal. The source test uses them to place the
owner fiber into each transitioning state directly. Product code reaches
those states through a readiness gate or a dependency loss instead; only
`get_relaxed` is public API.

Reach for this in diagnostics endpoints, state inspectors, and tests —
never in ordinary consumers. Consumers keep strict `get` so they never
observe half-torn configurations.

Source: `relaxed_read_succeeds_while_provider_transitioning` in
`crates/cordis/src/context.rs`.

## Inspecting a Deferred Config After an Update Veto

An `internal/update` listener returning JSON `false` vetoes the restart.
The proposed config parks in `Fiber::vetoed_config`, the fiber stays
`Active` on its old application, and `update` returns `Ok(())`. Only
explicit `false` is a veto; any other non-null value proceeds. Operators
can read what was deferred and apply it later inside the window.

```rust
use cordis::{Context, EventsService, Fiber};
use std::sync::Arc;

let ctx = Context::new_root();
let events = Arc::new(EventsService::new());
ctx.provide_arc(events.clone());
let fiber = Arc::new(Fiber::new());
fiber.set_reload_context(&ctx);   // crate-internal, see note below
fiber.set_id(70_300);             // crate-internal
// ... install a reload runner and satisfy its declared injects ...

fiber.set_raw_config(serde_json::json!({ "deferred": true })); // crate-internal

// An explicit JSON `false` bail verdict IS the veto.
let gate = events.on(
    cordis::events::INTERNAL_UPDATE_EVENT.into(),
    |_p| async move { Ok(serde_json::json!(false)) },
);
fiber.update(&ctx).await.expect("veto is Ok, not an error");
gate.dispose();

assert!(matches!(fiber.state(), cordis::fiber::FiberState::Active { .. }));
assert_eq!(
    fiber.vetoed_config(),
    Some(serde_json::json!({ "deferred": true })),
);
```

The setup calls `set_reload_context`, `set_id`, and `set_raw_config`
are crate-internal. The source test uses them to stage a minimal fiber.
Product code gets the same state from a normal `RegistryService`
registration; only the veto listener, `Fiber::update`, and
`Fiber::vetoed_config` are public surface.

Pair the gate with a maintenance-window check. During the window return
null (proceed); outside it return `false` (defer). A later update call
with a fresh proposed config overwrites `vetoed_config`.

Source: `update_veto_defers_config_and_returns_ok` in
`crates/cordis/src/fiber.rs`.

## Per-Fiber Log Level Override via `LoggerIntercept`

Install a `LoggerIntercept` on a child context to quiet one noisy logger
for one subtree. Writes through that context handle resolve the override
at write time; writes through other handles keep the ambient
configuration. `name: None` matches every logger.

```rust
use cordis::logger::{LogLevel, LoggerIntercept, LoggerService};

let root = Context::new_root();
root.provide(LoggerService::new());

// Fiber-scoped override: only "svc" drops to Error-only on the child.
let child = root.intercept(LoggerIntercept {
    name: Some("svc".into()),
    level: Some(LogLevel::ERROR),
});
child.debug("svc", vec!["suppressed".into()]);
child.error("svc", vec!["survives".into()]);
// Non-matching names keep the ambient configuration.
child.debug("other", vec!["other-passes".into()]);

// Wildcard intercept: name=None forces the level for every logger.
let wild = root.intercept(LoggerIntercept {
    name: None,
    level: Some(LogLevel::ERROR),
});
wild.info("anything", vec!["blocked".into()]);
```

`level: Some(l)` replaces the effective threshold for matching writes,
over both per-name pins and the default. Stack two intercepts and the
innermost matching layer wins, like every layered override. Use the
scoped form for request-scoped suppression; use the wildcard form for a
temporary global mute during a hot path benchmark.

Source: `logger_intercept_overrides_level` in
`crates/cordis/src/logger.rs`.
