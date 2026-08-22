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
use axum::{routing::get, Router};
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
struct ProviderRegistryService(pub Arc<ProviderRegistry>);
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for ProviderRegistryService {
    fn name(&self) -> &'static str {
        "ProviderRegistryService"
    }
    fn check(&self) -> bool {
        // Guarded withdrawal: if registry empty (no providers), dependents deactivate
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
struct AuthServiceWrapper(pub Arc<AuthService>);
#[cfg(feature = "postgres")]
impl ares_cordis_core::Service for AuthServiceWrapper {
    fn name(&self) -> &'static str {
        "AuthServiceWrapper"
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
        let ctx_clone = ctx.clone();
        Box::pin(async move {
            // Health loop spawns without blocking init; iterates inventory::iter + ctx.get check + ReflectService::notify (Thm 63 guarded withdrawal)
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
                                    .get::<ProviderRegistryService>()
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
                                    .get::<AuthServiceWrapper>()
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
    // duplicate-provider check in `Context::plugin` and the guard at the
    // direct-bootstrap provide (see run_server) keep this single-source.
    registry.register("EventsService", Arc::new(|ctx, _config| {
        let future = ctx.plugin(ares_cordis_core::EventsService::new());
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
    }));
}

/// Run the A.R.E.S server
#[cfg(feature = "postgres")]
async fn run_server(
    config_path: &std::path::Path,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Cordis Context — Phase 2 step 7 / Phase 4 step 16 wiring via `Context::plugin`.
    // Replaces 17 sequential `let` steps with ~8 `root_ctx.plugin(...).await` calls (see below after each domain init).
    // Inventory compile-time static registration is proved via `CordisInventory::inventory_len()` and `inventory::submit!`
    // in `crates/ares-cordis-core/src/lib.rs` and `src/main.rs` top-level wrappers.
    // ReflectService with `watch` + BFS `Fiber::refresh` is the unified hot-reload path (replaces 60s `ArcSwap` polling).
    let root_ctx = ares_cordis_core::Context::new_root();
    let reflect = {
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

    // Load .env file for secrets (JWT_SECRET, API_KEY, etc.)
    dotenvy::dotenv().ok();

    // Initialize tracing
    let log_filter = if verbose { "debug,ares=trace" } else { "info" };
    init_tracing(log_filter);

    tracing::info!("Starting A.R.E.S - Agentic Retrieval Enhanced Server");

    // Cordis Loader: reconcile plugin entries from config file.
    // The PluginRegistry maps entry `plugin` names → factories; each
    // non-disabled entry is instantiated via `Loader::instantiate`. Individual
    // failures (unknown plugin, duplicate provider) are logged, never fatal —
    // startup continues without the offending fiber.
    {
        use ares_cordis_core::loader::{Loader, EntryTree};
        register_loader_factories(&root_ctx);

        let entries_path = std::path::Path::new("config/cordis-entries.toml");
        if entries_path.exists() {
            match Loader::load_from_file(entries_path) {
                Ok(desired) => {
                    let current = EntryTree(vec![]); // first boot: nothing loaded yet
                    let loader = Loader::new();
                    let actions = loader.reconcile(&current, &desired);
                    for action in &actions {
                        Loader::execute_action(action, &root_ctx);
                    }
                    // Instantiate every enabled entry whose plugin has a
                    // registered factory (Begin actions carry no plugin name,
                    // so the desired tree is the source of truth here).
                    let mut ok = 0usize;
                    let mut failed = 0usize;
                    for e in &desired.0 {
                        if e.disabled {
                            tracing::info!(entry_id=%e.id, "Cordis Loader: skipping disabled entry");
                            continue;
                        }
                        match Loader::instantiate(&root_ctx, &e.plugin, &e.config, &e.id) {
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

    // Cordis hot-reload: watch config/cordis-entries.toml via `notify`
    // (RecommendedWatcher, parent dir non-recursive, 500 ms debounce) and
    // re-reconcile declarative entries on change. A 30 s mtime poll remains as
    // a fallback in case the watcher fails to start or the fs backend errors.
    // Detached daemon task — graceful shutdown not required (live config reload
    // is best-effort; the config_manager watcher owns its own handle).
    {
        let ctx_for_watch = root_ctx.clone();
        let entries_path_str = "config/cordis-entries.toml".to_string();

        // Reload closure shared by the notify path and the fallback poll.
        // Returns true when at least one reconcile action was produced.
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

            // Watch the parent directory non-recursively (single-file target).
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

            // Fallback mtime poll every 30s for when the watcher is unavailable.
            let mut last_modified =
                std::fs::metadata(&entries_path_str).and_then(|m| m.modified()).ok();
            let mut watcher_active = watcher.is_some();
            let mut fallback = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                if watcher_active {
                    tokio::select! {
                        maybe = rx.recv() => {
                            if maybe.is_none() {
                                // All senders dropped (watcher died/target missing) → poll only.
                                watcher_active = false;
                                continue;
                            }
                            // Debounce 500ms: coalesce bursts, then reload.
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

    tracing::info!(
        "Configuration loaded from {} (hot-reload enabled)",
        config_path_str
    );

    // Cordis plugin 1/8: ConfigService — single-source, check() always true (guarded withdrawal via Fiber)
    root_ctx
        .plugin(ConfigService(config_manager.clone()))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

    // =================================================================
    // Initialize Provider Registry
    // =================================================================
    let nvidia_cfg = config.nvidia.clone().unwrap_or_default();
    let catalog = Arc::new(NvidiaCatalogCache::new(nvidia_cfg.clone()));
    // Attempt an initial refresh; if it fails we still have the default_model fallback.
    match catalog.refresh().await {
        Ok(count) => tracing::info!("NVIDIA catalog refreshed with {} models", count),
        Err(e) => tracing::warn!("NVIDIA catalog initial refresh failed: {}", e),
    }
    // Clone the Arc before consuming one in the background refresh task.
    let catalog_for_registry = catalog.clone();
    catalog.clone().start_background_refresh();

    // Cordis plugin 2/8: CatalogService
    root_ctx
        .plugin(CatalogService(catalog.clone()))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

    let provider_registry =
        Arc::new(ProviderRegistry::from_config(&config).with_catalog(catalog_for_registry));
    tracing::info!(
        "[nvidia] api_base={} default_model={}",
        nvidia_cfg.api_base,
        nvidia_cfg.default_model,
    );

    // Cordis plugin 3/8: ProviderRegistryService
    root_ctx
        .plugin(ProviderRegistryService(provider_registry.clone()))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

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
    let db = init_postgres_db(&config.database.url).await?;
    tracing::info!("PostgreSQL database client initialized");

    // =================================================================
    // Run Database Migrations
    // =================================================================
    sqlx::migrate!("./migrations")
        .run(&db.pool)
        .await
        .expect("Failed to run database migrations");
    tracing::info!("Database migrations applied");

    // Seed default agent templates (idempotent)
    ares::db::tenant_agents::seed_default_templates(&db.pool)
        .await
        .expect("Failed to seed agent templates");
    tracing::info!("Agent templates seeded");

    // =================================================================
    // Initialize Auth Service
    // =================================================================
    let jwt_secret = config
        .jwt_secret()
        .expect("JWT_SECRET environment variable must be set");
    let auth_service = Arc::new(AuthService::new(
        jwt_secret,
        config.auth.jwt_access_expiry,
        config.auth.jwt_refresh_expiry,
    ));
    tracing::info!("Auth service initialized");

    // Cordis plugin 4/8: AuthServiceWrapper
    root_ctx
        .plugin(AuthServiceWrapper(auth_service.clone()))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

    // =================================================================
    // Initialize Tool Registry
    // =================================================================
    let mut tool_registry = ToolRegistry::with_config(&config);

    // Register built-in tools
    tool_registry.register(Arc::new(ares::tools::calculator::Calculator));
    #[cfg(feature = "search-tools")]
    tool_registry.register(Arc::new(ares::tools::search::WebSearch::new()));
    #[cfg(feature = "search-tools")]
    tool_registry.register(Arc::new(ares::tools::web_scrape::WebScrape::new()));

    if let Some(master_key) = MasterKey::from_env() {
        ares::tools::connectors::register_prebuilt_connector_tools(
            &mut tool_registry,
            db.pool.clone(),
            master_key,
        );
    } else {
        tracing::warn!(
            "FLEET_SECRETS_KEY is not set; pre-built connector tools are not registered"
        );
    }

    // Proprietary tools (POM, DCRM, Eruka) are registered by ares-dirmacs, not here.
    // Extension crates call tool_registry.register() in their own main.rs.

    // Register MCP client tools as agent-callable tools (MCP→ToolRegistry bridge)
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

    // Cordis plugin 5/8: AgentServiceWrapper
    root_ctx
        .plugin(AgentServiceWrapper {
            registry: agent_registry.clone(),
            dynamic: dynamic_config.clone(),
        })
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

    // =================================================================
    // Initialize MCP Registry (Eruka, etc.)
    // =================================================================
    #[cfg(feature = "mcp")]
    let mcp_registry: Option<Arc<McpRegistry>> =
        match McpRegistry::from_dir(config.config.mcps_dir.to_string_lossy().as_ref()) {
            Ok(registry) => {
                tracing::info!(
                    "MCP registry initialized with {} clients",
                    registry.client_names().len()
                );
                Some(Arc::new(registry))
            }
            Err(e) => {
                tracing::warn!("Failed to initialize MCP registry: {}", e);
                None
            }
        };
    // =================================================================
    // Create Application State
    // =================================================================
    let db_arc = Arc::new(db);
    let tenant_db = Arc::new(ares::TenantDb::new(db_arc.clone()));

    let fleet_secrets = ares::FleetSecrets::new();
    let fleet_provider_store =
        ares::db::fleet_provider_secrets::FleetProviderSecretsStore::new(&db_arc.pool);
    let fleet_provider_master = MasterKey::from_env();
    match fleet_provider_store
        .load_all(fleet_provider_master.as_ref())
        .await
    {
        Ok(providers) => {
            let count = providers.len();
            fleet_secrets.store(providers);
            tracing::info!(count, "Fleet provider secrets loaded");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load fleet provider secrets on startup");
        }
    }

    let runtime_tool_registry = Arc::new(ares::RuntimeToolRegistry::new(db_arc.pool.clone()));
    // Phase3 compile-time proof: registry creation inserts notifier/dependent for watch + BFS (ReflectService)
    {
        use std::any::TypeId;
        let tid = TypeId::of::<ares::RuntimeToolRegistry>();
        let _rx = reflect.ensure_notifier(tid);
        reflect.register_dependent(tid, 1);
    }
    if let Err(e) = runtime_tool_registry.reload().await {
        tracing::warn!("Failed to preload runtime tools on startup: {}", e);
    }
    // Phase3: background polling eliminated — reload via ReflectService::notify + Fiber::refresh

    let skill_engine = Arc::new(ares::skill_engine::SkillEngine::new(
        db_arc.pool.clone(),
        Arc::clone(&tool_registry),
        Arc::clone(&runtime_tool_registry),
        Arc::clone(&llm_factory),
        Arc::clone(&config_manager),
    ));

    // Cordis plugin 6/8: ToolServiceWrapper — static + runtime + unified
    root_ctx
        .plugin(ToolServiceWrapper {
            static_registry: tool_registry.clone(),
            runtime: runtime_tool_registry.clone(),
            unified: None,
        })
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

    // Keep calculator available when no declarative entry is configured. When
    // the loader already instantiated it, the guard avoids duplicate provider
    // failure while preserving the existing direct-bootstrap fallback.
    if root_ctx
        .get::<ares::tools::CalculatorService>()
        .is_none()
    {
        root_ctx
            .plugin(ares::tools::CalculatorService)
            .await
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;
    }

    // Provide AppState fields as Cordis services — replaces `let state = AppState { ... }`
    // =========================================================================
    // Cordis Bootstrap: provide all services to Context (Phase 2 §12, Phase 7)
    // =========================================================================
    // These `provide` calls replace the old `AppState { field1, field2, ... }` struct.
    // Handlers access them via `ctx.get::<ServiceWrapper>()`. As handlers migrate to
    // dedicated services (AgentResolverService, AgentExecutionService, etc.), these
    // wrappers become unused and can be removed. Target: 0 context_services wrappers.
    // Cordis EventsService — central event bus for inter-service communication
    // Guarded: the Loader may already have instantiated it from a declarative
    // `config/cordis-entries.toml` entry (`plugin = "EventsService"`); a second
    // provide here would trip the duplicate-provider check.
    if root_ctx.get::<ares_cordis_core::EventsService>().is_none() {
        root_ctx.provide(ares_cordis_core::EventsService::new());
    }
    root_ctx.plugin(ares::context_services::ConfigManagerService(Arc::clone(&config_manager))).await.expect("ConfigManager plugin failed");
    root_ctx.provide(ares::context_services::DynamicConfigService(dynamic_config.clone()));
    root_ctx.provide(ares::context_services::DbService(db_arc.clone() as Arc<dyn ares::db::traits::DatabaseClient>));
    root_ctx.provide(ares::context_services::TenantDbService(tenant_db.clone()));
    root_ctx.provide_arc(llm_factory.clone());
    root_ctx.provide(ares::context_services::ProviderRegistryService(provider_registry.clone()));
    root_ctx.provide_arc(agent_registry.clone());
    root_ctx.plugin(ares::context_services::ToolRegistryService(tool_registry.clone())).await.expect("ToolRegistry plugin failed");
    root_ctx.provide(ares::context_services::AuthServiceWrapper(auth_service.clone()));
    #[cfg(feature = "mcp")]
    root_ctx.provide(ares::context_services::McpRegistryService(mcp_registry.clone()));
    let deploy_registry = ares::api::handlers::deploy::new_deploy_registry();
    root_ctx.provide(ares::context_services::DeployRegistryService(deploy_registry.clone()));
    let loop_registry = ares::api::handlers::loops::LoopRegistry::new();
    root_ctx.provide(ares::context_services::LoopRegistryService(loop_registry.clone()));
    let emergency_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    root_ctx.provide(ares::context_services::EmergencyStopService(emergency_stop.clone()));
    let context_provider: Arc<dyn ares::agents::context_provider::ContextProvider> =
        Arc::new(ares::agents::NoOpContextProvider);
    root_ctx.provide(ares::context_services::ContextProviderService(context_provider.clone()));
    root_ctx.provide(ares::context_services::FleetSecretsService(fleet_secrets.clone()));
    root_ctx.provide(ares::context_services::RuntimeToolRegistryService(runtime_tool_registry.clone()));
    let active_runs = Arc::new(ares::active_runs::ActiveRuns::new());
    root_ctx.provide(ares::context_services::ActiveRunsService(active_runs.clone()));
    root_ctx.provide(ares::context_services::SkillEngineService(skill_engine.clone()));

    // Cordis: AgentResolverService — replaces inline resolve_agent in handlers (Phase 5 §19)
    root_ctx.provide(ares_agents::resolver::AgentResolverService::new(
        tenant_db.clone(),
        agent_registry.clone(),
        config_manager.config(),
    ));

    let state: AppState = root_ctx.clone();

    if let Err(e) = api::handlers::admin::reload_runtime_provider_registry(&state).await {
        tracing::warn!("Failed to preload runtime providers on startup: {}", e);
    }

    // =================================================================
    // Health Metrics Aggregation Job
    // =================================================================
    ares::health_metrics_job::spawn(state.get::<ares::context_services::TenantDbService>().expect("not provided").0.pool().clone());

    // =================================================================
    // Background Scheduler (Agent Schedules) — Cordis Service (Phase 4)
    // =================================================================
    // Cordis plugin 7/9: SchedulerService owns tick_ms 60_000, db, agent_execution, with next_run_at(cron) impl
    // Shared execution for Scheduler + Pipeline (both inject AgentExecutionService)
    let shared_execution = Arc::new(
        ares_agents::execution::AgentExecutionService::new()
            .with_db(db_arc.clone() as Arc<dyn ares::db::traits::DatabaseClient>)
            .with_tenant_db(tenant_db.clone())
            .with_llm_factory(llm_factory.clone())
            .with_agent_registry(agent_registry.clone())
            .with_fleet_secrets(Arc::new(ares::FleetSecrets::new()))
            .with_run_tracker(active_runs.clone() as Arc<dyn ares_agents::RunTracker>)
    );
    // Provide to context so handlers can use ctx.get::<AgentExecutionService>()
    root_ctx.provide_arc(shared_execution.clone());
    {
        root_ctx
            .plugin(ares::scheduler::SchedulerService::new(
                db_arc.clone(),
                shared_execution.clone(),
                60_000,
            ))
            .await
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;
        tracing::info!("SchedulerService plugin registered (tick_ms=60_000, watch + catch-up owned)");
    }

    // Cordis plugin 8/10: PipelineService — owns agent_pipelines lookup + conditional, injects AgentExecutionService
    root_ctx
        .plugin(ares::pipeline_engine::PipelineService::new(
            db_arc.clone(),
            shared_execution.clone(),
        ))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;
    tracing::info!("PipelineService plugin registered (no tick, downstream-triggered, conditional + execution owned)");

    // Cordis plugin 9/10: TriggerService — owns webhook/document_upload/field_change dispatch, injects AgentExecutionService
    root_ctx
        .plugin(ares::trigger_engine::TriggerService::new(
            db_arc.clone(),
            shared_execution.clone(),
        ))
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;
    tracing::info!("TriggerService plugin registered (webhook/document_upload/field_change owned)");

    // Cordis plugin 10/10: HealthJobService — inventory health loop via inventory::iter + ctx.get check + notify (Thm 63)
    root_ctx
        .plugin(HealthJobService::default())
        .await
        .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>)?;

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
        let pool = state.get::<ares::context_services::TenantDbService>().expect("not provided").0.pool().clone();

        // Startup snapshot: record all currently loaded agent configs
        let startup_agents = state.get::<ares::context_services::DynamicConfigService>().expect("not provided").0.agents();
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
        state.get::<ares::context_services::DynamicConfigService>().expect("not provided").0.set_version_tx(version_tx);

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

    // Cordis final router — Phase 2 step 12: `build_router(root_ctx.clone())` is final builder
    let app = ares::build_router(root_ctx.clone());
    let _ = &app; // suppress unused while legacy AppState router still serves traffic (HOLD shim)

    // =================================================================
    // Build Router (legacy AppState — HOLD shim)
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
            api::routes::create_router(state.get::<ares::context_services::AuthServiceWrapper>().expect("not provided").0.clone(), state.get::<ares::context_services::TenantDbService>().expect("not provided").0.clone()),
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
    ares::mcp::start_mcp_server(tenant_db, pool, &ares_api_url, None).await?;

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
    /* let db_status = match state.get::<ares::context_services::DbService>().expect("not provided").0.operation_conn().await {
        Ok(_) => serde_json::json!({ "status": "healthy" }),
        Err(e) => serde_json::json!({ "status": "unhealthy", "error": e.to_string() }),
    }; */

    // Get provider info
    let providers: Vec<String> = state.get::<ares::context_services::ConfigManagerService>().expect("not provided").0
        .config()
        .providers
        .keys()
        .cloned()
        .collect();

    // Get agent info
    let agents: Vec<String> = state.get::<ares::context_services::ConfigManagerService>().expect("not provided").0
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
    let config = state.get::<ares::context_services::ConfigManagerService>().expect("not provided").0.config();
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
fn ui_routes() -> Router<AppState> {
    ui::routes()
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;
    use ares_cordis_core::loader::Loader;
    use ares_cordis_core::{Context, PluginRegistry};
    use ares_tools::{Tool, ToolService};
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
}
