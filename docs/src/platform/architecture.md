# Architecture

ARES is a multi-tenant AI agent runtime. It uses a service-based architecture where components register themselves into a shared `Context` and handlers pull what they need at request time.

## Core concepts

| Concept | What it does |
|---------|-------------|
| Context | Typed service container. Holds all shared state. Handlers receive `Arc<Context>` and call `ctx.get::<T>()` to access services. |
| Service | Any `Send + Sync + 'static` type registered in the context. Implements `name()`, `init()`, and `check()`. |
| Fiber | Lifecycle state machine for a service instance. Tracks whether a service is active, reloading, or inactive. Epoch-based change detection triggers refresh when dependencies change. |
| Plugin | Registers a service into the context. Returns a disposable that undoes registration on drop. |
| Loader | Reads `config/entries.json` and reconciles desired state with current state (rebuild, update, retire, or begin). |

## Request flow

```
HTTP request
  -> Axum router
  -> Handler extracts `State<Arc<Context>>`
  -> Handler calls ctx.get::<AgentExecutionService>()
  -> AgentExecutionService resolves agent (tenant DB -> community -> system)
  -> Creates agent via AgentRegistry
  -> Calls agent.execute(message, context)
  -> Agent uses ToolCoordinator for multi-turn tool calling
  -> Response returned
```

## Workspace crates

| Crate | Purpose |
|-------|---------|
| `ares-types` | Shared types, error definitions |
| `ares-config` | TOML/TOON configuration, fleet secrets |
| `ares-store` | PostgreSQL client, migrations, tenant DB |
| `ares-llm` | Provider registry, LLM clients (OpenAI, Anthropic, Ollama, Nvidia) |
| `ares-agent` | Agent trait, ConfigurableAgent, AgentExecutionService, AgentResolverService |
| `ares-tools` | Tool trait, built-in tools, runtime tool registry |
| `ares-mcp` | MCP client integration |
| `ares-rag` | Vector search, BM25, hybrid retrieval |
| `ares-vector` | Pure-Rust vector store |
| `cordis` | Context, Fiber, Service, Registry, Loader, Events, ReflectService |

## Key services

**AgentExecutionService** (in `ares-agent`): The single entry point for running agents. Resolves the agent config, creates the agent, tracks the run via `RunTracker`, and executes. All five call sites (chat, v1 API, scheduler, pipeline, trigger) delegate here.

**AgentResolverService** (in `ares-agent`): 3-tier agent resolution. Queries tenant DB first, then community agents, then system config. Returns the resolved agent config and source tier.

**LlmService** (in `ares-llm`): LLM provider management with circuit breaker (closed/open/half-open). Supports per-request model override without mutating global state.

**UnifiedToolService** (in `ares-tools`): Merges static tools, runtime DB tools, and MCP tools behind one interface. Precedence: tenant runtime, fleet runtime, MCP bridge, static.

**ReflectService** (in `cordis`): Coordinates hot-reload. When a file changes or DB row updates, `notify(type_id)` walks dependent fibers and triggers refresh.

## Server bootstrap

`src/main.rs` creates a root context, registers services via `plugin()` and `provide()` calls, then builds the Axum router:

```rust
let root_ctx = Context::new_root();
// Register plugins (services with lifecycle)
root_ctx.plugin(ConfigService).await?;
root_ctx.plugin(CalculatorService).await?;
// ...
// Provide data services
root_ctx.provide_arc(agent_registry.clone());
root_ctx.provide_arc(llm_factory.clone());
root_ctx.provide(AgentResolverService::new(tenant_db, registry, config));
root_ctx.provide(AgentExecutionService::new()
    .with_db(db).with_tenant_db(tdb).with_agent_registry(reg)
    .with_fleet_secrets(secrets).with_run_tracker(active_runs));
// Build router
let app = build_router(root_ctx.clone());
```

## Hot-reload

File changes are detected via `notify` crate with 500ms debounce. When a watched file changes:
1. Watcher calls `ReflectService::notify(type_id)`
2. ReflectService walks dependent fibers via BFS
3. Each fiber recomputes its epoch from dependency versions
4. If epoch changed, fiber reloads with new config

No polling loops. No 60-second stale windows.

## Adding a new tool

1. Implement the `Tool` trait in `crates/ares-tools/src/tools/`
2. Register in `tool_registry.register(Arc::new(MyTool))` in main.rs
3. Optionally implement `Service` and register via `ctx.plugin(MyToolService)`

## Adding a new LLM provider

1. Implement `LLMClient` trait in `crates/ares-llm/src/`
2. Add to provider registry in config
3. The circuit breaker wraps it automatically

## Build

```bash
# Development (all features)
cargo build --features openai,postgres,mcp

# Minimal (no external deps)
cargo build --no-default-features

# Release
cargo build --release --features openai,postgres,mcp
```

Rust 1.98 required (`rust-toolchain.toml` pins it).
