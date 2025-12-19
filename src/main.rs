//! A.R.E.S Server Binary
//!
//! This is the main entry point for running A.R.E.S as a standalone server.
//! For library usage, import from the `ares` crate instead.

use ares::{
    AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager,
    ProviderRegistry, ToolRegistry, api, auth::jwt::AuthService, db::TursoClient,
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

const CONFIG_FILE: &str = "ares.toml";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file for secrets (JWT_SECRET, API_KEY, etc.)
    dotenv::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting A.R.E.S - Agentic Retrieval Enhanced Server");

    // =================================================================
    // Load TOML Configuration
    // =================================================================
    // The server REQUIRES ares.toml to exist. Panic if it doesn't.
    if !std::path::Path::new(CONFIG_FILE).exists() {
        panic!(
            "Configuration file '{}' not found!\n\
             A.R.E.S requires ares.toml to run.\n\
             Copy ares.example.toml to ares.toml and customize it.",
            CONFIG_FILE
        );
    }

    let mut config_manager = AresConfigManager::new(CONFIG_FILE)
        .expect("Failed to load ares.toml - check for syntax errors");

    // Start hot-reload watcher
    config_manager
        .start_watching()
        .expect("Failed to start config file watcher");

    let config_manager = Arc::new(config_manager);
    let config = config_manager.config();

    tracing::info!(
        "Configuration loaded from {} (hot-reload enabled)",
        CONFIG_FILE
    );

    // =================================================================
    // Initialize Provider Registry
    // =================================================================
    let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
    tracing::info!(
        "Provider registry initialized with {} providers, {} models",
        config.providers.len(),
        config.models.len()
    );

    // =================================================================
    // Initialize LLM Factory
    // =================================================================
    let llm_factory = Arc::new(
        ConfigBasedLLMFactory::from_config(&config)
            .expect("Failed to create LLM factory from config"),
    );
    tracing::info!(
        "LLM factory initialized with default model: {}",
        llm_factory.default_model()
    );

    // =================================================================
    // Initialize Database
    // =================================================================
    // Check for Turso cloud config first, then fall back to local SQLite
    let turso = if let (Some(turso_url_env), Some(turso_token_env)) = (
        &config.database.turso_url_env,
        &config.database.turso_token_env,
    ) {
        // Try to get cloud credentials from env vars
        if let (Ok(url), Ok(token)) = (std::env::var(turso_url_env), std::env::var(turso_token_env))
        {
            if !url.is_empty() && !token.is_empty() {
                tracing::info!("Initializing Turso (remote) database");
                TursoClient::new_remote(url, token).await?
            } else {
                init_local_db(&config.database.url).await?
            }
        } else {
            init_local_db(&config.database.url).await?
        }
    } else {
        init_local_db(&config.database.url).await?
    };

    tracing::info!("Database client initialized");

    // =================================================================
    // Initialize Auth Service
    // =================================================================
    let jwt_secret = config
        .jwt_secret()
        .expect("JWT_SECRET environment variable must be set");
    let auth_service = AuthService::new(
        jwt_secret,
        config.auth.jwt_access_expiry,
        config.auth.jwt_refresh_expiry,
    );
    tracing::info!("Auth service initialized");

    // =================================================================
    // Initialize Tool Registry
    // =================================================================
    let mut tool_registry = ToolRegistry::with_config(&config);

    // Register built-in tools
    tool_registry.register(Arc::new(ares::tools::calculator::Calculator));
    tool_registry.register(Arc::new(ares::tools::search::WebSearch::new()));

    let tool_registry = Arc::new(tool_registry);
    tracing::info!(
        "Tool registry initialized with {} tools",
        tool_registry.enabled_tool_names().len()
    );

    // =================================================================
    // Initialize Agent Registry
    // =================================================================
    let agent_registry = AgentRegistry::from_config(
        &config,
        Arc::clone(&provider_registry),
        Arc::clone(&tool_registry),
    );
    let agent_registry = Arc::new(agent_registry);
    tracing::info!(
        "Agent registry initialized with {} agents",
        agent_registry.agent_names().len()
    );

    // =================================================================
    // Initialize Dynamic Configuration (TOON)
    // =================================================================
    let dynamic_config = match DynamicConfigManager::from_config(&config) {
        Ok(dm) => {
            tracing::info!(
                "Dynamic config manager initialized with {} agents, {} models, {} tools",
                dm.agents().len(),
                dm.models().len(),
                dm.tools().len()
            );
            Arc::new(dm)
        }
        Err(e) => {
            tracing::warn!("Failed to initialize dynamic config manager: {}. Using empty config.", e);
            // Create an empty manager - directories may not exist yet
            Arc::new(
                DynamicConfigManager::new(
                    std::path::PathBuf::from(&config.config.agents_dir),
                    std::path::PathBuf::from(&config.config.models_dir),
                    std::path::PathBuf::from(&config.config.tools_dir),
                    std::path::PathBuf::from(&config.config.workflows_dir),
                    std::path::PathBuf::from(&config.config.mcps_dir),
                    false, // Don't try hot-reload if initial load failed
                )
                .unwrap_or_else(|_| panic!("Cannot create even empty DynamicConfigManager"))
            )
        }
    };

    // =================================================================
    // Create Application State
    // =================================================================
    let state = AppState {
        config_manager: Arc::clone(&config_manager),
        turso: Arc::new(turso),
        llm_factory,
        provider_registry,
        agent_registry,
        tool_registry,
        auth_service: Arc::new(auth_service),
        dynamic_config,
    };

    // =================================================================
    // Build OpenAPI Documentation
    // =================================================================
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

    // =================================================================
    // Build Router
    // =================================================================
    let app = Router::new()
        // Health check
        .route("/health", get(health_check))
        // Configuration info endpoint
        .route("/config/info", get(config_info))
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

    // =================================================================
    // Start Server
    // =================================================================
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Server running on http://{}", addr);
    tracing::info!("Swagger UI available at http://{}/swagger-ui/", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Initialize local SQLite database
async fn init_local_db(url: &str) -> Result<TursoClient, Box<dyn std::error::Error>> {
    // Ensure data directory exists for the default "./data/ares.db" path.
    if !url.contains(":memory:") && !url.starts_with("libsql://") && !url.starts_with("https://") {
        let path = url.strip_prefix("file:").unwrap_or(url);
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    tracing::info!(database_url = %url, "Initializing local database");
    Ok(TursoClient::new_local(url).await?)
}

/// Health check endpoint
async fn health_check() -> &'static str {
    "OK"
}

/// Configuration info endpoint (non-sensitive info only)
async fn config_info(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let config = state.config_manager.config();
    axum::Json(serde_json::json!({
        "server": {
            "host": config.server.host,
            "port": config.server.port,
            "log_level": config.server.log_level,
        },
        "providers": config.providers.keys().collect::<Vec<_>>(),
        "models": config.models.keys().collect::<Vec<_>>(),
        "agents": config.agents.keys().collect::<Vec<_>>(),
        "tools": config.enabled_tools(),
        "workflows": config.workflows.keys().collect::<Vec<_>>(),
    }))
}
