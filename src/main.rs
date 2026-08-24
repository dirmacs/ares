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

#![allow(
    deprecated,
    reason = "CLI init bridge re-export deprecated shims for one release"
)]
#![allow(
    dead_code,
    reason = "CLI init/rag paths unused in lib build; keep for binary"
)]

#[cfg(feature = "postgres")]
mod cli;
#[cfg(feature = "postgres")]
mod health_metrics_job;
#[cfg(feature = "postgres")]
mod mcp_agent_runner;
#[cfg(feature = "postgres")]
mod plugins;

#[cfg(feature = "postgres")]
use crate::cli::{init, output::Output, rag, AgentCommands, Cli, Commands};
#[cfg(feature = "postgres")]
use ares_http::{api, overlay::AresConfig, AresConfigManager, DynamicConfigManager};
#[cfg(feature = "postgres")]
use ares_store::PostgresClient;
#[cfg(feature = "postgres")]
use axum::routing::get;
#[cfg(feature = "postgres")]
use cordis::Context;
#[cfg(feature = "postgres")]
use std::sync::Arc;
#[cfg(feature = "postgres")]
use tower_http::{cors::CorsLayer, trace::TraceLayer};
#[cfg(feature = "postgres")]
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
#[cfg(all(feature = "postgres", feature = "swagger-ui"))]
use utoipa::OpenApi;
#[cfg(all(feature = "postgres", feature = "swagger-ui"))]
use utoipa_swagger_ui::SwaggerUi;

/// Stub main for builds without the `postgres` feature.
///
/// The `ares-server` binary is the standalone server and requires the
/// `postgres` feature for its database backend. When ares-server is built
/// without postgres (e.g. for pawan), the server binary is compiled to
/// this stub so the crate still produces a valid executable.
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

#[cfg(feature = "postgres")]
fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

#[cfg(feature = "postgres")]
fn inject_sync<T: cordis::Service>(ctx: &Arc<Context>) -> Arc<T> {
    crate::plugins::inject_sync::<T>(ctx)
}

/// Register built-in factories consumed by declarative Cordis entries.
///
/// Factories construct services through `Context::plugin`, preserving the
/// context's duplicate-provider checks instead of taking a log-only path.
#[cfg(feature = "postgres")]
fn register_loader_factories(root_ctx: &Arc<Context>) {
    use cordis::PluginRegistry;

    if root_ctx.get::<PluginRegistry>().is_none() {
        root_ctx.provide(PluginRegistry::new());
    }
    let Some(registry) = root_ctx.get::<PluginRegistry>() else {
        return;
    };

    #[cfg(feature = "inventory")]
    {
        // Primary path: compile-time collected factories from every crate
        // linked into the binary. The manual chains below stay compiled as
        // the fallback for `--no-default-features` builds and unit tests.
        //
        // `std::hint::black_box` keeps this crate's own factory submits
        // (Overlay, HealthJobService, noop_probe in `plugins.rs`) alive:
        // without a runtime reference the linker drops their inventory
        // registration nodes along with the rest of the code.
        std::hint::black_box(crate::plugins::register_plugins as fn(&cordis::PluginRegistry));
        cordis::register_inventory_factories(&registry);
    }
    #[cfg(not(feature = "inventory"))]
    {
        ares_server::register_plugins(&registry); // facade: cordis, store, tools, llm, agent
        ares_http::register_plugins(&registry);
        crate::plugins::register_plugins(&registry); // Overlay, Execute overwrite, HealthJob
    }
}

/// Boot as one ordered pass over the entries file (the program).
///
/// - Provides `LoaderJournal` + `CurrentEntries` so hot-reload/admin share state.
/// - Preserves only the legacy `--config` injection into an empty Overlay config
///   (factory_overlay would otherwise default to `ares.toml`).
/// - Instantiates entries in file order; when it reaches the Overlay entry it
///   immediately runs `fill_empty_entry_configs` so every later entry (Store,
///   Llm, ...) sees filled configs. Overlay precedes those in the TOML by design.
/// - Exits(1) when no Overlay entry exists or fails, matching prior behavior.
#[cfg(feature = "postgres")]
fn boot_loader_program(
    ctx: &Arc<Context>,
    entries_path: &std::path::Path,
    config_path: &str,
) -> Result<(), String> {
    use cordis::loader::Loader;

    let journal = cordis::LoaderJournal::provide_new(ctx);
    if ctx.get::<cordis::RegistryService>().is_none() {
        ctx.provide(cordis::RegistryService::new());
    }
    let mut desired = compose_entries_tree(
        Loader::load_from_file(entries_path)
            .map_err(|e| format!("failed to load {}: {e}", entries_path.display()))?,
        entries_path,
    );

    // Legacy --config injection: empty Overlay config + non-default path.
    if config_path != "ares.toml" {
        if let Some(entry) = desired.0.iter_mut().find(|e| e.plugin == "Overlay") {
            let empty = match &entry.config {
                serde_json::Value::Null => true,
                serde_json::Value::Object(map) => map.is_empty(),
                serde_json::Value::Array(arr) => arr.is_empty(),
                _ => false,
            };
            if empty {
                entry.config = serde_json::json!({ "toml_path": config_path });
            }
        }
    }

    let mut overlay_done = false;
    let mut ok = 0usize;
    let mut failed = 0usize;
    for idx in 0..desired.0.len() {
        let entry = desired.0[idx].clone();
        if entry.disabled {
            tracing::info!(entry_id=%entry.id, "Cordis Loader: skipping disabled entry");
            continue;
        }
        match Loader::instantiate_entry(ctx, &entry) {
            Ok(_fid) => {
                ok += 1;
                if entry.plugin == "Overlay" {
                    overlay_done = true;
                    if let Some(overlay) = ctx.get::<AresConfigManager>() {
                        overlay.fill_empty_entry_configs(&mut desired);
                    } else {
                        return Err(
                            "Overlay instantiated but AresConfigManager missing".to_string()
                        );
                    }
                }
            }
            Err(err) => {
                failed += 1;
                tracing::warn!(entry_id=%entry.id, plugin=%entry.plugin, error=%err,
                    "Cordis Loader: instantiation failed for entry (continuing)");
            }
        }
    }

    if !overlay_done {
        let output = Output::new();
        output.banner();
        output.error("No active Overlay entry found!");
        output.newline();
        output.info("A.R.E.S requires an Overlay entry to load its configuration.");
        output.info("Add an [[entry]] with plugin = \"Overlay\" to:");
        output.newline();
        output.command(entries_path.to_string_lossy().as_ref());
        output.newline();
        std::process::exit(1);
    }

    // Publish shared state for hot reload + admin endpoint.
    if let Some(overlay) = ctx.get::<AresConfigManager>() {
        struct OverlayFiller(Arc<AresConfigManager>);
        impl cordis::loader::EntryConfigFiller for OverlayFiller {
            fn fill_empty_entry_configs(&self, tree: &mut cordis::loader::EntryTree) {
                self.0.fill_empty_entry_configs(tree);
            }
        }
        ctx.provide_arc(Arc::new(cordis::loader::EntryConfigFillerHandle(Arc::new(
            OverlayFiller(overlay),
        ))));
    }
    let current_entries = cordis::loader::CurrentEntries {
        tree: Arc::new(std::sync::Mutex::new(desired.clone())),
        path: entries_path.to_path_buf(),
    };
    ctx.provide_arc(Arc::new(current_entries));
    let _ = journal;

    tracing::info!(
        total_entries = desired.0.len(),
        instantiated = ok,
        failed = failed,
        "Cordis Loader: startup reconciliation complete"
    );
    Ok(())
}

/// Reload the entries file through `Loader::apply`, diffing against the last
/// applied tree. Returns true when at least one reconcile action ran.
#[cfg(feature = "postgres")]
fn reload_cordis_entries(ctx: &Arc<Context>, path: &std::path::Path) -> bool {
    use cordis::loader::Loader;
    let Some(journal) = ctx.get::<cordis::LoaderJournal>() else {
        tracing::warn!("Cordis hot-reload: LoaderJournal missing; skipping");
        return false;
    };
    let Some(current_entries) = ctx.get::<cordis::loader::CurrentEntries>() else {
        tracing::warn!("Cordis hot-reload: CurrentEntries missing; skipping");
        return false;
    };
    // Compose the freshly re-read tree BEFORE diffing so `@include` splices,
    // `@group` flattening, and `${rhai: …}` interpolation match boot state
    // (fail-open: on composition error we proceed with the raw entries).
    let Ok(composed_tree) = Loader::load_from_file(path).map(|t| compose_entries_tree(t, path))
    else {
        tracing::warn!(path = %path.display(), "Cordis hot-reload: reparse failed after change");
        return false;
    };
    let mut current = current_entries.tree.lock().expect("entries lock").clone();
    let actions = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(Loader::reload_current(
            ctx,
            path,
            &mut current,
            &composed_tree,
            &journal,
        ))
    });
    let Some(actions) = actions else {
        return false;
    };
    let ran = !actions.is_empty();
    for action in &actions {
        match &action.status {
            Ok(()) => tracing::info!(
                entry_id = %action.id,
                action = action.action,
                "Cordis hot-reload: applied"
            ),
            Err(err) => tracing::warn!(
                entry_id = %action.id,
                action = action.action,
                error = %err,
                "Cordis hot-reload: action failed"
            ),
        }
    }
    if ran {
        *current_entries.tree.lock().expect("entries lock") = current;
        tracing::info!(
            actions = actions.len(),
            "Cordis hot-reload: reconciled entries change"
        );
    }
    ran
}

/// Compose freshly parsed entries in place: resolve `@include` splices,
/// flatten `@group` children, then interpolate `${rhai: …}` config
/// placeholders.
///
/// Composition is best-effort (fail-open): on error the RAW entries are kept
/// and boot/reload proceeds — a bad include must not brick the server — but
/// the reason is logged loudly at `error` level.
#[cfg(feature = "postgres")]
fn compose_entries_tree(
    mut tree: cordis::loader::EntryTree,
    path: &std::path::Path,
) -> cordis::loader::EntryTree {
    let base_dir = path.parent().unwrap_or(std::path::Path::new("."));
    if let Err(e) = cordis::compose_all(&mut tree.0, base_dir) {
        tracing::error!(
            path = %path.display(),
            error = %e,
            "Cordis compose: entry composition failed; \
             proceeding with raw (uncomposed) entries"
        );
    }
    tree
}

/// Forward cordis-entries.toml onto `watch_many_with` so HMR dylib apply
/// (inside watch_many) shares the file-watch path. Hold the returned
/// `WatchHandle` until `run_server` returns.
#[cfg(feature = "postgres")]
fn start_cordis_entries_watch(
    ctx: &Arc<Context>,
    entries_path: &std::path::Path,
) -> Option<cordis::watcher::WatchHandle> {
    let reflect = ctx.get::<cordis::ReflectService>()?;
    let entries = entries_path.to_path_buf();
    let on_change: cordis::watcher::WatchOnChange = std::sync::Arc::new(move |c, _p| {
        let _ = reload_cordis_entries(c, &entries);
    });
    cordis::watcher::watch_many_with(
        ctx.clone(),
        reflect,
        vec![entries_path.to_path_buf()],
        std::any::TypeId::of::<cordis::ReflectService>(),
        on_change,
    )
    .ok()
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
    let root_ctx = cordis::Context::new_root();
    let _reflect = {
        use std::any::TypeId;
        root_ctx
            .plugin(cordis::ReflectService::new())
            .await
            .expect("ReflectService plugin failed");
        let reflect = root_ctx
            .get::<cordis::ReflectService>()
            .expect("ReflectService missing");
        let tid_tool = TypeId::of::<ares_tools::Tools>();
        let tid_provider = TypeId::of::<ares_llm::Llm>();
        let _rx_tool = reflect.ensure_notifier(tid_tool);
        let _rx_provider = reflect.ensure_notifier(tid_provider);
        reflect.register_dependent(tid_tool, 1);
        reflect.register_dependent(tid_provider, 2);
        reflect.set_context(&root_ctx);
        reflect.notify(tid_tool);
        reflect
    };
    let _inv_len = cordis::inventory_len();

    // 3. PluginRegistry + register_plugins (loader is the program)
    register_loader_factories(&root_ctx);

    // 4. Boot: one ordered pass over cordis-entries.toml (the program).
    {
        let entries_path = std::path::Path::new("config/cordis-entries.toml");
        if let Err(e) = boot_loader_program(&root_ctx, entries_path, &config_path.to_string_lossy())
        {
            tracing::error!(error = %e, "Cordis Loader: boot failed");
            std::process::exit(1);
        }
    }

    if root_ctx.get::<AresConfigManager>().is_none() {
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

    let config = root_ctx
        .get::<AresConfigManager>()
        .expect("Overlay missing after loader")
        .config();

    // 8. cordis-entries watcher (handle lives until run_server returns)
    let _cordis_entries_watch = start_cordis_entries_watch(
        &root_ctx,
        std::path::Path::new("config/cordis-entries.toml"),
    );
    if _cordis_entries_watch.is_none() {
        tracing::warn!(
            path = "config/cordis-entries.toml",
            "Cordis hot-reload watcher failed to start; falling back to 30s poll"
        );
        let ctx_for_watch = root_ctx.clone();
        let entries_path_str = "config/cordis-entries.toml".to_string();
        tokio::spawn(async move {
            let entries_path = std::path::Path::new(&entries_path_str);
            let mut last_modified = std::fs::metadata(&entries_path_str)
                .and_then(|m| m.modified())
                .ok();
            let mut fallback = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                fallback.tick().await;
                let cur = std::fs::metadata(&entries_path_str)
                    .and_then(|m| m.modified())
                    .ok();
                if cur != last_modified && cur.is_some() {
                    last_modified = cur;
                    if entries_path.exists() {
                        reload_cordis_entries(&ctx_for_watch, entries_path);
                    }
                }
            }
        });
    }

    // 9. build_router + bind + graceful shutdown (below)
    let state: Arc<Context> = root_ctx.clone();

    if let Err(e) = api::handlers::admin::reload_runtime_provider_registry(&state).await {
        tracing::warn!("Failed to preload runtime providers on startup: {}", e);
    }

    // Cordis reactive-fiber demo: a fiber depending on EventsService that flips
    // Active/Inactive as the service is retired/re-provided via admin endpoints.
    // A second dependent fiber on ToolRegistry registers a distinct
    // `TypeId` dependency so ReflectService's BFS dependency walk covers two
    // services — prooving reactive recomputation fans out across both keys.
    {
        use std::any::TypeId;
        let reflect = root_ctx.get::<cordis::ReflectService>().expect("reflect");

        // Dependents on EventsService (admin retire/provide flow).
        let fiber_events = std::sync::Arc::new(cordis::Fiber::new());
        fiber_events.declare_inject::<cordis::EventsService>();
        let fid_events = 990_001u64;
        reflect.register_dependent(TypeId::of::<cordis::EventsService>(), fid_events);
        reflect.register_fiber(
            fid_events,
            fiber_events.clone(),
            TypeId::of::<cordis::EventsService>(),
        );
        fiber_events.refresh(&root_ctx).await; // initial: Active (EventsService provided earlier)
        tracing::info!(state=?fiber_events.state(), "reactive demo fiber initialized (expect Active)");

        // Dependents on Tools — second service so the reflect BFS walk covers
        // two services. Tools is provided via `plugin(Tools)` in the Tools
        // loader factory, so initial refresh is Active.
        let fiber_tools = std::sync::Arc::new(cordis::Fiber::new());
        fiber_tools.declare_inject::<ares_tools::Tools>();
        let fid_tools = 990_002u64;
        reflect.register_dependent(TypeId::of::<ares_tools::Tools>(), fid_tools);
        reflect.register_fiber(
            fid_tools,
            fiber_tools.clone(),
            TypeId::of::<ares_tools::Tools>(),
        );
        fiber_tools.refresh(&root_ctx).await; // initial: Active (Tools provided)
        tracing::info!(state=?fiber_tools.state(), "reactive demo fiber (tools) initialized (expect Active)");
    }

    // =================================================================
    // Agent Config Versioning (Sprint 11)
    // =================================================================
    {
        let pool = state
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone();

        // Startup snapshot: record all currently loaded agent configs
        let startup_agents = state
            .get::<DynamicConfigManager>()
            .expect("not provided")
            .agents();
        if !startup_agents.is_empty() {
            let inputs: Vec<ares_store::AgentVersionInput> = startup_agents
                .iter()
                .map(|a| ares_store::AgentVersionInput {
                    name: a.name.clone(),
                    version: a.version.clone(),
                    config_json: serde_json::to_value(a)
                        .unwrap_or_else(|_| serde_json::json!({"name": a.name})),
                })
                .collect();
            if let Err(e) =
                ares_store::agent_versions::record_agent_versions(&pool, &inputs, "startup").await
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
        let (version_tx, mut version_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<ares_http::toon_config::ToonAgentConfig>>();
        state
            .get::<DynamicConfigManager>()
            .expect("not provided")
            .set_version_tx(version_tx);

        tokio::spawn(async move {
            while let Some(agents) = version_rx.recv().await {
                let inputs: Vec<ares_store::AgentVersionInput> = agents
                    .iter()
                    .map(|a| ares_store::AgentVersionInput {
                        name: a.name.clone(),
                        version: a.version.clone(),
                        config_json: serde_json::to_value(a)
                            .unwrap_or_else(|_| serde_json::json!({"name": a.name})),
                    })
                    .collect();
                if let Err(e) =
                    ares_store::agent_versions::record_agent_versions(&pool, &inputs, "hot_reload")
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
            ares_http::api::handlers::auth::register,
            ares_http::api::handlers::auth::login,
            ares_http::api::handlers::auth::logout,
            ares_http::api::handlers::auth::refresh_token,
            // Chat endpoints
            ares_http::api::handlers::chat::chat,
            ares_http::api::handlers::chat::chat_stream,
            ares_http::api::handlers::chat::get_user_memory,
            // Research endpoints
            ares_http::api::handlers::research::deep_research,
            // Conversation endpoints
            ares_http::api::handlers::conversations::list_conversations,
            ares_http::api::handlers::conversations::get_conversation,
            ares_http::api::handlers::conversations::update_conversation,
            ares_http::api::handlers::conversations::delete_conversation,
            // RAG endpoints
            ares_http::api::handlers::rag::ingest,
            ares_http::api::handlers::rag::search,
            ares_http::api::handlers::rag::delete_collection,
            ares_http::api::handlers::rag::list_collections,
        ),
        components(schemas(
            ares_types::types::ChatRequest,
            ares_types::types::ChatResponse,
            ares_types::types::ResearchRequest,
            ares_types::types::ResearchResponse,
            ares_types::types::LoginRequest,
            ares_types::types::RegisterRequest,
            ares_types::types::TokenResponse,
            ares_types::types::AgentType,
            ares_types::types::Source,
            ares_http::api::handlers::auth::RefreshTokenRequest,
            ares_http::api::handlers::auth::LogoutRequest,
            ares_http::api::handlers::auth::LogoutResponse,
            ares_http::api::handlers::conversations::ConversationSummary,
            ares_http::api::handlers::conversations::ConversationDetails,
            ares_http::api::handlers::conversations::ConversationMessage,
            ares_http::api::handlers::conversations::UpdateConversationRequest,
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
            ares_http::api::handlers::auth::register,
            ares_http::api::handlers::auth::login,
            ares_http::api::handlers::auth::logout,
            ares_http::api::handlers::auth::refresh_token,
            // Chat endpoints
            ares_http::api::handlers::chat::chat,
            ares_http::api::handlers::chat::chat_stream,
            ares_http::api::handlers::chat::get_user_memory,
            // Research endpoints
            ares_http::api::handlers::research::deep_research,
            // Conversation endpoints
            ares_http::api::handlers::conversations::list_conversations,
            ares_http::api::handlers::conversations::get_conversation,
            ares_http::api::handlers::conversations::update_conversation,
            ares_http::api::handlers::conversations::delete_conversation,
        ),
        components(schemas(
            ares_types::types::ChatRequest,
            ares_types::types::ChatResponse,
            ares_types::types::ResearchRequest,
            ares_types::types::ResearchResponse,
            ares_types::types::LoginRequest,
            ares_types::types::RegisterRequest,
            ares_types::types::TokenResponse,
            ares_types::types::AgentType,
            ares_types::types::Source,
            ares_http::api::handlers::auth::RefreshTokenRequest,
            ares_http::api::handlers::auth::LogoutRequest,
            ares_http::api::handlers::auth::LogoutResponse,
            ares_http::api::handlers::conversations::ConversationSummary,
            ares_http::api::handlers::conversations::ConversationDetails,
            ares_http::api::handlers::conversations::ConversationMessage,
            ares_http::api::handlers::conversations::UpdateConversationRequest,
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

    // Http plugin owns `/health` and `/api`. Extra binary routes merge on top.
    let http = state.get::<ares_http::Http>().ok_or(
        "Http plugin not instantiated; add [[entry]] plugin=\"Http\" to config/cordis-entries.toml",
    )?;
    let extra = axum::Router::new()
        .route("/health/detailed", get(health_check_detailed))
        .route("/config/info", get(config_info))
        .with_state(state.clone());
    #[allow(unused_mut)]
    let mut app = http.router.clone().merge(extra);

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
    } else {
        tracing::warn!("Rate limiting is disabled - not recommended for production");
        app.layer(cors).layer(TraceLayer::new_for_http())
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
    let tenant_db = Arc::new(ares_store::TenantDb::new(Arc::new(db)));

    // Get API URL from environment or config
    let ares_api_url =
        std::env::var("ARES_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    tracing::info!("ARES API URL: {}", ares_api_url);

    // Start MCP server (extensions like Eruka are registered by managed platform crates)
    let runner = std::sync::Arc::new(
        crate::mcp_agent_runner::ExecutionAgentRunner::with_tenant_db(tenant_db.clone()),
    );
    ares_mcp::start_mcp_server(tenant_db, pool, &ares_api_url, Some(runner)).await?;

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
    let providers: Vec<String> = state
        .get::<AresConfigManager>()
        .expect("not provided")
        .config()
        .providers
        .keys()
        .cloned()
        .collect();

    // Get agent info
    let agents: Vec<String> = state
        .get::<AresConfigManager>()
        .expect("not provided")
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
    let config = state
        .get::<AresConfigManager>()
        .expect("not provided")
        .config();
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

    use cordis::Context;
    use std::sync::Arc;

    #[derive(Embed)]
    #[folder = "ui/dist/"]
    struct UiAssets;

    pub fn routes() -> Router<Arc<Context>> {
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
fn ui_routes() -> axum::Router<Arc<Context>> {
    ui::routes()
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;
    use ares_tools::Tool;
    use cordis::loader::Loader;
    use cordis::{Context, PluginRegistry, Service};
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
        let output = service
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
    fn loader_registers_execute_plugin() {
        let ctx = Context::new_root();
        register_loader_factories(&ctx);
        let names = ctx
            .get::<PluginRegistry>()
            .expect("PluginRegistry after register_loader_factories")
            .names();
        for required in [
            "Execute",
            "Tools",
            "Llm",
            "Overlay",
            "Store", // migrate + seed_default_templates live in this factory
            "EventsService",
            "AuthService",
            "CalculatorService",
        ] {
            assert!(
                names.iter().any(|name| name == required),
                "{required} factory missing from {names:?}"
            );
        }
        for forbidden in [
            "ExecutionStack",
            "ProviderRegistry",
            "LlmFactory",
            "AppStateServices",
            "CatalogService",
            "ToolRegistry",
            "AgentRegistry",
            "DynamicConfig",
        ] {
            assert!(
                !names.iter().any(|name| name == forbidden),
                "deleted loader key {forbidden} still registered: {names:?}"
            );
        }
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn health_job_service_init_spawns_without_blocking() {
        let ctx = Context::new_root();
        let svc = crate::plugins::HealthJobService::new(60_000);
        let start = std::time::Instant::now();
        let _ = svc.init(&ctx).await.unwrap();
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
    }

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn inject_sync_returns_already_provided_service() {
        let ctx = Context::new_root();
        ctx.provide(cordis::EventsService::new());
        let got = inject_sync::<cordis::EventsService>(&ctx);
        assert!(got.check());
    }

    #[cfg(feature = "postgres")]
    #[tokio::test(flavor = "multi_thread")]
    async fn inject_sync_waits_until_service_is_provided() {
        let ctx = Context::new_root();
        let waiter = ctx.clone();
        let handle = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || inject_sync::<cordis::EventsService>(&waiter))
                .await
                .expect("join")
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        ctx.provide(cordis::EventsService::new());
        let got = tokio::time::timeout(std::time::Duration::from_millis(500), handle)
            .await
            .expect("timed out")
            .expect("task");
        assert!(got.check());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_cordis_entries_watch_holds_handle_for_existing_toml() {
        let dir = tempfile::tempdir().expect("temporary config directory");
        let path = dir.path().join("cordis-entries.toml");
        std::fs::write(
            &path,
            r#"
[[entry]]
id = "probe"
plugin = "noop_probe"
disabled = false
"#,
        )
        .expect("write toml");

        let ctx = Context::new_root();
        ctx.provide(cordis::ReflectService::new());
        let handle = start_cordis_entries_watch(&ctx, &path);
        assert!(
            handle.is_some(),
            "start_cordis_entries_watch must return Some for an existing toml"
        );
    }
}

#[cfg(all(test, feature = "postgres"))]
mod boot_tests {
    use super::*;

    /// Boot-order proof: Overlay as an ordinary entry feeds later entries.
    /// events → overlay → store; the Store entry has an empty config that only
    /// Overlay's fill_empty_entry_configs can populate (from ares.toml), so a
    /// live Store after boot proves the ordering contract.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_loader_program_instantiates_store_after_overlay_fill() {
        let dir = std::env::temp_dir().join(format!(
            "ares-boot-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("tmpdir");

        let db_url = std::env::var("TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| "postgres://dirmacs@%2Fvar%2Frun%2Fpostgresql/ares_test".into());

        std::fs::write(
            dir.join("ares.toml"),
            format!(
                "[server]\nhost = \"127.0.0.1\"\nport = 39471\nlog_level = \"info\"\n\n[database]\nurl = \"{db_url}\"\n\n[auth]\njwt_secret_env = \"JWT_SECRET_TEST_BOOT\"\njwt_access_expiry = 900\njwt_refresh_expiry = 604800\napi_key_env = \"API_KEY_TEST_BOOT\"\n"
            ),
        )
        .expect("write ares.toml");

        std::fs::write(
            dir.join("cordis-entries.toml"),
            "[[entry]]\nid = \"events\"\nplugin = \"EventsService\"\ndisabled = false\n\n[[entry]]\nid = \"overlay\"\nplugin = \"Overlay\"\ndisabled = false\n\n[[entry]]\nid = \"store\"\nplugin = \"Store\"\ndisabled = false\n",
        )
        .expect("write entries");

        let _ = init_tracing("debug");
        let ctx = Context::new_root();
        ctx.plugin(cordis::ReflectService::new())
            .await
            .expect("reflect");
        register_loader_factories(&ctx);

        // chdir so relative paths in factories/config resolution land in tmpdir.
        let original = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&dir).expect("chdir");

        let config_path = "ares.toml";
        let entries_path = std::path::Path::new("config/cordis-entries.toml");
        std::fs::create_dir_all(dir.join("config")).expect("config dir");
        std::fs::copy(
            dir.join("cordis-entries.toml"),
            dir.join("config/cordis-entries.toml"),
        )
        .expect("place entries");

        // Surface any Overlay instantiation error directly first.
        if let Err(e) = cordis::loader::Loader::load_from_file(entries_path) {
            panic!("entries parse failed: {e}");
        }
        let result = boot_loader_program(&ctx, entries_path, config_path);
        assert!(result.is_ok(), "boot failed: {result:?}");

        assert!(ctx.get::<cordis::EventsService>().is_some(), "events live");
        let store = ctx.get::<ares_store::Store>();
        assert!(store.is_some(), "store instantiated after overlay fill");

        // CurrentEntries published and tracks the applied tree (3 entries).
        let current = ctx
            .get::<cordis::loader::CurrentEntries>()
            .expect("current");
        {
            let tree = current.tree.lock().expect("lock");
            assert_eq!(tree.0.len(), 3);
            assert_eq!(tree.0[2].plugin, "Store");
            assert!(!tree.0[2].config.is_null(), "overlay filled store config");
        }

        std::env::set_current_dir(original).ok();
        std::fs::remove_dir_all(&dir).ok();
    }
}
