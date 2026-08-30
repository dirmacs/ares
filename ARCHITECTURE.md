# Architecture

ARES is a multi-tenant AI agent runtime. Components register as services in a shared Cordis `Context`. Handlers pull the services they need at request time.

## Core concepts

| Concept | What it does |
|---------|-------------|
| Context | Typed service container. Holds all shared state. Handlers receive `Arc<Context>` and call `ctx.get::<T>()` to access services. |
| Service | Any `Send + Sync + 'static` type registered in the context. Implements `name()`, `init()`, and `check()`. |
| Fiber | Lifecycle state machine for a service instance. Tracks the `Active`, `Loading`, `Reloading`, `Unloading`, `Inactive`, and `Failed` states. Epoch-based change detection triggers a refresh when dependencies change. |
| Plugin | Declarative unit that provides a service into the context. Its `apply` method builds the service from the context and the entry configuration. Registration returns a fiber id. |
| Loader | Reads the composed `config/cordis-entries.toml` program (TOML with `@include` splice, `@group` flatten, `${rhai: …}` interpolation) and reconciles desired state with current state (`Begin`, `RebuildFiber`, `UpdateConfig`, `Retire`). Every action lands in the loader journal. |

## Request flow

```
HTTP request
  -> Axum router
  -> Handler extracts `State<Arc<Context>>`
  -> Handler calls ctx.get::<Execute>()
  -> Execute resolves the agent (tenant DB -> community -> system)
  -> Creates the agent via AgentRegistry
  -> Calls Execute::run(request, context)
  -> Execute drives the multi-turn loop via ToolCoordinator
  -> Response returned
```

## Workspace crates

| Crate | Purpose |
|-------|---------|
| `ares-types` | Shared types, error definitions |
| `ares-store` | PostgreSQL client, migrations, tenant DB, fleet secrets |
| `ares-llm` | Provider registry and `Llm` service. HTTP providers use genai. LlamaCpp is optional. |
| `ares-agent` | Agent trait, ConfigurableAgent, Execute service, 3-tier resolver |
| `ares-tools` | Tool trait, built-in tools, runtime tool registry |
| `ares-mcp` | MCP client integration |
| `ares-rag` | Vector search, BM25, hybrid retrieval |
| `ares-vector` | Pure-Rust vector store |
| `cordis` | Cordis kernel: Context, Fiber, Service, Registry, Loader, Events, ReflectService |

## Key services

**Execute** (in `ares-agent`): The single entry point to run agents. It resolves the agent through the 3-tier resolver. It creates the agent through `AgentRegistry`. It tracks the run through `RunTracker` and drives the multi-turn loop. All call sites (chat, v1 API, scheduler, pipeline, trigger, MCP runner) delegate here.

**Agent resolver** (in `ares-agent`): 3-tier agent resolution. It queries the tenant DB first, then community agents, then system configuration. It returns the resolved agent configuration and the source tier.

**Llm** (in `ares-llm`): LLM provider management with a circuit breaker (`Closed`/`Open`/`HalfOpen`). It supports a per-request model override through `ModelOverride` without mutation of global state. It also caches the NVIDIA model catalog for capability-based selection.

**Tools** (in `ares-tools`): Merges static tools, tenant runtime tools, fleet runtime tools, and MCP bridge registrations behind one interface. Precedence: tenant runtime, fleet runtime, static. Static registrations include the MCP bridge.

**ReflectService** (in `cordis`): Coordinates hot-reload. On a file change or a DB row update, `notify(type_id)` walks dependent fibers and triggers a refresh.

## Server bootstrap

`src/main.rs` creates the root context and registers the loader factories. The loader then applies `config/cordis-entries.toml` in order. Plugins such as EventsService, Overlay, Store, Tools, Llm, Execute, and Http register through it. The `Http` plugin owns `/health` and `/api`. The binary merges extra routes on top and binds the listener:

```rust
let root_ctx = Context::new_root();
root_ctx.plugin(cordis::ReflectService::new()).await?;
// Register loader factories (capability crates + server extras)
register_loader_factories(&root_ctx);
// Boot: one ordered pass over config/cordis-entries.toml
boot_loader_program(&root_ctx, entries_path, &config_path)?;
// The Http plugin serves the router. The binary merges extra routes
let mut app = http.router.clone().merge(extra);
axum::serve(listener, app.into_make_service_with_connect_info())
    .await?;
```

## Hot reload

The `notify` crate watches files with a 500 ms debounce. When a watched file changes:
1. The watcher calls `ReflectService::notify(type_id)`
2. ReflectService walks the dependent fibers through BFS
3. Each fiber recomputes its epoch from the dependency versions
4. If the epoch changed, the fiber reloads with the new configuration

A watcher failure falls back to a 30-second poll for `cordis-entries.toml`.

## Dispatcher parity

The event dispatcher (`EventsService`) mirrors the five Cordis dispatch modes. `Emit` invokes every handler fire-and-forget on the runtime and broadcasts the event on the bus. It returns immediately. `Parallel` fans the handlers out across a `JoinSet` and propagates the first observed error. `Serial` threads the payload through each handler in order and aborts on the first error. `Bail` stops at the first handler that returns a non-null result. Later handlers do not run. `Waterfall` is a serial around-middleware chain. Each handler receives `(payload, next)`. A call to `next` delegates downstream. A return without `next` short-circuits. All 22 catalog events carry typed payloads. A consistency test enforces the binding against the catalog. Every dispatch increments a counter exposed at `GET /admin/cordis/events`. `RhaiPolicy` entries attach sandboxed script listeners to any catalog event.

## Loader journal

`LoaderJournal` keeps live bookkeeping for loader entries. Each record stores the plugin label, the last applied configuration, the live fiber id, and a monotonically increasing generation counter. `Loader::execute_action` and `Loader::instantiate` update it. `RebuildFiber` and `instantiate` upsert a record. `UpdateConfig` bumps the generation and reaches the live fiber through `RegistryService::get_fiber` to call `Fiber::update`. `Retire` clears the record. Provide the journal as a service with `ctx.provide(LoaderJournal::new())`. Without it, loader actions degrade to log-only.

## Fiber lifecycle

A fiber moves through lifecycle states as services arrive and reload. `Active` and `Inactive` mean satisfied or missing dependencies. `Reloading` and `Unloading` cover in-flight change and teardown. `Loading` marks a fiber mid-instantiation. `Failed { error }` records a terminal activation failure. Failed fibers stay wired into `ReflectService`, so their dependents receive notifications. The admin Cordis endpoints and the loader journal expose them for inspection. Re-registration of the same key supersedes the failed fiber with a fresh fiber id.

The kernel guarantees in `cordis::metatheory` cover guarded withdrawal (providers cannot retire while active consumers exist), eager inject reconciliation (declarations on Active fibers take effect immediately and race-free), peer-dependency versioning (`provide_versioned` / `declare_inject_versioned` keep dependents Inactive on incompatible versions), LIFO disposal, order-confluent registration, and quiescence after every operation. Each guarantee has a property test.

## Adding a new tool

1. Implement the `Tool` trait in `crates/ares-tools/src/tools/`
2. Register the tool in the registry that the ares-tools plugin factory builds
3. Optionally implement `Service` and register through `ctx.plugin(MyToolService)`

## Adding a new HTTP LLM provider

1. Add a `ProviderConfig` variant in `crates/ares-llm/src/config.rs`.
2. Map that variant to a genai `AdapterKind` in `crates/ares-llm/src/client.rs`.
3. Keep HTTP traffic in `crates/ares-llm/src/genai_client.rs`. Do not add a crate feature. Do not add `openai.rs`.
4. Add the model configuration to `ares.example.toml`.

## Build

```bash
# Default build (postgres, ares-vector, mcp, inventory, rhai-policy, http, cli)
cargo build

# Embed-only build without the server defaults
cargo build --no-default-features --features postgres,mcp

# Optional in-process GGUF
cargo build --features llamacpp

# Release
cargo build --release
```

The build requires Rust 1.98. `rust-toolchain.toml` pins the version.
