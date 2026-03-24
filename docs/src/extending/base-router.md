# Building on base_router()

ARES exports `base_router(state)` which returns a fully configured Axum router with all generic endpoints. Extension crates can build managed platforms by merging additional routes on top.

## Pattern

```rust
use ares::{base_router, AppState};
use axum::{routing::post, Router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build AppState (config, DB, LLM, tools, agents)
    let state = build_my_state().await?;

    // Start with all ARES generic routes
    let app = Router::new()
        .route("/health", axum::routing::get(|| async { "OK" }))
        .nest("/api", ares::api::routes::create_router(
            state.auth_service.clone(),
            state.tenant_db.clone(),
        ))
        // Add your own routes
        .nest("/v1/my-feature", my_routes())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

## What base_router() Includes

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

## Registering Custom Tools

```rust
let mut tool_registry = ToolRegistry::with_config(&config);

// Built-in tools
tool_registry.register(Arc::new(ares::tools::calculator::Calculator));

// Your custom tools
tool_registry.register(Arc::new(MyCustomTool::new()));
```

## Adding Middleware

```rust
let app = base_router(state.clone())
    .layer(my_auth_middleware())
    .layer(my_logging_middleware());
```
