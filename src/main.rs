//! A.R.E.S Server Binary
//!
//! This is the main entry point for running A.R.E.S as a standalone server.
//! For library usage, import from the `ares` crate instead.
//!
//! ## Usage
//!
//! ```bash
//! # Initialize a new project
//! ares-server init
//!
//! # Start the server (requires ares.toml)
//! ares-server
//!
//! # Use a custom config file
//! ares-server --config my-config.toml
//! ```

use ares::{
    api,
    auth::jwt::AuthService,
    cli::{init, output::Output, AgentCommands, Cli, Commands},
    db::TursoClient,
    utils::toml_config::AresConfig,
    AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager,
    ProviderRegistry, ToolRegistry,
};
use axum::{routing::get, Router};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(feature = "swagger-ui")]
use utoipa::OpenApi;
#[cfg(feature = "swagger-ui")]
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse CLI arguments
    let cli = Cli::parse_args();

    // Create output helper based on --no-color flag
    let output = if cli.no_color {
        Output::no_color()
    } else {
        Output::new()
    };

    // Handle subcommands
    match cli.command {
        Some(Commands::Init {
            path,
            force,
            minimal,
            no_examples,
            provider,
            host,
            port,
        }) => {
            let config = init::InitConfig {
                path,
                force,
                minimal,
                no_examples,
                provider,
                host,
                port,
            };

            match init::run(config, &output) {
                init::InitResult::Success => std::process::exit(0),
                init::InitResult::AlreadyExists => std::process::exit(1),
                init::InitResult::Error(_) => std::process::exit(1),
            }
        }

        Some(Commands::Config { full, validate }) => {
            handle_config_command(&cli.config, full, validate, &output)?;
            return Ok(());
        }

        Some(Commands::Agent(agent_cmd)) => {
            handle_agent_command(&cli.config, agent_cmd, &output)?;
            return Ok(());
        }

        None => {
            // No subcommand - run the server
            run_server(&cli.config, cli.verbose).await?;
        }
    }

    Ok(())
}

/// Handle the config subcommand
fn handle_config_command(
    config_path: &std::path::Path,
    full: bool,
    validate: bool,
    output: &Output,
) -> Result<(), Box<dyn std::error::Error>> {
    output.banner();

    if !config_path.exists() {
        output.error(&format!(
            "Configuration file '{}' not found!",
            config_path.display()
        ));
        output.hint("Run 'ares-server init' to create a new configuration");
        return Err("Config not found".into());
    }

    // Use load_unchecked since we don't need env vars for displaying info
    let config = AresConfig::load_unchecked(config_path)?;

    if validate {
        output.success("Configuration is valid!");
        output.newline();
    }

    output.header("Configuration Summary");
    output.newline();

    output.kv("Config file", config_path.to_str().unwrap_or("ares.toml"));
    output.kv(
        "Server",
        &format!("{}:{}", config.server.host, config.server.port),
    );
    output.kv("Log level", &config.server.log_level);
    output.newline();

    output.subheader("Providers");
    for provider_name in config.providers.keys() {
        output.list_item(provider_name);
    }

    output.subheader("Models");
    for model_name in config.models.keys() {
        output.list_item(model_name);
    }

    output.subheader("Agents");
    for agent_name in config.agents.keys() {
        output.list_item(agent_name);
    }

    output.subheader("Tools");
    for tool_name in config.enabled_tools() {
        output.list_item(tool_name);
    }

    if full {
        output.subheader("Workflows");
        for workflow_name in config.workflows.keys() {
            output.list_item(workflow_name);
        }
    }

    Ok(())
}

/// Handle the agent subcommand
fn handle_agent_command(
    config_path: &std::path::Path,
    cmd: AgentCommands,
    output: &Output,
) -> Result<(), Box<dyn std::error::Error>> {
    output.banner();

    if !config_path.exists() {
        output.error(&format!(
            "Configuration file '{}' not found!",
            config_path.display()
        ));
        output.hint("Run 'ares-server init' to create a new configuration");
        return Err("Config not found".into());
    }

    // Use load_unchecked since we don't need env vars for displaying info
    let config = AresConfig::load_unchecked(config_path)?;

    match cmd {
        AgentCommands::List => {
            output.header("Configured Agents");
            output.newline();
            output.table_header(&["Name", "Model", "Tools"]);

            for (name, agent) in &config.agents {
                let tools = agent.tools.join(", ");
                let tools_display = if tools.is_empty() { "-" } else { &tools };
                output.table_row(&[name, &agent.model, tools_display]);
            }
        }

        AgentCommands::Show { name } => {
            if let Some(agent) = config.agents.get(&name) {
                output.header(&format!("Agent: {}", name));
                output.newline();
                output.kv("Model", &agent.model);
                output.kv(
                    "Max tool iterations",
                    &agent.max_tool_iterations.to_string(),
                );
                output.kv("Parallel tools", &agent.parallel_tools.to_string());

                if !agent.tools.is_empty() {
                    output.subheader("Tools");
                    for tool in &agent.tools {
                        output.list_item(tool);
                    }
                }

                output.subheader("System Prompt");
                if let Some(prompt) = &agent.system_prompt {
                    println!("{}", prompt);
                } else {
                    println!("(no custom system prompt)");
                }
            } else {
                output.error(&format!("Agent '{}' not found", name));
                output.hint("Use 'ares-server agent list' to see available agents");
            }
        }
    }

    Ok(())
}

/// Run the A.R.E.S server
async fn run_server(
    config_path: &std::path::Path,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file for secrets (JWT_SECRET, API_KEY, etc.)
    dotenvy::dotenv().ok();

    // Initialize tracing
    let log_filter = if verbose { "debug,ares=trace" } else { "info" };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_filter.into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting A.R.E.S - Agentic Retrieval Enhanced Server");

    // =================================================================
    // Load TOML Configuration
    // =================================================================
    if !config_path.exists() {
        let output = Output::new();
        output.banner();
        output.error(&format!(
            "Configuration file '{}' not found!",
            config_path.display()
        ));
        output.newline();
        output.info("A.R.E.S requires a configuration file to run.");
        output.info("You can create one by running:");
        output.newline();
        output.command("ares-server init");
        output.newline();
        output.hint("This will create ares.toml and all necessary configuration files");

        std::process::exit(1);
    }

    let config_path_str = config_path.to_str().unwrap_or("ares.toml");
    let mut config_manager = AresConfigManager::new(config_path_str)
        .expect("Failed to load configuration - check for syntax errors");

    // Start hot-reload watcher
    config_manager
        .start_watching()
        .expect("Failed to start config file watcher");

    let config_manager = Arc::new(config_manager);
    let config = config_manager.config();

    tracing::info!(
        "Configuration loaded from {} (hot-reload enabled)",
        config_path_str
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
    let turso = if let (Some(turso_url_env), Some(turso_token_env)) = (
        &config.database.turso_url_env,
        &config.database.turso_token_env,
    ) {
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
            tracing::warn!(
                "Failed to initialize dynamic config manager: {}. Using empty config.",
                e
            );
            Arc::new(
                DynamicConfigManager::new(
                    std::path::PathBuf::from(&config.config.agents_dir),
                    std::path::PathBuf::from(&config.config.models_dir),
                    std::path::PathBuf::from(&config.config.tools_dir),
                    std::path::PathBuf::from(&config.config.workflows_dir),
                    std::path::PathBuf::from(&config.config.mcps_dir),
                    false,
                )
                .unwrap_or_else(|_| panic!("Cannot create even empty DynamicConfigManager")),
            )
        }
    };

    // =================================================================
    // Initialize Agent Registry (with TOON support)
    // =================================================================
    let agent_registry = AgentRegistry::with_dynamic_config(
        &config,
        Arc::clone(&provider_registry),
        Arc::clone(&tool_registry),
        Arc::clone(&dynamic_config),
    );
    let agent_registry = Arc::new(agent_registry);
    tracing::info!(
        "Agent registry initialized with {} agents (TOML + TOON)",
        agent_registry.agent_names().len()
    );

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
    // Build OpenAPI Documentation (only when swagger-ui is enabled)
    // =================================================================
    // Version with RAG endpoints (requires both local-embeddings and ares-vector)
    #[cfg(all(
        feature = "swagger-ui",
        feature = "local-embeddings",
        feature = "ares-vector"
    ))]
    #[derive(OpenApi)]
    #[openapi(
        paths(
            // Auth endpoints
            ares::api::handlers::auth::register,
            ares::api::handlers::auth::login,
            ares::api::handlers::auth::logout,
            ares::api::handlers::auth::refresh_token,
            // Chat endpoints
            ares::api::handlers::chat::chat,
            ares::api::handlers::chat::chat_stream,
            ares::api::handlers::chat::get_user_memory,
            // Research endpoints
            ares::api::handlers::research::deep_research,
            // Conversation endpoints
            ares::api::handlers::conversations::list_conversations,
            ares::api::handlers::conversations::get_conversation,
            ares::api::handlers::conversations::update_conversation,
            ares::api::handlers::conversations::delete_conversation,
            // RAG endpoints
            ares::api::handlers::rag::ingest,
            ares::api::handlers::rag::search,
            ares::api::handlers::rag::delete_collection,
            ares::api::handlers::rag::list_collections,
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
            ares::api::handlers::auth::RefreshTokenRequest,
            ares::api::handlers::auth::LogoutRequest,
            ares::api::handlers::auth::LogoutResponse,
            ares::api::handlers::conversations::ConversationSummary,
            ares::api::handlers::conversations::ConversationDetails,
            ares::api::handlers::conversations::ConversationMessage,
            ares::api::handlers::conversations::UpdateConversationRequest,
        )),
        tags(
            (name = "auth", description = "Authentication endpoints"),
            (name = "chat", description = "Chat endpoints"),
            (name = "research", description = "Research endpoints"),
            (name = "conversations", description = "Conversation management endpoints"),
            (name = "rag", description = "RAG (Retrieval Augmented Generation) endpoints"),
        ),
        info(
            title = "A.R.E.S - Agentic Retrieval Enhanced Server API",
            version = "0.3.0",
            description = "Production-grade agentic chatbot server with multi-provider LLM support"
        )
    )]
    struct ApiDoc;

    // Version without RAG endpoints (when local-embeddings is not available)
    #[cfg(all(
        feature = "swagger-ui",
        not(all(feature = "local-embeddings", feature = "ares-vector"))
    ))]
    #[derive(OpenApi)]
    #[openapi(
        paths(
            // Auth endpoints
            ares::api::handlers::auth::register,
            ares::api::handlers::auth::login,
            ares::api::handlers::auth::logout,
            ares::api::handlers::auth::refresh_token,
            // Chat endpoints
            ares::api::handlers::chat::chat,
            ares::api::handlers::chat::chat_stream,
            ares::api::handlers::chat::get_user_memory,
            // Research endpoints
            ares::api::handlers::research::deep_research,
            // Conversation endpoints
            ares::api::handlers::conversations::list_conversations,
            ares::api::handlers::conversations::get_conversation,
            ares::api::handlers::conversations::update_conversation,
            ares::api::handlers::conversations::delete_conversation,
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
            ares::api::handlers::auth::RefreshTokenRequest,
            ares::api::handlers::auth::LogoutRequest,
            ares::api::handlers::auth::LogoutResponse,
            ares::api::handlers::conversations::ConversationSummary,
            ares::api::handlers::conversations::ConversationDetails,
            ares::api::handlers::conversations::ConversationMessage,
            ares::api::handlers::conversations::UpdateConversationRequest,
        )),
        tags(
            (name = "auth", description = "Authentication endpoints"),
            (name = "chat", description = "Chat endpoints"),
            (name = "research", description = "Research endpoints"),
            (name = "conversations", description = "Conversation management endpoints"),
        ),
        info(
            title = "A.R.E.S - Agentic Retrieval Enhanced Server API",
            version = "0.3.0",
            description = "Production-grade agentic chatbot server with multi-provider LLM support"
        )
    )]
    struct ApiDoc;

    // =================================================================
    // Build Router
    // =================================================================
    #[allow(unused_mut)]
    let mut app = Router::new()
        // Health check (simple - returns "OK")
        .route("/health", get(health_check))
        // Detailed health check with component status
        .route("/health/detailed", get(health_check_detailed))
        // Configuration info endpoint
        .route("/config/info", get(config_info))
        // API routes
        .nest(
            "/api",
            api::routes::create_router(state.auth_service.clone()),
        );

    // Swagger UI (optional - requires network during build)
    #[cfg(feature = "swagger-ui")]
    {
        app = app
            .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));
        tracing::info!("Swagger UI enabled - available at /swagger-ui");
    }

    // =================================================================
    // Add UI routes if the `ui` feature is enabled
    // =================================================================
    #[cfg(feature = "ui")]
    {
        app = app.nest("", ui_routes());
        tracing::info!("UI enabled - available at /");
    }

    // =================================================================
    // Add Middleware
    // =================================================================
    // Build CORS layer from configuration
    let cors = build_cors_layer(&config.server.cors_origins);

    // Build rate limiting layer if enabled (per-IP rate limiting using tower_governor)
    let app = if config.server.rate_limit_per_second > 0 {
        use std::sync::Arc;
        use std::time::Duration;
        use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

        // Configure per-IP rate limiting
        let governor_conf = Arc::new(
            GovernorConfigBuilder::default()
                .per_second(config.server.rate_limit_per_second as u64)
                .burst_size(config.server.rate_limit_burst)
                .use_headers() // Include x-ratelimit-* headers in responses
                .finish()
                .expect("Failed to build rate limiter configuration"),
        );

        // Clone the limiter for background cleanup task
        let governor_limiter = governor_conf.limiter().clone();
        let cleanup_interval = Duration::from_secs(60);

        // Background task to periodically clean up old rate limiting entries
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);
            loop {
                interval.tick().await;
                tracing::debug!(
                    "Rate limiter storage size: {}, cleaning up old entries",
                    governor_limiter.len()
                );
                governor_limiter.retain_recent();
            }
        });

        tracing::info!(
            "Rate limiting enabled: {} req/sec per IP with burst of {}",
            config.server.rate_limit_per_second,
            config.server.rate_limit_burst
        );

        app.layer(GovernorLayer::new(governor_conf))
            .layer(cors)
            .layer(TraceLayer::new_for_http())
            .with_state(state)
    } else {
        tracing::warn!("Rate limiting is disabled - not recommended for production");
        app.layer(cors)
            .layer(TraceLayer::new_for_http())
            .with_state(state)
    };

    // =================================================================
    // Start Server
    // =================================================================
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("Server running on http://{}", addr);
    tracing::info!("Swagger UI available at http://{}/swagger-ui/", addr);
    #[cfg(feature = "ui")]
    tracing::info!("Web UI available at http://{}/", addr);

    // Use graceful shutdown with signal handling
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());

    server.await?;

    tracing::info!("Server shut down gracefully");
    Ok(())
}

/// Signal handler for graceful shutdown.
/// Listens for Ctrl+C (SIGINT) and SIGTERM on Unix systems.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received Ctrl+C, initiating graceful shutdown...");
        }
        _ = terminate => {
            tracing::info!("Received SIGTERM, initiating graceful shutdown...");
        }
    }
}

/// Initialize local SQLite database
async fn init_local_db(url: &str) -> Result<TursoClient, Box<dyn std::error::Error>> {
    if !url.contains(":memory:") && !url.starts_with("libsql://") && !url.starts_with("https://") {
        let path = url.strip_prefix("file:").unwrap_or(url);
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    tracing::info!(database_url = %url, "Initializing local database");
    Ok(TursoClient::new_local(url).await?)
}

/// Build CORS layer from configuration
fn build_cors_layer(origins: &[String]) -> CorsLayer {
    use axum::http::{header, Method};
    use tower_http::cors::AllowOrigin;

    let (allow_origin, allow_credentials) = if origins.len() == 1 && origins[0] == "*" {
        tracing::warn!(
            "CORS is configured to allow all origins (*) - not recommended for production"
        );
        // Cannot use credentials with wildcard origin
        (AllowOrigin::any(), false)
    } else if origins.is_empty() {
        tracing::warn!("No CORS origins configured, defaulting to allow all");
        (AllowOrigin::any(), false)
    } else {
        tracing::info!("CORS configured for origins: {:?}", origins);
        (
            AllowOrigin::list(origins.iter().filter_map(|o| o.parse().ok())),
            true,
        )
    };

    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::PATCH,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::ORIGIN,
        ])
        .allow_credentials(allow_credentials)
}

/// Health check endpoint
async fn health_check() -> &'static str {
    "OK"
}

/// Detailed health check endpoint with component status
async fn health_check_detailed(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    use std::time::Instant;

    let start = Instant::now();

    // Check database connectivity
    let db_status = match state.turso.operation_conn().await {
        Ok(_) => serde_json::json!({ "status": "healthy" }),
        Err(e) => serde_json::json!({ "status": "unhealthy", "error": e.to_string() }),
    };

    // Get provider info
    let providers: Vec<String> = state
        .config_manager
        .config()
        .providers
        .keys()
        .cloned()
        .collect();

    // Get agent info
    let agents: Vec<String> = state
        .config_manager
        .config()
        .agents
        .keys()
        .cloned()
        .collect();

    let elapsed_ms = start.elapsed().as_millis();

    // Overall status is healthy if database is healthy
    let db_healthy = db_status
        .get("status")
        .and_then(|s| s.as_str())
        .map(|s| s == "healthy")
        .unwrap_or(false);
    let overall_status = if db_healthy { "healthy" } else { "degraded" };

    axum::Json(serde_json::json!({
        "status": overall_status,
        "version": env!("CARGO_PKG_VERSION"),
        "checks": {
            "database": db_status,
        },
        "providers": providers,
        "agents": agents,
        "latency_ms": elapsed_ms,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
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
        "ui_enabled": cfg!(feature = "ui"),
    }))
}

// =============================================================================
// UI Embedding (when `ui` feature is enabled)
// =============================================================================

#[cfg(feature = "ui")]
mod ui {
    use axum::{
        body::Body,
        http::{header, StatusCode, Uri},
        response::Response,
        routing::get,
        Router,
    };
    use rust_embed::Embed;

    use crate::AppState;

    #[derive(Embed)]
    #[folder = "ui/dist/"]
    struct UiAssets;

    pub fn routes() -> Router<AppState> {
        Router::new()
            .route("/", get(index_handler))
            .route("/*path", get(static_handler))
    }

    async fn index_handler() -> Response {
        serve_file("index.html")
    }

    async fn static_handler(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');

        // Try to serve the exact file
        if let Some(asset) = UiAssets::get(path) {
            return build_response(path, &asset.data);
        }

        // For SPA routing, return index.html for non-asset paths
        if !path.contains('.') {
            if let Some(asset) = UiAssets::get("index.html") {
                return build_response("index.html", &asset.data);
            }
        }

        // Return 404 for truly missing files
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .unwrap()
    }

    fn serve_file(path: &str) -> Response {
        match UiAssets::get(path) {
            Some(asset) => build_response(path, &asset.data),
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Not Found"))
                .unwrap(),
        }
    }

    fn build_response(path: &str, data: &[u8]) -> Response {
        let mime = mime_guess::from_path(path).first_or_octet_stream();

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(Body::from(data.to_vec()))
            .unwrap()
    }
}

#[cfg(feature = "ui")]
fn ui_routes() -> Router<AppState> {
    ui::routes()
}
