use ares::{
    AppState, api, auth::jwt::AuthService, db::TursoClient, llm::LLMClientFactory,
    utils::config::Config,
};
use axum::{Router, routing::get};
use std::sync::Arc;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting A.R.E.S - Agentic Retrieval Enhanced Server");

    // Load configuration
    let config = Config::from_env()?;
    tracing::info!("Configuration loaded");

    // Initialize database clients
    // Local-first initialization. If Turso cloud config is provided, it will be preferred.
    // Otherwise we use a local libsql/SQLite file database.
    let turso = if let (Some(url), Some(token)) = (
        config.database.turso_url.clone(),
        config.database.turso_auth_token.clone(),
    ) {
        tracing::info!("Initializing Turso (remote) database");
        TursoClient::new_remote(url, token).await?
    } else {
        // Ensure data directory exists for the default "./data/ares.db" path.
        // (No-op if DATABASE_URL points elsewhere, or is ":memory:")
        if !config.database.database_url.contains(":memory:")
            && !config.database.database_url.starts_with("libsql://")
            && !config.database.database_url.starts_with("https://")
        {
            let path = config
                .database
                .database_url
                .strip_prefix("file:")
                .unwrap_or(&config.database.database_url);
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
        }

        tracing::info!(
            database_url = %config.database.database_url,
            "Initializing local database"
        );
        TursoClient::new_local(&config.database.database_url).await?
    };
    tracing::info!("Database client initialized");

    // Initialize LLM factory from environment
    let llm_factory = LLMClientFactory::from_env()?;
    tracing::info!("LLM client factory initialized");

    // Initialize auth service
    let auth_service = AuthService::new(
        config.auth.jwt_secret.clone(),
        config.auth.jwt_access_expiry,
        config.auth.jwt_refresh_expiry,
    );
    tracing::info!("Auth service initialized");

    // Create application state
    let state = AppState {
        config: Arc::new(config.clone()),
        turso: Arc::new(turso),
        llm_factory: Arc::new(llm_factory),
        auth_service: Arc::new(auth_service),
    };

    // Build OpenAPI documentation
    #[derive(OpenApi)]
    #[openapi(
        paths(
            ares::api::handlers::auth::register,
            ares::api::handlers::auth::login,
            ares::api::handlers::chat::chat,
            ares::api::handlers::research::deep_research,
        ),
        components(schemas(
            ares::types::ChatRequest,
            ares::types::ChatResponse,
            ares::types::ResearchRequest,
            ares::types::ResearchResponse,
            ares::types::LoginRequest,
            ares::types::RegisterRequest,
            ares::types::TokenResponse,
            ares::types::AgentType,
            ares::types::Source,
        )),
        tags(
              (name = "auth", description = "Authentication endpoints"),
              (name = "chat", description = "Chat endpoints"),
              (name = "research", description = "Research endpoints"),
          ),
        info(
            title = "A.R.E.S - Agentic Retrieval Enhanced Server API",
            version = "0.1.0",
            description = "Production-grade agentic chatbot server with multi-provider LLM support"
        )
    )]
    struct ApiDoc;

    // Build router
    let app = Router::new()
        // Health check
        .route("/health", get(health_check))
        // API routes
        .nest(
            "/api",
            api::routes::create_router(state.auth_service.clone()),
        )
        // Swagger UI
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        // Add middleware
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        // Add state
        .with_state(state);

    // Start server
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Server running on http://{}", addr);
    tracing::info!("Swagger UI available at http://{}/swagger-ui/", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> &'static str {
    "OK"
}
