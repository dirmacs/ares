# Kernel concepts

The Cordis kernel is a typed service graph with a lifecycle engine. This chapter explains the ideas. The how-to chapters cover the state machine and interception in detail.

## Ideas

**Typed service graph.** A `Context` stores services keyed by Rust `TypeId`. A child context walks its parent chain like a prototype chain. Isolate labels mark realm boundaries. Two realms can provide the same service type. Multi-tenant isolation uses this.

**Fibers own lifecycles.** Each registration creates a fiber. The fiber holds the state machine, the dependency epoch, and an undo accumulator. When a provider changes, the kernel refreshes dependent fibers. Disposing a fiber runs every undo in reverse order (LIFO).

**Effects are undos.** Every provide, timer, and listener pushes an undo closure onto its fiber. There is no separate effect tree. Undo metadata carries a label and a timestamp for inspection.

**Event-first middleware.** Product code talks over an event bus with five dispatch modes: `emit`, `parallel`, `serial`, `bail`, and `waterfall`. Waterfall handlers wrap downstream work through a `next` continuation.

**Interception.** Five kernel meta-events act as veto points around reads, writes, config resolution, restarts, and listener registration. A sixth meta-event observes every non-internal dispatch. With no listener registered, each point costs two map lookups.

**Layered overrides.** An intercept override shadows one service type. Overrides stack in layers. The innermost layer wins. Accessors are name-keyed computed properties beside the TypeId store; they bypass interception on purpose.

**Declarative management.** A loader applies entry trees against a journal. It stages changes in two phases and rolls back on failure. A reflect service fans change notifications out through a dependency graph.

## Why a kernel

Applications wire objects together by hand. Hand wiring creates four
recurring problems. The kernel solves each one with one mechanism.

**Problem 1: wiring.** Components reference each other by concrete type.
Every change to a component ripples through every construction site.
The kernel replaces hand wiring with typed lookup: provide once, resolve
anywhere through the parent chain. Consumers name what they need; nobody
names who builds it.

**Problem 2: lifecycle.** Services start in the wrong order and stop in
the wrong order. Timers, listeners, and connections leak when shutdown
misses one. The kernel gives every registration a fiber. The fiber tracks
every undo closure in registration order. Teardown runs them in reverse,
always, even during reactive dependency churn.

**Problem 3: observation.** Once components talk directly, nothing sees
the traffic. Debugging relies on print statements at every call site.
The kernel routes reads, writes, config resolution, restarts, and listener
registration through six meta-events. One subscription observes every
sensitive operation. With no subscriber, each point costs two map lookups.

**Problem 4: replacement.** Swapping an implementation means touching
call sites or restarting the process. The kernel separates declaration
from use. A loader stages a new entry tree in two phases and rolls back
on failure. Layered intercept overrides shadow one service type for one
subtree without touching the parent context. Hot swap of native plugins
rides the same fibers.

The result is one graph that owns construction, teardown, inspection,
and substitution. Product code states facts; the kernel keeps the facts
true.

## Glossary

| Term | Definition |
| --- | --- |
| Fiber | Lifecycle owner of one registration. Holds the state machine, the dependency epoch, and the undo accumulator. Type: `cordis::fiber::Fiber`. |
| Service | Any value stored by Rust `TypeId`. Implement the empty `Service` trait to participate. |
| Effect | One labeled undo closure on a fiber. Every provide, timer, and listener pushes one. Disposal pops effects last-in, first-out. |
| Event | Named message on the event bus. Dispatch modes: `emit`, `parallel`, `serial`, `bail`, `waterfall`. |
| Meta-event | Kernel-owned veto point such as `internal/get` or `internal/set`. Distinct from product events. |
| Isolate | Realm label marking a boundary. Contexts beyond an isolate do not see layers or services across it. Multi-tenant isolation uses this. |
| Loader | Declarative engine applying entry trees against a journal in two phases, with rollback. |
| Epoch | String encoding every declared dependency version. A changed epoch triggers a refresh pass. |
| Readiness gate | Predicate holding a fiber in reversible `Pending` until it reports ready. |
| Accessor | Name-keyed computed property beside the `TypeId` store. Bypasses interception by design. |

## Map: idea to module

| Idea | Implemented in |
| --- | --- |
| Typed store, parent walk, isolate realms, accessors, layered overrides | `cordis::context` |
| Fibers, states, epochs, undo accumulator | `cordis::fiber` |
| Single-provider registry, plugins, readiness gates | `cordis::registry` |
| Disposable effects | `cordis::effect` |
| Event bus, dispatch modes, meta-events | `cordis::events` |
| Declared event contracts and typed payloads | `cordis::events_catalog`, `cordis::events_payload` |
| Errors | `cordis::service` (`CordisError`) |
| Declarative loader, staged batches, journal | `cordis::loader`; `LoaderJournal` in the crate root |
| Change fan-out and BFS refresh | `ReflectService` in the crate root |
| File-watch hot reload | `cordis::watcher`, `cordis::reload`, `cordis::stamp` |
| Plugin-module graph | `cordis::module_graph` |
| Native-code hot swap | `cordis::hmr` |
| Dependency-cycle detection | `cordis::cycles` |
| Peer-dependency versions | `Context::VERSION_MAJOR_SCALE` (`context`), constraints in `fiber` |
| Executable kernel guarantees | `cordis::metatheory` |
| Timer and logger primitives | `cordis::timer`, `cordis::logger` |
| Entry composition (`@include`, `@group`, Rhai interpolation) | `cordis::compose`, `cordis::rhai_service` |
| Supervised worker exit protocol | `cordis::worker` |

Read [Lifecycle](lifecycle.md) for the full state machine. Read [Interception](interception.md) for the exact veto semantics.
