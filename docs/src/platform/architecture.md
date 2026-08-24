# Architecture

ARES is a multi-tenant AI agent runtime. It uses a service-based architecture. Components register themselves into a shared `Context`, and handlers pull what they need at request time.

## Core concepts

| Concept | What it does |
|---------|-------------|
| Context | Typed service container. It holds all shared state. Handlers receive `Arc<Context>` and call `ctx.get::<T>()` to access services. |
| Service | Any `Send + Sync + 'static` type registered in the context. It implements `name()`, `init()`, and `check()`. |
| Fiber | Lifecycle state machine for a service instance. It tracks whether a service is active, reloading, or inactive. Epoch-based change detection triggers refresh when dependencies change. |
| Plugin | Registers a service into the context. It returns a disposable that undoes registration on drop. |
| Loader | Reads `config/cordis-entries.toml` and reconciles desired state with current state (rebuild, update, retire, or begin). |

## Request flow

```
HTTP request
  -> Axum router
  -> Handler extracts `State<Arc<Context>>`
  -> Handler calls ctx.get::<Execute>()
  -> Execute resolves agent (tenant DB -> community -> system)
  -> Creates agent via AgentRegistry
  -> Calls agent execution with the message and context
  -> Agent runs the ToolCoordinator loop for multi-turn tool calling
  -> Response returned
```

## Workspace crates

| Crate | Purpose |
|-------|---------|
| `ares-types` | Shared types, error definitions |
| `ares-store` | PostgreSQL client, migrations, tenant DB, fleet secrets |
| `ares-llm` | Provider registry, LLM clients (OpenAI-compatible, Anthropic, Ollama, NVIDIA) |
| `ares-agent` | Execute engine, ConfigurableAgent, AgentRegistry, loop detection, checkpointing |
| `ares-tools` | Tool trait, built-in tools, tenant-aware tool resolution |
| `ares-mcp` | MCP client integration |
| `ares-rag` | Vector search, BM25, hybrid retrieval |
| `ares-vector` | Pure-Rust vector store |
| `cordis` | Context, Fiber, Service, Registry, Loader, Events, ReflectService |

## Key services

**Execute** (in `ares-agent`): the single entry point for running agents. It resolves the agent configuration, creates the agent, tracks the run through the optional `RunTracker`, and executes it. All five call sites (chat, v1 API, scheduler, pipeline, trigger) delegate here.

**Resolver** (in `ares-agent`, crate-private): 3-tier agent resolution. It queries the tenant DB first, then community agents, then system config. It returns the resolved agent configuration and source tier.

**Llm** (in `ares-llm`): LLM provider management with circuit breaker (closed/open/half-open). It supports per-request model override without changes to global state.

**Tools** (in `ares-tools`): merges static tools, runtime DB tools, and MCP tools behind one interface. Precedence: tenant runtime, fleet runtime, static (static includes MCP bridge registrations).

**ReflectService** (in `cordis`): coordinates hot-reload. When a file changes or a DB row updates, `notify(type_id)` walks dependent fibers and triggers refresh.

## Server bootstrap

`src/main.rs` creates a root context, then boots the loader program from `config/cordis-entries.toml`. Loader factories register services such as Store, Llm, Tools, and Http:

```rust
let root_ctx = Context::new_root();
// Register plugins (services with lifecycle)
root_ctx.plugin(ConfigService).await?;
root_ctx.plugin(CalculatorService).await?;
// ...
// Provide data services
root_ctx.provide_arc(agent_registry.clone());
root_ctx.provide_arc(llm_factory.clone());
root_ctx.provide(Execute::new()
    .with_db(db).with_tenant_db(tdb).with_agent_registry(reg)
    .with_fleet_secrets(secrets).with_run_tracker(active_runs));
// Build router
let app = build_router(root_ctx.clone());
```

## Hot-reload

The server detects file changes via the `notify` crate with 500ms debounce. When a watched file changes:

1. The watcher calls `ReflectService::notify(type_id)`.
2. `ReflectService` walks dependent fibers via BFS.
3. Each fiber recomputes its epoch from dependency versions.
4. If the epoch changed, the fiber reloads with new config.

No polling loops run. No 60-second stale windows exist.

## Adding a new tool

1. Implement the `Tool` trait in `crates/ares-tools/src/tools/`.
2. Register it in the tool registry (`tool_registry.register(Arc::new(MyTool))`) or ship a loader entry in `config/cordis-entries.toml`.
3. Optionally implement `Service` and register via `ctx.plugin(MyToolService)`.

## Adding a new LLM provider

1. Implement the `LLMClient` trait in `crates/ares-llm/src/`.
2. Add the provider to the provider registry in config.
3. The circuit breaker wraps it automatically.

## Build

```bash
# Development (all features)
cargo build --features openai,postgres,mcp

# Minimal (no external deps)
cargo build --no-default-features

# Release
cargo build --release --features openai,postgres,mcp
```

Rust 1.98 is required (`rust-toolchain.toml` pins it).
