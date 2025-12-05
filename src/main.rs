use ares::{
    AppState,
    auth::jwt::AuthService,
    db::{QdrantClient, TursoClient},
    llm::{LLMClientFactory, Provider},
    utils::config::Config,
};
use ares::{api, types};
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

    if config.is_local_mode() {
        tracing::info!("Running in LOCAL mode - no external services required");
    } else {
        tracing::info!("Running in REMOTE mode - connecting to external services");
    }

    // Ensure data directory exists for local mode
    if config.is_local_mode()
        && !config.database.turso_url.starts_with(":memory:")
        && let Some(parent) = std::path::Path::new(&config.database.turso_url).parent()
    {
        std::fs::create_dir_all(parent)?;
    }

    // Initialize database clients
    let turso = TursoClient::new(
        config.database.turso_url.clone(),
        config.database.turso_auth_token.clone(),
    )
    .await?;
    tracing::info!(
        "Database client initialized (mode: {})",
        if config.is_local_mode() {
            "local"
        } else {
            "remote"
        }
    );

    // Initialize vector store (local in-memory by default)
    let qdrant = QdrantClient::new(
        config.database.qdrant_url.clone(),
        config.database.qdrant_api_key.clone(),
    )
    .await?;
    tracing::info!("Vector store initialized (local in-memory mode)");

    // Initialize LLM factory with default provider
    let default_provider = determine_llm_provider(&config);
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

/// Determine the best LLM provider based on configuration
fn determine_llm_provider(config: &Config) -> Provider {
    // Check for explicit provider preference
    if let Some(provider_name) = &config.llm.default_provider {
        match provider_name.to_lowercase().as_str() {
            "openai" => {
                if let Some(api_key) = &config.llm.openai_api_key {
                    return Provider::OpenAI {
                        api_base: config
                            .llm
                            .openai_api_base
                            .clone()
                            .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
                        api_key: api_key.clone(),
                        model: config
                            .llm
                            .default_model
                            .clone()
                            .unwrap_or_else(|| "gpt-4o-mini".to_string()),
                    };
                }
            }
            "ollama" => {
                return Provider::Ollama {
                    base_url: config.llm.ollama_url.clone(),
                    model: config
                        .llm
                        .default_model
                        .clone()
                        .unwrap_or_else(|| "llama3.2".to_string()),
                };
            }
            "llamacpp" => {
                return Provider::LlamaCpp {
                    model_path: config
                        .llm
                        .default_model
                        .clone()
                        .unwrap_or_else(|| "./models/llama.gguf".to_string()),
                };
            }
            _ => {}
        }
    }

    // Auto-detect based on available credentials
    if let Some(api_key) = &config.llm.openai_api_key {
        Provider::OpenAI {
            api_base: config
                .llm
                .openai_api_base
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            api_key: api_key.clone(),
            model: config
                .llm
                .default_model
                .clone()
                .unwrap_or_else(|| "gpt-4o-mini".to_string()),
        }
    } else {
        // Default to Ollama for local-first operation
        Provider::Ollama {
            base_url: config.llm.ollama_url.clone(),
            model: config
                .llm
                .default_model
                .clone()
                .unwrap_or_else(|| "llama3.2".to_string()),
        }
    }
}

async fn health_check() -> &'static str {
    "OK"
}
