# Building on the ARES router

The `Http` plugin in `ares_http` serves the generic endpoints through an Axum router. Extension crates merge additional routes on top of this router to build managed platforms.

## Pattern

The server builds its HTTP surface in `run_server`: it takes the `Http` service from the Cordis `Context` and merges extra binary routes on top:

```rust
// Http plugin owns `/health` and `/api`. Extra binary routes merge on top.
let http = state.get::<ares_http::Http>().ok_or(
    "Http plugin not instantiated; add [[entry]] plugin=\"Http\" to config/cordis-entries.toml",
)?;
let extra = axum::Router::new()
    .route("/health/detailed", get(health_check_detailed))
    .with_state(state.clone());
let mut app = http.router.clone().merge(extra);
```

Extension crates call `app.merge(...)` with their own routes in their own `main.rs`.

## Included route groups

| Route Group | Endpoints |
|-------------|-----------|
| Auth | `/api/auth/register`, `/api/auth/login`, `/api/auth/refresh`, `/api/auth/logout` |
| Chat | `/api/chat`, `/api/chat/stream` |
| Agents | `/api/agents` |
| Research | `/api/research` |
| Workflows | `/api/workflows`, `/api/workflows/{name}` |
| User Agents | `/api/user/agents/*` |
| Conversations | `/api/conversations/*` |
| Admin | `/api/admin/tenants/*`, `/api/admin/agents/*`, `/api/admin/deploy/*` |
| V1 (API Key) | `/api/v1/chat`, `/api/v1/agents/*`, `/api/v1/usage` |
| RAG | `/api/rag/ingest`, `/api/rag/search` (requires `local-embeddings` + `ares-vector` features) |

## Registering custom tools

The `ares_tools::Tools` service is the public registration point for tools. Handlers resolve tools through `ctx.get::<Tools>()`. The `Tools` loader factory builds its registry from your tool configurations:

```rust
let mut tool_registry = ToolRegistry::with_config(&config.tools);

// Built-in tools
tool_registry.register(Arc::new(CalculatorService));

// Your custom tools
tool_registry.register(Arc::new(MyCustomTool::new()));
```

Provide custom registries through the `Tools` factory in `crates/ares-tools/src/plugins.rs`.

## Adding middleware

Apply Axum layers to the merged router:

```rust
let app = http.router.clone().merge(extra)
    .layer(my_auth_middleware())
    .layer(my_logging_middleware());
```
