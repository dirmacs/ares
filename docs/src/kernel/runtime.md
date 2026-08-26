# Runtime Services

This chapter describes the runtime services that live beside the kernel
core: effect ownership, the fiber-scoped timer suite, the logger service,
the module graph, and the tenant file fence. Every signature here comes
from `crates/cordis/src` and `crates/ares-tools/src/fence.rs`.

## Effect Ownership

The kernel models cleanup as *effects*. An effect is anything that
implements one method:

```rust
pub trait Disposable: Send + 'static {
    fn dispose(self: Box<Self>);
}
```

Every closure with a compatible signature is a `Disposable`. A fiber
holds its effects as labeled undo entries. `Fiber::dispose` pops them in
last-in, first-out (LIFO) order and runs each undo once. A reactive pass
through `Unloading` runs the same stack. This gives one rule: teardown
order is the reverse of registration order, always.

### EffectHandle dispose semantics

The timer suite returns an `EffectHandle` per registration. Its rules:

- Dropping the handle does NOT cancel the effect. Callers must dispose
  it explicitly or let the owning fiber do it.
- Clones share one cancellation flag. Disposing any clone cancels the
  registration.
- Disposal is idempotent: the flag flips once and the teardown hook runs
  once.
- `handle.is_cancelled()` reports the state at any time.
- Each handle pushes a labeled undo (prefix `timer:`) onto the current
  fiber scope. Fiber disposal therefore cancels timers without caller
  action.

A registration made outside a fiber scope logs a warning and returns an
*orphan* handle. An orphan still works when you dispose it by hand; no
fiber cancels it automatically.

### Inert handles

Two APIs return handles that may be *inert*: disposing them flips
nothing.

- Event listener registration rides the `internal/listener` veto point.
  When that chain bails or errors, the registration is cancelled before
  it enters either registry. The caller receives an inert handle whose
  `dispose` does nothing. The failure is fail-closed: an erroring veto
  chain cancels too.
- `Context::register_accessor` returns an accessor `EffectHandle`.
  `handle.dispose()` removes the declaration and every alias bound to
  it, and returns `true` only when the declaration was still live. After
  removal, reads resolve `None`.

## Fiber-Scoped Timer Suite

`cordis::timer` provides six primitives. All of them run on one shared
timer thread named `cordis-timer`, never on the owning task. The thread
sleeps until the nearest deadline in a shared wheel, drains all due
entries under one short lock, then runs callbacks outside the lock.
Callbacks must be cheap and non-blocking. A panicking callback is caught
and logged; the thread survives.

### Wheel mechanics

The wheel is a min-heap of entries ordered by `(deadline, seq)`. The
sequence number breaks ties so equal deadlines fire in registration
order. One process-wide instance serves every fiber; it lives behind a
`LazyLock<Mutex<Wheel>>`.

The driver loop has two phases:

1. **Sleep.** Read the nearest deadline while holding the lock, release,
   then `park_timeout` for that long. A park with no timeout waits for
   the next insert. Any `schedule` call pushes its entry and unparks the
   thread, so an earlier deadline preempts a long sleep immediately.
2. **Drain.** Re-acquire the lock, pop every entry whose deadline is at
   or before now, release, then run each job. Callbacks run outside the
   lock on purpose: an interval callback re-arms itself by calling
   `schedule`, which needs the lock. Holding it across callbacks would
   deadlock every self-re-arming pattern, including debounce and
   throttle emits.

Cancellation is cooperative through `EffectHandle`. Disposal flips the
shared flag but does not remove the heap entry. Two paths make that
safe:

- The `timeout` job checks the flag inside the job body. A disposal
  racing the drain still prevents the callback from running.
- The future-based `sleep` resolves early and silently when disposed
  while pending; nothing observes a cancelled wake-up.

A panicking callback lands in `catch_unwind`. The driver logs a warning
and moves to the next job. One bad callback cannot starve the rest of
the wheel.

| Primitive | Shape |
|---|---|
| `timeout(delay, callback)` | One-shot delay, then callback. Returns `EffectHandle`. |
| `sleep(delay)` | One-shot delay as a future. Returns `(EffectHandle, impl Future)`. |
| `interval(delay, callback)` | Repeating callback. Returns `EffectHandle`. |
| `interval_stream(delay)` | Repeating ticks as a pollable stream. Returns `Interval`. |
| `debounce(delay)` | Trailing-edge burst collapse. Returns `Scheduled<T>`. |
| `throttle(delay, no_trailing)` | Leading edge plus optional trailing edge. Returns `Scheduled<T>`. |

Register inside `with_current_fiber(&fiber, || ..)` to attach the effect
to that fiber. Key behaviors:

- `timeout` checks the cancellation flag inside the job, so a disposal
  that races the drain still prevents the callback from running.
- `interval` re-arms the next tick from the moment each tick fires. The
  cadence never runs ahead of the callback.
- `sleep` resolves early and silently when its handle is disposed while
  the future is pending.
- `interval_stream` queues ticks in a channel while nobody polls. After
  disposal the stream yields exactly ONE final
  `Err(InactiveEffect)` item, then closes. Ticks queued before the
  disposal are discarded, so teardown is always the final observation.
- `debounce` keeps only the last value of a burst and delivers it after
  a quiet window. `throttle` delivers the first value immediately and,
  unless `no_trailing` is set, the last value of the window at close.

`Scheduled<T>` pairs a submit side (`call`) with a consumer side
(`receive`, `receive_timeout`). Cancellation drops pending values; later
receives return `None`.

## LoggerService

`LoggerService` is a bounded ring of recent messages plus exporter
fan-out. Provide it once on the root context:

```rust
ctx.provide(LoggerService::new());
```

The default capacity is 1000 messages. Use `LoggerService::with_capacity`
to change it. The oldest message leaves first at capacity. `snapshot`
returns detached clones, oldest first.

### Write path

Every write follows four steps:

1. Resolve the effective threshold for the logger name.
2. Bail BEFORE argument assembly when the kind fails the gate.
3. Append the record to the ring.
4. Fan out to every exporter that accepts `(name, kind)`.

Step 2 matters for cost. Prefer `log_with` with a closure so disabled
paths never build arguments:

```rust
logger.log_with(&ctx, "db", LogKind::Debug, || {
    vec!["rows".into(), count.into()]
});
```

Convenience methods `error`, `warn`, `info`, and `debug` take pre-built
arguments and still pass the gate. Facade methods on `Context`
(`ctx.info(..)`, and so on) no-op when no `LoggerService` is provided.

### Levels

Severity ranks are numeric: `Error=0`, `Warn=1`, `Info=2`, `Debug=3`.
Lower means more severe. A kind passes a threshold when its rank is less
than or equal to the threshold value.

- `set_default_level` pins the threshold for unlisted names. The default
  is `DEBUG`, which passes everything.
- `set_level(name, level)` pins one name. It wins over the default.
- `clear_level(name)` removes a pin.

### Exporters

An exporter implements `export(&self, message: &Message, text: &str)`.
It runs inline on the writer's thread and must not panic. Registration
takes an `ExporterConfig` with two fields:

- `levels`: per-name thresholds for this sink. Unlisted names pass.
- `max_length`: character cap on rendered text. Default 4096.

`register` returns a `Box<dyn Disposable>`. Disposing it removes the
sink. The buffer keeps recording after a sink leaves.

### Printf placeholders

When the leading argument is a string containing `%`, `Message::render`
treats it as a format string:

| Specifier | Meaning |
|---|---|
| `%s` | String |
| `%d`, `%i` | Integer |
| `%f` | Float |
| `%o` | Compact JSON object |
| `%O` | Pretty JSON object |
| `%c` | Colorized with the stable palette slot for this name |
| `%C` | Bold colorized variant of `%c` |
| `%%` | Literal percent |

Unknown specifiers and exhausted arguments stay literal. Unconsumed
arguments join at the end with spaces. Without a format head, arguments
join with single spaces.

### Stable color slots

`%c` and `%C` pick one of sixteen ANSI colors from the logger name. The
slot must be stable across processes and platforms, so it comes from a
hash, not a counter. `name_color_code` in `logger.rs` computes FNV-1a
over the name bytes:

$$h_0 = \texttt{0xcbf29ce484222325}, \qquad h_{i+1} = (h_i \oplus b_i) \cdot \texttt{0x00000100000001b3} \;\bmod\; 2^{64}$$

where $b_i$ is the $i$-th name byte. The palette index is then

$$\text{color}(name) = \text{ANSI16}[\,h \bmod 16\,]$$

with `ANSI16 = [30..=37, 90..=97]`: eight normal foregrounds followed by
their bright variants. The multiplication wraps (`wrapping_mul`), so no
input can overflow-panic. The same name always renders in the same
color — in tests, in production logs, and across restarts. Test anchor:
`colorization_is_stable_per_name`.

### LoggerIntercept override

`LoggerIntercept` rides the normal intercept channel. Install it with
`ctx.intercept(..)`. Writes through that context handle resolve it at
write time:

```rust
let child = root.intercept(LoggerIntercept {
    name: Some("svc".into()),   // None matches every logger
    level: Some(LogLevel::ERROR),
});
```

`level: Some(l)` replaces the effective threshold for matching writes,
over both pins and defaults. Names that do not match keep the ambient
configuration.

### Derived names

`hyphenate` turns `CamelCase` into `kebab-case` and handles acronym
heads (`HTTPServer` becomes `http-server`). `derived_name::<T>()`
applies it to the short type name. Use it for logger naming:
`ctx.info(&derived_name::<Self>(), ..)`.

## Module Graph Transactional Reloads

The watcher fans file changes out to service-level dependents by
`TypeId`. That layer cannot answer "which plugin must reload because
this file changed?" because file edges carry no `TypeId`. `ModuleGraph`
is that missing layer.

Callers register every dynamic module under a key, usually the watched
file stem:

```rust
graph.register_module("foo", vec!["shared".into()], "FooPlugin");
```

Each entry carries its declared dependencies and the plugin that
implements it.

### Transaction shape

`ModuleGraph::change_many(ctx, keys)` runs one settled batch in two
phases:

1. **Compute** (read-only): walk the transitive dependent set across ALL
   input keys with a shared visited set. Cycles terminate. A plugin
   reachable from several inputs appears exactly once. If no input key
   matches a registered module, nothing is computed.
2. **Apply** (sequential): reload each affected plugin through the
   `ModuleReload` seam in breadth-first propagation order. The FIRST
   failure rolls that plugin back to its previous state and stops the
   batch. Earlier successes stay active.

The classified result is a `ChangeOutcome`:

- `Ignored`: no key matched a registered module. Nothing changed.
- `Reloaded(plugins)`: every affected plugin reloaded, deduped.
- `RolledBack { reloaded, failed_plugin, error }`: names what applied,
  what failed, and the error text. The text also reports a rollback
  failure when the restore itself failed.

The default seam, `NoopReload`, never fails. Deployments wire their own
`reload` / `rollback` pair, or swap one in later with `set_reloader`.

### One transaction, narrated

Watch two shared files change at once: `routes.toml` and `auth.toml`.
Three modules depend on them. `foo` depends on both; `bar` depends on
`shared`; `baz` is independent.

1. The watcher's debounce settles with both paths. Each path maps to its
   file stem, and the watcher hands `["routes", "auth"]` to
   `change_many`.
2. The compute phase walks the transitive dependent set across BOTH keys
   with one shared visited set. It reaches `foo` through either key but
   records it once — dedup happens during the walk, not after.
3. The apply phase reloads affected plugins in breadth-first order:
   dependencies before their dependents, so each plugin reloads into a
   kernel where what it injects already exists.
4. Suppose `bar`'s reload fails on its new code. The seam rolls `bar`
   back to its previous state and the batch stops there. `baz` never ran
   — it matched no input key.

The result is
`RolledBack { reloaded: ["foo"], failed_plugin: "bar", error: .. }`.

**Sibling survival.** Plugins that reloaded before the failure stay
active on their NEW code. The batch does not unwind earlier successes.
This guarantee shapes how you write reloaders:

- A reload must leave the kernel consistent on its own. Earlier siblings
  will not be reverted for you.
- Order failures so cheap ones fail first when possible; breadth-first
  order plus early failure minimizes divergence between old and new.
- The rollback text reports a restore failure separately. Rolling back
  `bar` can itself error; the error field says so. Surface that case as
  an operator alert: the running state then matches no recorded state.

Contrast with the loader's two-phase reload (see
[Lifecycle](lifecycle.md)): the loader rolls back everything newest-first;
the module graph deliberately keeps successful siblings. The loader owns
declarative trees. The module graph owns native-code hot swap, where a
reloaded `.so` cannot always be unloaded again safely.

### Watcher integration

When a debounced watcher batch settles, the watcher maps each changed
path to its file stem and hands those stems to `change_many` — but only
when a `ModuleGraph` is provided on the context. No graph registered
means zero cost; the `TypeId` path stays unchanged. The HMR dynamic
library fingerprint gate is untouched by this layer; neither consults
the other.

## File Fence Layers L0-L3

The tenant filesystem permission fence lives in
`crates/ares-tools/src/fence.rs`. One `Fence` instance serves one
session. Its policy value is pure and shareable; the observed-set ledger
and audit ring sit behind a mutex.

Layers run in fixed order. A path passes only when every active layer
passes:

- **L0 mode**: `FenceMode::ReadOnly` denies every write. Reads still
  pass L1 and L2.
- **L1 boundary**: the resolved path must stay inside `workspace_root`.
  `FenceMode::Full` waives this layer.
- **L2 blocklist**: a blocked name denies reads and writes in every
  mode.
- **L3 write guards**: session-level enforcement over the policy.

`check_read` and `check_write` on `FencePolicy` stay pure path checks
(L0-L2). Only the `Fence` methods touch file contents.

### Layer matrix

The matrix lists, for each layer, what it guarantees and which stable
code reports its failure. Layers run top to bottom; the first failure
decides the code.

| Layer | Guarantee | Fails with | Applies to |
| --- | --- | --- | --- |
| L0 mode | `ReadOnly` denies every write | `FS_FENCE_DENIED` | Writes only |
| L1 boundary | Resolved path stays inside `workspace_root` (`Full` waives) | `FS_FENCE_DENIED` | Reads and writes |
| L2 blocklist | Blocked names denied in every mode | `FS_FENCE_DENIED` | Reads and writes |
| L3 observation | Canonical path observed before any guarded write in non-blind modes; missing paths record version 0 | `FS_NOT_OBSERVED` | Writes only |
| L3 contract | Guard matches observed state: absent path for create, unchanged version for replace | `FS_EXISTS`, `FS_VERSION_CONFLICT` | Writes only |
| L3 I/O | Atomic sibling-temp-plus-rename write | `FS_IO` | Writes only |

Reading the table as an operator:

- Three different denials all report `FS_FENCE_DENIED`; the audit ring
  entry records the reason text that separates them.
- `FS_NOT_OBSERVED` is a protocol error, not a permission error. The
  agent forgot to read before writing. A read of a missing path counts,
  so creating a new file needs one prior failed-or-absent read.
- `FS_VERSION_CONFLICT` means someone changed the file after your read.
  Re-read and re-apply the edit.
- `FS_IO` covers everything underneath the policy: permission bits at
  the OS level, full disks, vanished parents. The reason text carries
  the OS message.

Determinism is the point. The same path, mode, guard, and observed state
always produce the same code. Agent-facing retry logic branches on codes,
not on parsed prose.

### L3 write guards

Every write names a guard contract (`WriteGuard`):

- `Unconditional`: overwrite whatever is there.
- `CreateIfAbsent`: fails with `FS_EXISTS` when the path already exists.
- `ReplaceIfVersion { version }`: fails with `FS_VERSION_CONFLICT` when
  the file is gone or changed since observation.

In modes without blind-write allowance, the canonical path must have
been observed through `Fence::fence_read` first. Otherwise the write
fails with `FS_NOT_OBSERVED`. This covers every contract, including
creating new files. A read records a version fingerprint; a missing path
records version `0`, so a later create can prove absence.

Writes land through a sibling temporary file and an atomic rename, so an
interrupted write leaves no torn file behind. A successful write becomes
the new observed version, so chained guarded writes work against your
own output.

Errors carry stable `FS_*` codes: `FS_NOT_OBSERVED`,
`FS_VERSION_CONFLICT`, `FS_EXISTS`, `FS_FENCE_DENIED`, and `FS_IO`. The
first failing layer determines the code, so agent-facing errors stay
deterministic. Every operation lands in a bounded audit ring
(`audit_log()`); the oldest entry leaves at capacity 200.
