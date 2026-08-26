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
