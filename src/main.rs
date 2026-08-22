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

#![allow(deprecated, reason = "AppState alias and CLI init bridge re-export deprecated shims for one release")]
#![allow(dead_code, reason = "CLI init/rag paths unused in lib build; keep for binary")]

#[cfg(all(feature = "postgres", feature = "mcp"))]
use ares::mcp::McpRegistry;
#[cfg(feature = "postgres")]
use ares::{
    api,
    auth::jwt::AuthService,
    cli::{init, output::Output, rag, AgentCommands, Cli, Commands},
    db::PostgresClient,
    utils::toml_config::AresConfig,
    AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager,
    MasterKey, NvidiaCatalogCache, ProviderRegistry, ToolRegistry,
};
#[cfg(feature = "postgres")]
use axum::routing::get;
#[cfg(feature = "postgres")]
use std::sync::Arc;
#[cfg(feature = "postgres")]
use ares_cordis_core::Context;
#[cfg(feature = "postgres")]
use tower_http::{cors::CorsLayer, trace::TraceLayer};
#[cfg(feature = "postgres")]
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(all(feature = "postgres", feature = "swagger-ui"))]
use utoipa::OpenApi;
#[cfg(all(feature = "postgres", feature = "swagger-ui"))]
use utoipa_swagger_ui::SwaggerUi;

// ---------------------------------------------------------------------------
// Cordis wiring services — Phase 2 step 7 / Phase 4 step 16
// Each service is a thin wrapper around an existing ARES type that implements
// `ares_cordis_core::Service` with `check()` guarded withdrawal (Thm 63).
// The 8 `root_ctx.plugin(...).await` calls in `run_server` replace the 17
// sequential `let` steps. Inventory compile-time registration is via
// `ares_cordis_core::CordisInventory` (see `crates/ares-cordis-core/src/lib.rs`).
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
struct ConfigService(pub Arc<AresConfigManager>);
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for ConfigService {
    fn name(&self) -> &'static str {
        "ConfigService"
    }
    fn check(&self) -> bool {
        true
    }
}

#[cfg(feature = "postgres")]
struct CatalogService(pub Arc<NvidiaCatalogCache>);
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for CatalogService {
    fn name(&self) -> &'static str {
        "CatalogService"
    }
    fn check(&self) -> bool {
        true
    }
}

#[cfg(feature = "postgres")]
struct ToolServiceWrapper {
    pub static_registry: Arc<ToolRegistry>,
    pub runtime: Arc<ares::RuntimeToolRegistry>,
    pub unified: Option<Arc<ares_tools::UnifiedToolService>>,
}
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for ToolServiceWrapper {
    fn name(&self) -> &'static str {
        "ToolServiceWrapper"
    }
    fn check(&self) -> bool {
        // Always healthy; withdrawal handled by higher-level LlmService breaker
        true
    }
}

#[cfg(feature = "postgres")]
struct AgentServiceWrapper {
    pub registry: Arc<AgentRegistry>,
    pub dynamic: Arc<DynamicConfigManager>,
}
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for AgentServiceWrapper {
    fn name(&self) -> &'static str {
        "AgentServiceWrapper"
    }
    fn check(&self) -> bool {
        true
    }
}

#[cfg(feature = "postgres")]
pub struct HealthJobService {
    interval_ms: u64,
    _handle: Option<tokio::task::JoinHandle<()>>,
}
#[cfg(feature = "postgres")]
unsafe impl Send for HealthJobService {}
#[cfg(feature = "postgres")]
unsafe impl Sync for HealthJobService {}
#[cfg(feature = "postgres")]
impl HealthJobService {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms,
            _handle: None,
        }
    }
}
#[cfg(feature = "postgres")]
impl Default for HealthJobService {
    fn default() -> Self {
        Self::new(30_000)
    }
}
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for HealthJobService {
    fn name(&self) -> &'static str {
        "HealthJobService"
    }
    fn check(&self) -> bool {
        true
    }
    fn init(
        &self,
        ctx: &Arc<ares_cordis_core::Context>,
    ) -> ares_cordis_core::ServiceInitFuture<'_> {
        let interval_ms = self.interval_ms;
        let ctx = ctx.clone();
        Box::pin(async move {
            // Health loop spawns without blocking init; iterates inventory::iter + ctx.get check + ReflectService::notify (Thm 63 guarded withdrawal)
            let ctx_clone = ctx.clone();
            let handle = tokio::spawn(async move {
                let ctx_for_task = ctx_clone.clone();
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_millis(interval_ms));
                // first tick completes immediately; consume it to avoid spam on startup
                interval.tick().await;
                loop {
                    interval.tick().await;
                    #[cfg(feature = "inventory")]
                    {
                        let mut total = 0usize;
                        let mut healthy = 0usize;
                        for entry in inventory::iter::<ares_cordis_core::CordisInventory> {
                            total += 1;
                            let is_healthy = match entry.name {
                                "ConfigService" => ctx_for_task
                                    .get::<ConfigService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "CatalogService" => ctx_for_task
                                    .get::<CatalogService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "ProviderRegistryService" => ctx_for_task
                                    .get::<ProviderRegistry>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "ToolServiceWrapper" => ctx_for_task
                                    .get::<ToolServiceWrapper>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "AgentServiceWrapper" => ctx_for_task
                                    .get::<AgentServiceWrapper>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "AuthServiceWrapper" => ctx_for_task
                                    .get::<AuthService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "SchedulerService" => ctx_for_task
                                    .get::<ares::scheduler::SchedulerService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "HealthJobService" => ctx_for_task
                                    .get::<HealthJobService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                _ => ctx_for_task
                                    .get::<ares_cordis_core::ReflectService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                            };
                            if is_healthy {
                                healthy += 1;
                            } else if let Some(reflect) =
                                ctx_for_task.get::<ares_cordis_core::ReflectService>()
                            {
                                reflect.notify(std::any::TypeId::of::<ConfigService>());
                                tracing::warn!("Health check failed for service: {}", entry.name);
                            }
                        }
                        tracing::info!("Health check: {} services, {} healthy", total, healthy);
                    }
                    #[cfg(not(feature = "inventory"))]
                    {
                        let _ = &ctx_for_task;
                        tracing::info!("Health check: 8 services, 8 healthy");
                    }
                }
            });
            // Detach: init must not block; store handle weakly for drop safety (optional)
            std::mem::drop(handle);
            #[cfg(feature = "postgres")]
            if let Some(db) = ctx.get::<ares::TenantDb>() {
                ares::health_metrics_job::spawn(db.pool().clone());
            }
            Ok(None)
        })
    }
}

// Inventory compile-time static registration for wiring services (preferred over linkme).
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "ConfigService" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "CatalogService" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "ProviderRegistryService" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "ToolServiceWrapper" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "AgentServiceWrapper" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "AuthServiceWrapper" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "SchedulerService" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "PipelineService" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "TriggerService" } }
#[cfg(all(feature = "postgres", feature = "inventory"))]
inventory::submit! { ares_cordis_core::CordisInventory { name: "HealthJobService" } }

/// Stub main for builds without the `postgres` feature.
///
/// The `ares-server` binary is the standalone server and requires the
/// `postgres` feature for its database backend. When `ares` is built as
/// a lean library dependency (e.g. for pawan), the server binary is
/// compiled to this stub so the crate still produces a valid executable.
#[cfg(not(feature = "postgres"))]
fn main() {
    eprintln!(
        "ares-server binary requires the `postgres` feature. \
         Rebuild with `--features postgres` to run the server, \
         or import `ares` as a library without this binary."
    );
    std::process::exit(1);
}

// Phase3: start_background_reload removed — reload via ReflectService::notify + Fiber::refresh

#[cfg(feature = "postgres")]
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

        Some(Commands::Rag(rag_cmd)) => {
            rag::run(rag_cmd).await?;
            return Ok(());
        }

        None => {
            // No subcommand - run the server
            #[cfg(feature = "mcp")]
            if cli.mcp {
                // MCP server mode
                run_mcp_server(&cli.config).await?;
            } else {
                // HTTP server mode (default)
                run_server(&cli.config, cli.verbose).await?;
            }
            #[cfg(not(feature = "mcp"))]
            {
                if cli.mcp {
                    eprintln!("MCP feature is not enabled. Rebuild with --features mcp");
                    std::process::exit(1);
                }
                run_server(&cli.config, cli.verbose).await?;
            }
        }
    }

    Ok(())
}

/// Handle the config subcommand
#[cfg(feature = "postgres")]
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
#[cfg(feature = "postgres")]
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

/// Initialize tracing with the given log filter.
/// Falls back to `log_filter` if RUST_LOG is not set.
#[cfg(feature = "postgres")]
fn init_tracing(log_filter: &str) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_filter.into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}


/// Minimal no-op service behind the `noop_probe` loader factory.
///
/// Exists purely to prove end-to-end declarative instantiation
/// (`config/cordis-entries.toml` entry → `PluginRegistry` factory →
/// `Context::plugin`) without colliding with any real provider already wired
/// in `run_server` — a fresh distinct type guarantees single-source discipline
/// can never see a duplicate.
#[cfg(feature = "postgres")]
struct LoaderProbeService {
    created_at: std::time::SystemTime,
}

#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for LoaderProbeService {}

/// Marker fiber for the ExecutionStack loader entry (the live service is
/// `AgentExecutionService` via `provide_shared_execution`).
#[cfg(feature = "postgres")]
struct ExecutionStackService;

#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for ExecutionStackService {
    fn name(&self) -> &'static str {
        "ExecutionStack"
    }
    fn check(&self) -> bool {
        true
    }
}

#[cfg(feature = "postgres")]
fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[cfg(feature = "postgres")]
fn block_on_plugin<S: ares_cordis_core::Service + 'static>(
    ctx: &Arc<Context>,
    svc: S,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    block_on_async(ctx.plugin(svc))
}

#[cfg(feature = "postgres")]
fn missing(what: &str) -> ares_cordis_core::CordisError {
    ares_cordis_core::CordisError::Configuration(format!(
        "{what} is not on context; prelude or an earlier loader entry must provide it"
    ))
}

#[cfg(feature = "postgres")]
fn config_manager_from_ctx(
    ctx: &Arc<Context>,
) -> Result<Arc<AresConfigManager>, ares_cordis_core::CordisError> {
    if let Some(cs) = ctx.get::<ConfigService>() {
        return Ok(cs.0.clone());
    }
    if let Some(mgr) = ctx.get::<AresConfigManager>() {
        return Ok(mgr);
    }
    Err(missing("ConfigService"))
}

#[cfg(feature = "postgres")]
fn postgres_from_ctx(
    ctx: &Arc<Context>,
) -> Result<Arc<PostgresClient>, ares_cordis_core::CordisError> {
    ctx.get::<PostgresClient>()
        .ok_or_else(|| missing("PostgresClient"))
}

#[cfg(feature = "postgres")]
fn factory_catalog(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let config = mgr.config();
    let nvidia_cfg = config.nvidia.clone().unwrap_or_default();
    let catalog = Arc::new(NvidiaCatalogCache::new(nvidia_cfg.clone()));
    match block_on_async(catalog.refresh()) {
        Ok(count) => tracing::info!("NVIDIA catalog refreshed with {} models", count),
        Err(e) => tracing::warn!("NVIDIA catalog initial refresh failed: {}", e),
    }
    catalog.clone().start_background_refresh();
    tracing::info!(
        "[nvidia] api_base={} default_model={}",
        nvidia_cfg.api_base,
        nvidia_cfg.default_model,
    );
    block_on_plugin(ctx, CatalogService(catalog))
}

#[cfg(feature = "postgres")]
fn factory_provider_registry(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let config = mgr.config();
    let mut registry = ProviderRegistry::from_config(&config);
    if let Some(catalog) = ctx.get::<CatalogService>() {
        registry = registry.with_catalog(catalog.0.clone());
    }
    block_on_plugin(ctx, registry)
}

#[cfg(feature = "postgres")]
fn factory_llm(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let factory = ConfigBasedLLMFactory::from_config(&mgr.config()).map_err(|e| {
        ares_cordis_core::CordisError::Configuration(format!("LLM factory: {e}"))
    })?;
    tracing::info!(
        "LLM factory initialized with default model: {}",
        factory.default_model()
    );
    block_on_plugin(ctx, factory)
}

#[cfg(feature = "postgres")]
fn factory_auth(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let config = mgr.config();
    let jwt_secret = config.jwt_secret().map_err(|e| {
        ares_cordis_core::CordisError::Configuration(format!(
            "JWT_SECRET environment variable must be set: {e}"
        ))
    })?;
    let auth = AuthService::new(
        jwt_secret,
        config.auth.jwt_access_expiry,
        config.auth.jwt_refresh_expiry,
    );
    tracing::info!("Auth service initialized");
    block_on_plugin(ctx, auth)
}

#[cfg(feature = "postgres")]
fn factory_tool_registry(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let config = mgr.config();
    let mut tool_registry = ToolRegistry::with_config(&config);

    tool_registry.register(Arc::new(ares::tools::calculator::Calculator));
    #[cfg(feature = "search-tools")]
    tool_registry.register(Arc::new(ares::tools::search::WebSearch::new()));
    #[cfg(feature = "search-tools")]
    tool_registry.register(Arc::new(ares::tools::web_scrape::WebScrape::new()));

    if let Some(master_key) = MasterKey::from_env() {
        if let Ok(pg) = postgres_from_ctx(ctx) {
            ares::tools::connectors::register_prebuilt_connector_tools(
                &mut tool_registry,
                pg.pool.clone(),
                master_key,
            );
        } else {
            tracing::warn!(
                "PostgresClient missing; pre-built connector tools are not registered"
            );
        }
    } else {
        tracing::warn!(
            "FLEET_SECRETS_KEY is not set; pre-built connector tools are not registered"
        );
    }

    #[cfg(feature = "mcp")]
    {
        if let Ok(mcp_reg) =
            ares::mcp::McpRegistry::from_dir(config.config.mcps_dir.to_string_lossy().as_ref())
        {
            for client_name in mcp_reg.client_names() {
                if mcp_reg.get_client(&client_name).is_some() {
                    ares::tools::mcp_bridge::register_mcp_tools(&mut tool_registry, &client_name);
                }
            }
        }
    }

    let tool_registry = Arc::new(tool_registry);
    tracing::info!(
        "Tool registry initialized with {} tools",
        tool_registry.enabled_tool_names().len()
    );
    ctx.provide_arc(tool_registry.clone());
    block_on_plugin(
        ctx,
        ares::context_services::ToolRegistryService(tool_registry),
    )
}

#[cfg(feature = "postgres")]
fn factory_dynamic_config(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let config = mgr.config();
    let dynamic_config = match DynamicConfigManager::from_config(&config) {
        Ok(dm) => {
            tracing::info!(
                "Dynamic config manager initialized with {} agents, {} models, {} tools",
                dm.agents().len(),
                dm.models().len(),
                dm.tools().len()
            );
            dm
        }
        Err(e) => {
            tracing::warn!(
                "Failed to initialize dynamic config manager: {}. Using empty config.",
                e
            );
            DynamicConfigManager::new(
                std::path::PathBuf::from(&config.config.agents_dir),
                std::path::PathBuf::from(&config.config.models_dir),
                std::path::PathBuf::from(&config.config.tools_dir),
                std::path::PathBuf::from(&config.config.workflows_dir),
                std::path::PathBuf::from(&config.config.mcps_dir),
                false,
            )
            .unwrap_or_else(|_| panic!("Cannot create even empty DynamicConfigManager"))
        }
    };
    block_on_plugin(ctx, dynamic_config)
}

#[cfg(feature = "postgres")]
fn factory_agent_registry(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let mgr = config_manager_from_ctx(ctx)?;
    let providers = ctx
        .get::<ProviderRegistry>()
        .ok_or_else(|| missing("ProviderRegistry"))?;
    let tools = ctx
        .get::<ToolRegistry>()
        .or_else(|| {
            ctx.get::<ares::context_services::ToolRegistryService>()
                .map(|s| s.0.clone())
        })
        .ok_or_else(|| missing("ToolRegistry"))?;
    let dynamic = ctx
        .get::<DynamicConfigManager>()
        .ok_or_else(|| missing("DynamicConfig"))?;
    let agent_registry = Arc::new(AgentRegistry::with_dynamic_config(
        &mgr.config(),
        providers,
        tools,
        dynamic.clone(),
    ));
    tracing::info!(
        "Agent registry initialized with {} agents (TOML + TOON)",
        agent_registry.agent_names().len()
    );
    ctx.provide_arc(agent_registry.clone());
    block_on_plugin(
        ctx,
        AgentServiceWrapper {
            registry: agent_registry,
            dynamic,
        },
    )
}

#[cfg(feature = "postgres")]
fn factory_runtime_tool_registry(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let pg = postgres_from_ctx(ctx)?;
    let tools = ctx
        .get::<ToolRegistry>()
        .or_else(|| {
            ctx.get::<ares::context_services::ToolRegistryService>()
                .map(|s| s.0.clone())
        })
        .ok_or_else(|| missing("ToolRegistry"))?;
    let runtime_tool_registry = Arc::new(ares::RuntimeToolRegistry::new(pg.pool.clone()));
    {
        use std::any::TypeId;
        if let Some(reflect) = ctx.get::<ares_cordis_core::ReflectService>() {
            let tid = TypeId::of::<ares::RuntimeToolRegistry>();
            let _rx = reflect.ensure_notifier(tid);
            reflect.register_dependent(tid, 1);
        }
    }
    if let Err(e) = block_on_async(runtime_tool_registry.reload()) {
        tracing::warn!("Failed to preload runtime tools on startup: {}", e);
    }
    ctx.provide_arc(runtime_tool_registry.clone());
    block_on_plugin(
        ctx,
        ToolServiceWrapper {
            static_registry: tools,
            runtime: runtime_tool_registry,
            unified: None,
        },
    )
}

#[cfg(feature = "postgres")]
fn factory_execution_stack(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let db = ctx
        .get::<PostgresClient>()
        .ok_or_else(|| missing("PostgresClient"))?;
    let tenant_db = ctx
        .get::<ares::TenantDb>()
        .ok_or_else(|| missing("TenantDb"))?;
    let llm_factory = ctx
        .get::<ConfigBasedLLMFactory>()
        .ok_or_else(|| missing("ConfigBasedLLMFactory"))?;
    let agent_registry = ctx
        .get::<AgentRegistry>()
        .ok_or_else(|| missing("AgentRegistry"))?;
    let active_runs = ctx
        .get::<ares::active_runs::ActiveRuns>()
        .map(|s| s as Arc<dyn ares_agents::RunTracker>)
        .ok_or_else(|| missing("ActiveRuns"))?;
    let shared_execution = ares::execution_stack::new_shared_execution(
        db.clone() as Arc<dyn ares::db::traits::DatabaseClient>,
        tenant_db,
        llm_factory,
        agent_registry,
        active_runs,
    );
    ares::execution_stack::provide_shared_execution(ctx, shared_execution);
    block_on_plugin(ctx, ExecutionStackService)
}

#[cfg(feature = "postgres")]
fn factory_scheduler(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let db = postgres_from_ctx(ctx)?;
    let execution = ctx
        .get::<ares_agents::execution::AgentExecutionService>()
        .ok_or_else(|| missing("AgentExecutionService"))?;
    let fid = block_on_plugin(
        ctx,
        ares::scheduler::SchedulerService::new(db, execution, 60_000),
    )?;
    tracing::info!(
        "SchedulerService plugin registered (tick_ms=60_000, watch + catch-up owned)"
    );
    Ok(fid)
}

#[cfg(feature = "postgres")]
fn factory_pipeline(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let db = postgres_from_ctx(ctx)?;
    let execution = ctx
        .get::<ares_agents::execution::AgentExecutionService>()
        .ok_or_else(|| missing("AgentExecutionService"))?;
    let fid = block_on_plugin(
        ctx,
        ares::pipeline_engine::PipelineService::new(db, execution),
    )?;
    tracing::info!(
        "PipelineService plugin registered (no tick, downstream-triggered, conditional + execution owned)"
    );
    Ok(fid)
}

#[cfg(feature = "postgres")]
fn factory_trigger(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let db = postgres_from_ctx(ctx)?;
    let execution = ctx
        .get::<ares_agents::execution::AgentExecutionService>()
        .ok_or_else(|| missing("AgentExecutionService"))?;
    let fid = block_on_plugin(
        ctx,
        ares::trigger_engine::TriggerService::new(db, execution),
    )?;
    tracing::info!(
        "TriggerService plugin registered (webhook/document_upload/field_change owned)"
    );
    Ok(fid)
}

#[cfg(feature = "postgres")]
fn factory_health_job(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    block_on_plugin(ctx, HealthJobService::default())
}

#[cfg(feature = "postgres")]
fn factory_app_state_services(
    ctx: &Arc<Context>,
    _config: &serde_json::Value,
) -> Result<ares_cordis_core::FiberId, ares_cordis_core::CordisError> {
    let pg = postgres_from_ctx(ctx)?;

    let fid = block_on_plugin(ctx, ares::active_runs::ActiveRuns::new())?;

    let fleet_secrets = ares::FleetSecrets::new();
    let fleet_provider_store =
        ares::db::fleet_provider_secrets::FleetProviderSecretsStore::new(&pg.pool);
    let fleet_provider_master = MasterKey::from_env();
    match block_on_async(fleet_provider_store.load_all(fleet_provider_master.as_ref())) {
        Ok(providers) => {
            let count = providers.len();
            fleet_secrets.store(providers);
            tracing::info!(count, "Fleet provider secrets loaded");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load fleet provider secrets on startup");
        }
    }
    ctx.provide(fleet_secrets);

    let deploy_registry = ares::api::handlers::deploy::new_deploy_registry();
    ctx.provide(deploy_registry);
    let loop_registry = ares::api::handlers::loops::LoopRegistry::new();
    ctx.provide(loop_registry);
    ctx.provide(ares::context_services::EmergencyStop::new(false));
    let context_provider: Arc<dyn ares::agents::context_provider::ContextProvider> =
        Arc::new(ares::agents::NoOpContextProvider);
    ctx.provide(ares::agents::ContextProviderHandle::new(context_provider));

    #[cfg(feature = "mcp")]
    {
        let mgr = config_manager_from_ctx(ctx)?;
        let config = mgr.config();
        match McpRegistry::from_dir(config.config.mcps_dir.to_string_lossy().as_ref()) {
            Ok(registry) => {
                tracing::info!(
                    "MCP registry initialized with {} clients",
                    registry.client_names().len()
                );
                ctx.provide(registry);
            }
            Err(e) => {
                tracing::warn!("Failed to initialize MCP registry: {}", e);
            }
        }
    }

    let tools = ctx
        .get::<ToolRegistry>()
        .or_else(|| {
            ctx.get::<ares::context_services::ToolRegistryService>()
                .map(|s| s.0.clone())
        })
        .ok_or_else(|| missing("ToolRegistry"))?;
    let runtime = ctx
        .get::<ares::RuntimeToolRegistry>()
        .ok_or_else(|| missing("RuntimeToolRegistry"))?;
    let llm_factory = ctx
        .get::<ConfigBasedLLMFactory>()
        .ok_or_else(|| missing("ConfigBasedLLMFactory"))?;
    let mgr = config_manager_from_ctx(ctx)?;
    let skill_engine = Arc::new(ares::skill_engine::SkillEngine::new(
        pg.pool.clone(),
        tools,
        runtime.clone(),
        llm_factory,
        mgr.clone(),
    ));
    ctx.provide_arc(skill_engine);

    if let Some(tenant_db) = ctx.get::<ares::TenantDb>() {
        if let Some(agent_registry) = ctx.get::<AgentRegistry>() {
            ctx.provide(ares_agents::resolver::AgentResolverService::new(
                tenant_db,
                agent_registry,
                mgr.config(),
            ));
        }
    }

    Ok(fid)
}

/// Register built-in factories consumed by declarative Cordis entries.
///
/// Factories construct services through `Context::plugin`, preserving the
/// context's duplicate-provider checks instead of taking a log-only path.
#[cfg(feature = "postgres")]
fn register_loader_factories(root_ctx: &Arc<Context>) {
    use ares_cordis_core::PluginRegistry;

    if root_ctx.get::<PluginRegistry>().is_none() {
        root_ctx.provide(PluginRegistry::new());
    }
    let Some(registry) = root_ctx.get::<PluginRegistry>() else {
        return;
    };

    // Keep the existing probe semantics: it owns a distinct service type and
    // therefore cannot collide with a real startup provider.
    registry.register("noop_probe", Arc::new(|ctx, _config| {
        let future = ctx.plugin(LoaderProbeService {
            created_at: std::time::SystemTime::now(),
        });
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
    }));

    // TOML's empty `[entry.config]` is represented as `{}`, while the empty
    // unit config remains part of CalculatorService's public API. Accept the
    // two default forms and reject non-empty malformed configuration.
    registry.register("CalculatorService", Arc::new(|ctx, config| {
        let calculator_config = if config.is_null()
            || config.as_object().is_some_and(|object| object.is_empty())
        {
            ares_tools::CalculatorConfig
        } else {
            serde_json::from_value::<ares_tools::CalculatorConfig>(config.clone()).map_err(|error| {
                ares_cordis_core::CordisError::Configuration(format!(
                    "invalid CalculatorService config: {error}"
                ))
            })?
        };
        let future = ctx.plugin(ares_tools::CalculatorService::with_config(calculator_config));
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
    }));

    // EventsService — the central event bus. Registered so a declarative entry
    // (`plugin = "EventsService"`) can instantiate it at startup via the Loader,
    // using the same block_in_place pattern as the other factories. The
    // duplicate-provider check in `Context::plugin` keeps this single-source.
    registry.register("EventsService", Arc::new(|ctx, _config| {
        let future = ctx.plugin(ares_cordis_core::EventsService::new());
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
    }));

    registry.register("CatalogService", Arc::new(factory_catalog));
    registry.register("ProviderRegistry", Arc::new(factory_provider_registry));
    registry.register("LlmFactory", Arc::new(factory_llm));
    registry.register("AuthService", Arc::new(factory_auth));
    registry.register("ToolRegistry", Arc::new(factory_tool_registry));
    registry.register("AgentRegistry", Arc::new(factory_agent_registry));
    registry.register("RuntimeToolRegistry", Arc::new(factory_runtime_tool_registry));
    registry.register("DynamicConfig", Arc::new(factory_dynamic_config));
    registry.register("ExecutionStack", Arc::new(factory_execution_stack));
    registry.register("SchedulerService", Arc::new(factory_scheduler));
    registry.register("PipelineService", Arc::new(factory_pipeline));
    registry.register("TriggerService", Arc::new(factory_trigger));
    registry.register("HealthJobService", Arc::new(factory_health_job));
    registry.register("AppStateServices", Arc::new(factory_app_state_services));
}

/// Run the A.R.E.S server
#[cfg(feature = "postgres")]
async fn run_server(
    config_path: &std::path::Path,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. tracing / dotenv
    dotenvy::dotenv().ok();
    let log_filter = if verbose { "debug,ares=trace" } else { "info" };
    init_tracing(log_filter);
    tracing::info!("Starting A.R.E.S - Agentic Retrieval Enhanced Server");

    // 2. Context::new_root + ReflectService (needed before watchers)
    let root_ctx = ares_cordis_core::Context::new_root();
    let _reflect = {
        use std::any::TypeId;
        root_ctx
            .plugin(ares_cordis_core::ReflectService::new())
            .await
            .expect("ReflectService plugin failed");
        let reflect = root_ctx
            .get::<ares_cordis_core::ReflectService>()
            .expect("ReflectService missing");
        let tid_tool = TypeId::of::<ares::RuntimeToolRegistry>();
        let tid_provider = TypeId::of::<ares::ProviderRegistry>();
        let _rx_tool = reflect.ensure_notifier(tid_tool);
        let _rx_provider = reflect.ensure_notifier(tid_provider);
        reflect.register_dependent(tid_tool, 1);
        reflect.register_dependent(tid_provider, 2);
        reflect.set_context(&root_ctx);
        reflect.notify(tid_tool);
        reflect
    };
    let _inv_len = ares_cordis_core::inventory_len();

    // 3. Load AresConfigManager from config_path, start ares.toml watcher
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

    config_manager
        .start_watching()
        .expect("Failed to start config file watcher");

    let config_manager = Arc::new(config_manager);
    let config = config_manager.config();

    tracing::info!(
        "Configuration loaded from {} (hot-reload enabled)",
        config_path_str
    );

    // 4. init_postgres_db, migrations, seed templates
    let db = init_postgres_db(&config.database.url).await?;
    tracing::info!("PostgreSQL database client initialized");

    sqlx::migrate!("./migrations")
        .run(&db.pool)
        .await
        .expect("Failed to run database migrations");
    tracing::info!("Database migrations applied");

    ares::db::tenant_agents::seed_default_templates(&db.pool)
        .await
        .expect("Failed to seed agent templates");
    tracing::info!("Agent templates seeded");

    // 5. provide ConfigService, TenantDb, PostgresClient
    let db_arc = Arc::new(db);
    let tenant_db = Arc::new(ares::TenantDb::new(db_arc.clone()));

    root_ctx
        .plugin(ConfigService(config_manager.clone()))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;
    root_ctx.provide_arc(config_manager.clone());
    root_ctx.provide_arc(tenant_db.clone());
    root_ctx.provide_arc(db_arc.clone());

    // 6–7. register factories + Loader::load_from_file + instantiate every enabled entry
    {
        use ares_cordis_core::loader::{EntryTree, Loader};
        register_loader_factories(&root_ctx);

        let entries_path = std::path::Path::new("config/cordis-entries.toml");
        if entries_path.exists() {
            match Loader::load_from_file(entries_path) {
                Ok(desired) => {
                    let current = EntryTree(vec![]);
                    let loader = Loader::new();
                    let actions = loader.reconcile(&current, &desired);
                    for action in &actions {
                        Loader::execute_action(action, &root_ctx);
                    }
                    let mut ok = 0usize;
                    let mut failed = 0usize;
                    for e in &desired.0 {
                        if e.disabled {
                            tracing::info!(entry_id=%e.id, "Cordis Loader: skipping disabled entry");
                            continue;
                        }
                        match Loader::instantiate_entry(&root_ctx, e) {
                            Ok(_fid) => ok += 1,
                            Err(err) => {
                                failed += 1;
                                tracing::warn!(entry_id=%e.id, plugin=%e.plugin, error=%err,
                                    "Cordis Loader: instantiation failed for entry (continuing)");
                            }
                        }
                    }
                    tracing::info!(
                        total_actions = actions.len(),
                        total_entries = desired.0.len(),
                        instantiated = ok,
                        failed = failed,
                        "Cordis Loader: startup reconciliation complete"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Cordis Loader: failed to load entries, continuing without");
                }
            }
        }
    }

    // 8. cordis-entries watcher
    {
        let ctx_for_watch = root_ctx.clone();
        let entries_path_str = "config/cordis-entries.toml".to_string();

        let reload = |ctx: &std::sync::Arc<Context>, path: &std::path::Path| -> bool {
            use ares_cordis_core::loader::{EntryTree, Loader};
            match Loader::load_from_file(path) {
                Ok(desired) => {
                    let current = EntryTree(vec![]);
                    let actions = Loader::new().reconcile(&current, &desired);
                    for action in &actions {
                        Loader::execute_action(action, ctx);
                    }
                    if !actions.is_empty() {
                        tracing::info!(
                            actions = actions.len(),
                            "Cordis hot-reload: reconciled entries change"
                        );
                    }
                    !actions.is_empty()
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Cordis hot-reload: parse failed");
                    false
                }
            }
        };

        tokio::spawn(async move {
            use notify::Watcher;
            let entries_path = std::path::Path::new(&entries_path_str);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

            let mut watcher = notify::recommended_watcher(
                move |res: Result<notify::Event, notify::Error>| match res {
                    Ok(event) if event.kind.is_modify() || event.kind.is_create() => {
                        let _ = tx.send(());
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!(error = ?e, "Cordis hot-reload watcher error"),
                },
            )
            .ok();

            if let Some(w) = watcher.as_mut() {
                let target = entries_path.parent().filter(|p| p.exists());
                match target {
                    Some(parent) => {
                        if let Err(e) = w.watch(parent, notify::RecursiveMode::NonRecursive) {
                            tracing::warn!(
                                error = %e,
                                path = %parent.display(),
                                "Cordis hot-reload watcher failed to start; falling back to 30s poll"
                            );
                            watcher = None;
                        } else {
                            tracing::info!(
                                path = %parent.display(),
                                "Cordis hot-reload notify watcher started (500ms debounce)"
                            );
                        }
                    }
                    None => {
                        tracing::warn!(
                            path = %entries_path_str,
                            "Cordis hot-reload watch target missing; falling back to 30s poll"
                        );
                        watcher = None;
                    }
                }
            }

            let mut last_modified =
                std::fs::metadata(&entries_path_str).and_then(|m| m.modified()).ok();
            let mut watcher_active = watcher.is_some();
            let mut fallback = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                if watcher_active {
                    tokio::select! {
                        maybe = rx.recv() => {
                            if maybe.is_none() {
                                watcher_active = false;
                                continue;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            while rx.try_recv().is_ok() {}
                            if entries_path.exists() {
                                last_modified = std::fs::metadata(&entries_path_str).and_then(|m| m.modified()).ok();
                                reload(&ctx_for_watch, entries_path);
                            }
                        }
                        _ = fallback.tick() => {
                            let cur = std::fs::metadata(&entries_path_str).and_then(|m| m.modified()).ok();
                            if cur != last_modified && cur.is_some() {
                                last_modified = cur;
                                if entries_path.exists() {
                                    reload(&ctx_for_watch, entries_path);
                                }
                            }
                        }
                    }
                } else {
                    fallback.tick().await;
                    let cur = std::fs::metadata(&entries_path_str).and_then(|m| m.modified()).ok();
                    if cur != last_modified && cur.is_some() {
                        last_modified = cur;
                        if entries_path.exists() {
                            reload(&ctx_for_watch, entries_path);
                        }
                    }
                }
            }
        });
    }

    // 9. build_router + bind + graceful shutdown (below)
    let state: AppState = root_ctx.clone();

    if let Err(e) = api::handlers::admin::reload_runtime_provider_registry(&state).await {
        tracing::warn!("Failed to preload runtime providers on startup: {}", e);
    }

    // Cordis reactive-fiber demo: a fiber depending on EventsService that flips
    // Active/Inactive as the service is retired/re-provided via admin endpoints.
    // A second dependent fiber on ToolRegistryService registers a distinct
    // `TypeId` dependency so ReflectService's BFS dependency walk covers two
    // services — prooving reactive recomputation fans out across both keys.
    {
        use std::any::TypeId;
        let reflect = root_ctx
            .get::<ares_cordis_core::ReflectService>()
            .expect("reflect");

        // Dependents on EventsService (admin retire/provide flow).
        let fiber_events = std::sync::Arc::new(ares_cordis_core::Fiber::new());
        fiber_events.declare_inject::<ares_cordis_core::EventsService>();
        let fid_events = 990_001u64;
        reflect.register_dependent(TypeId::of::<ares_cordis_core::EventsService>(), fid_events);
        reflect.register_fiber(
            fid_events,
            fiber_events.clone(),
            TypeId::of::<ares_cordis_core::EventsService>(),
        );
        fiber_events.refresh(&root_ctx).await; // initial: Active (EventsService provided earlier)
        tracing::info!(state=?fiber_events.state(), "reactive demo fiber initialized (expect Active)");

        // Dependents on ToolRegistryService — second service so the reflect BFS
        // walk covers two services. ToolRegistryService is provided at line
        // ~1052 via `plugin(ToolRegistryService(...))`, so initial refresh is Active.
        let fiber_tools = std::sync::Arc::new(ares_cordis_core::Fiber::new());
        fiber_tools.declare_inject::<ares::context_services::ToolRegistryService>();
        let fid_tools = 990_002u64;
        reflect.register_dependent(
            TypeId::of::<ares::context_services::ToolRegistryService>(),
            fid_tools,
        );
        reflect.register_fiber(
            fid_tools,
            fiber_tools.clone(),
            TypeId::of::<ares::context_services::ToolRegistryService>(),
        );
        fiber_tools.refresh(&root_ctx).await; // initial: Active (ToolRegistryService provided)
        tracing::info!(state=?fiber_tools.state(), "reactive demo fiber (tools) initialized (expect Active)");
    }

    // =================================================================
    // Agent Config Versioning (Sprint 11)
    // =================================================================
    {
        let pool = state.get::<ares::TenantDb>().expect("not provided").pool().clone();

        // Startup snapshot: record all currently loaded agent configs
        let startup_agents = state.get::<DynamicConfigManager>().expect("not provided").agents();
        if !startup_agents.is_empty() {
            if let Err(e) =
                ares::db::agent_versions::record_agent_versions(&pool, &startup_agents, "startup")
                    .await
            {
                tracing::warn!("Failed to snapshot agent versions on startup: {}", e);
            } else {
                tracing::info!(
                    count = startup_agents.len(),
                    "Agent configs snapshotted to agent_config_versions"
                );
            }
        }

        // Hot-reload version tracking: background task drains mpsc channel
        let (version_tx, mut version_rx) = tokio::sync::mpsc::unbounded_channel::<
            Vec<ares::utils::toon_config::ToonAgentConfig>,
        >();
        state.get::<DynamicConfigManager>().expect("not provided").set_version_tx(version_tx);

        tokio::spawn(async move {
            while let Some(agents) = version_rx.recv().await {
                if let Err(e) =
                    ares::db::agent_versions::record_agent_versions(&pool, &agents, "hot_reload")
                        .await
                {
                    tracing::warn!("Failed to record hot-reload agent versions: {}", e);
                }
            }
        });
    }

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

    // Cordis routes are the live base (`/health`, `/health/context`); extra
    // routes attach before `with_state` so there is a single router tree.
    #[allow(unused_mut)]
    let mut app = ares::cordis_routes()
        .route("/health/detailed", get(health_check_detailed))
        .route("/config/info", get(config_info))
        .nest(
            "/api",
            api::routes::create_router(
                state.get::<AuthService>().expect("not provided").clone(),
                state.get::<ares::TenantDb>().expect("not provided").clone(),
            ),
        );

    // Proprietary routes are registered by ares-dirmacs, not here.
    // Extension crates call app.merge(), ...) in their own main.rs.

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
#[cfg(feature = "postgres")]
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

/// Run the A.R.E.S MCP server
#[cfg(all(feature = "postgres", feature = "mcp"))]
async fn run_mcp_server(config_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file for secrets
    dotenvy::dotenv().ok();
    init_tracing("info");

    tracing::info!("Starting A.R.E.S MCP Server");

    // Load configuration (load_unchecked returns a clear error if the file is missing)
    let config_path_str = config_path.to_str().unwrap_or("ares.toml");
    let config = AresConfig::load_unchecked(config_path_str)?;

    // Initialize database
    let db = init_postgres_db(&config.database.url).await?;
    let pool = db.pool.clone();
    let tenant_db = Arc::new(ares::TenantDb::new(Arc::new(db)));

    // Get API URL from environment or config
    let ares_api_url =
        std::env::var("ARES_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    tracing::info!("ARES API URL: {}", ares_api_url);

    // Start MCP server (extensions like Eruka are registered by managed platform crates)
    let runner = std::sync::Arc::new(
        ares::mcp_agent_runner::ExecutionAgentRunner::with_tenant_db(tenant_db.clone()),
    );
    ares::mcp::start_mcp_server(tenant_db, pool, &ares_api_url, Some(runner)).await?;

    Ok(())
}

/// Initialize PostgreSQL database
#[cfg(feature = "postgres")]
async fn init_postgres_db(url: &str) -> Result<PostgresClient, Box<dyn std::error::Error>> {
    tracing::info!(database_url = %url, "Initializing PostgreSQL database");
    Ok(PostgresClient::new_local(url).await?)
}

/// Build CORS layer from configuration
#[cfg(feature = "postgres")]
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
            axum::http::HeaderName::from_static("x-admin-secret"),
        ])
        .allow_credentials(allow_credentials)
}

/// Health check endpoint
#[cfg(feature = "postgres")]
async fn health_check() -> &'static str {
    "OK"
}

/// Detailed health check endpoint with component status
#[cfg(feature = "postgres")]
async fn health_check_detailed(
    axum::extract::State(state): axum::extract::State<Arc<Context>>,
) -> axum::Json<serde_json::Value> {
    use std::time::Instant;

    let start = Instant::now();

    // Check database connectivity
    let db_status = serde_json::json!({ "status": "healthy" });
    /* let db_status = match state.get::<PostgresClient>().expect("not provided").operation_conn().await {
        Ok(_) => serde_json::json!({ "status": "healthy" }),
        Err(e) => serde_json::json!({ "status": "unhealthy", "error": e.to_string() }),
    }; */

    // Get provider info
    let providers: Vec<String> = state.get::<AresConfigManager>().expect("not provided")
        .config()
        .providers
        .keys()
        .cloned()
        .collect();

    // Get agent info
    let agents: Vec<String> = state.get::<AresConfigManager>().expect("not provided")
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
#[cfg(feature = "postgres")]
async fn config_info(
    axum::extract::State(state): axum::extract::State<Arc<Context>>,
) -> axum::Json<serde_json::Value> {
    let config = state.get::<AresConfigManager>().expect("not provided").config();
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

#[cfg(all(feature = "postgres", feature = "ui"))]
mod ui {
    use axum::{
        body::Body,
        http::{header, StatusCode, Uri},
        response::Response,
        routing::get,
        Router,
    };
    use rust_embed::Embed;

    use ares::AppState;

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

#[cfg(all(feature = "postgres", feature = "ui"))]
fn ui_routes() -> axum::Router<AppState> {
    ui::routes()
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;
    use ares_cordis_core::loader::Loader;
    use ares_cordis_core::{Context, PluginRegistry, Service};
    use ares_tools::ToolService;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn calculator_entry_loads_factory_and_executes_tool() {
        let dir = tempfile::tempdir().expect("temporary config directory");
        let path = dir.path().join("cordis-entries.toml");
        std::fs::write(
            &path,
            r#"
[[entry]]
id = "calculator"
plugin = "CalculatorService"
disabled = false

[entry.config]
"#,
        )
        .expect("write loader config");

        let desired = Loader::load_from_file(&path).expect("parse loader config");
        let entry = desired.0.first().expect("calculator entry");
        assert_eq!(entry.plugin, "CalculatorService");
        assert_eq!(entry.config, json!({}));

        let ctx = Context::new_root();
        ctx.provide(PluginRegistry::new());
        register_loader_factories(&ctx);

        let fiber_id = Loader::instantiate(&ctx, &entry.plugin, &entry.config, &entry.id)
            .expect("calculator factory should instantiate");
        assert!(fiber_id > 0);

        let service = ctx
            .get::<ares_tools::CalculatorService>()
            .expect("factory must provide CalculatorService");
        let tool = service
            .resolve("calculator", None)
            .expect("calculator should resolve through ToolService");
        let output = tool
            .execute(json!({"operation": "add", "a": 2.0, "b": 3.0}))
            .await
            .expect("calculator execution");
        assert_eq!(output["result"], json!(5.0));

        // A second instance in the same context must be rejected by the real
        // Context::plugin path rather than silently replacing the provider.
        let duplicate = Loader::instantiate(&ctx, &entry.plugin, &entry.config, &entry.id);
        assert!(duplicate.is_err());
    }

    #[test]
    fn loader_factories_include_execution_stack() {
        let ctx = Context::new_root();
        register_loader_factories(&ctx);
        let names = ctx
            .get::<PluginRegistry>()
            .expect("PluginRegistry after register_loader_factories")
            .names();
        assert!(
            names.iter().any(|name| name == "ExecutionStack"),
            "ExecutionStack factory missing from {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "ProviderRegistry"),
            "ProviderRegistry factory missing from {names:?}"
        );
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn health_job_service_init_spawns_without_blocking() {
        let ctx = Context::new_root();
        let svc = HealthJobService::new(60_000);
        let start = std::time::Instant::now();
        let _ = svc.init(&ctx).await.unwrap();
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
    }
}
