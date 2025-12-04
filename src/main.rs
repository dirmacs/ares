mod agents;
mod api;
mod auth;
mod db;
mod llm;
mod mcp;
mod memory;
mod rag;
mod research;
mod tools;
mod types;
mod utils;

use crate::{
    auth::jwt::AuthService,
    db::{QdrantClient, TursoClient},
    llm::{LLMClientFactory, Provider},
    utils::config::Config,
};
use axum::{
    Router,
    routing::{get, post},
};
use std::sync::Arc;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub turso: Arc<TursoClient>,
    pub qdrant: Arc<QdrantClient>,
    pub llm_factory: Arc<LLMClientFactory>,
    pub auth_service: Arc<AuthService>,
}

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
    let turso = TursoClient::new(
        config.database.turso_url.clone(),
        config.database.turso_auth_token.clone(),
    )
    .await?;
    tracing::info!("Turso client initialized");

    let qdrant = QdrantClient::new(
        config.database.qdrant_url.clone(),
        config.database.qdrant_api_key.clone(),
    )
    .await?;
    tracing::info!("Qdrant client initialized");

    // Initialize LLM factory with default provider
    let default_provider = if let Some(api_key) = &config.llm.openai_api_key {
        Provider::OpenAI {
            api_base: "https://integrate.api.nvidia.com".to_string(),
            api_key: api_key.clone(),
            model: "mistralai/mistral-large-3-675b-instruct-2512".to_string(),
        }
    } else {
        Provider::Ollama {
            base_url: config.llm.ollama_url.clone(),
            model: "qwen3-vl:2b".to_string(),
        }
    };

    let llm_factory = LLMClientFactory::new(default_provider);
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
        qdrant: Arc::new(qdrant),
        llm_factory: Arc::new(llm_factory),
        auth_service: Arc::new(auth_service),
    };

    // Build OpenAPI documentation
    #[derive(OpenApi)]
    #[openapi(
        paths(
            api::handlers::auth::register,
            api::handlers::auth::login,
            api::handlers::chat::chat,
            api::handlers::research::deep_research,
        ),
        components(schemas(
            types::ChatRequest,
            types::ChatResponse,
            types::ResearchRequest,
            types::ResearchResponse,
            types::LoginRequest,
            types::RegisterRequest,
            types::TokenResponse,
            types::AgentType,
            types::Source,
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
        .nest("/api", api::routes::create_router())
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
