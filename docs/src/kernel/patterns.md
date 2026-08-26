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
