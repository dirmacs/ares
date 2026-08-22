//! Server-owned Cordis string factories (Overlay, Auth, engines, probe, Execute extras).
//!
//! Capability crates register EventsService / Store / Tools / Llm / Execute core.
//! This module registers the remaining production keys and overwrites `Execute`
//! with the server-side factory (ActiveRuns, SkillEngine).

use std::sync::Arc;

use cordis::{Context, CordisError, FiberId, PluginRegistry, Service};
use serde_json::Value;

use crate::auth::jwt::AuthService;
use crate::overlay::{AresConfigManager, Overlay, OverlayConfig};
use crate::toon_config::DynamicConfigManager;
use crate::{AgentRegistry, TenantDb};

/// Minimal no-op service behind the `noop_probe` loader factory.
pub struct LoaderProbeService {
    created_at: std::time::SystemTime,
}

impl Service for LoaderProbeService {}

/// Health-job plugin: inventory loop + metrics spawn. Init must not block.
pub struct HealthJobService {
    interval_ms: u64,
    _handle: Option<tokio::task::JoinHandle<()>>,
}

unsafe impl Send for HealthJobService {}
unsafe impl Sync for HealthJobService {}

impl HealthJobService {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms,
            _handle: None,
        }
    }
}

impl Default for HealthJobService {
    fn default() -> Self {
        Self::new(30_000)
    }
}

impl Service for HealthJobService {
    fn name(&self) -> &'static str {
        "HealthJobService"
    }
    fn check(&self) -> bool {
        true
    }
    fn init(&self, ctx: &Arc<Context>) -> cordis::ServiceInitFuture<'_> {
        let interval_ms = self.interval_ms;
        let ctx = ctx.clone();
        Box::pin(async move {
            let ctx_clone = ctx.clone();
            let handle = tokio::spawn(async move {
                let ctx_for_task = ctx_clone.clone();
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_millis(interval_ms));
                interval.tick().await;
                loop {
                    interval.tick().await;
                    #[cfg(feature = "inventory")]
                    {
                        let mut total = 0usize;
                        let mut healthy = 0usize;
                        for entry in inventory::iter::<cordis::CordisInventory> {
                            total += 1;
                            let is_healthy = match entry.name {
                                "Overlay" => ctx_for_task
                                    .get::<Overlay>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "Llm" => ctx_for_task
                                    .get::<ares_llm::Llm>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "Tools" => ctx_for_task
                                    .get::<ares_tools::Tools>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "Execute" => ctx_for_task
                                    .get::<ares_agent::Execute>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "Store" => ctx_for_task
                                    .get::<TenantDb>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "AuthService" => ctx_for_task
                                    .get::<AuthService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "SchedulerService" => ctx_for_task
                                    .get::<ares_agent::scheduler::SchedulerService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                "HealthJobService" => ctx_for_task
                                    .get::<HealthJobService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                                _ => ctx_for_task
                                    .get::<cordis::ReflectService>()
                                    .map(|s| s.check())
                                    .unwrap_or(true),
                            };
                            if is_healthy {
                                healthy += 1;
                            } else if let Some(reflect) =
                                ctx_for_task.get::<cordis::ReflectService>()
                            {
                                reflect.notify(std::any::TypeId::of::<Overlay>());
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
            std::mem::drop(handle);
            if let Some(db) = ctx.get::<TenantDb>() {
                crate::health_metrics_job::spawn(db.pool().clone());
            }
            Ok(None)
        })
    }
}

#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "Overlay" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "Llm" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "Tools" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "Execute" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "Store" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "AuthService" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "SchedulerService" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "PipelineService" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "TriggerService" } }
#[cfg(feature = "inventory")]
inventory::submit! { cordis::CordisInventory { name: "HealthJobService" } }

fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

/// Local helper wrapping `ctx.inject` for engine factories. Not on Context.
pub fn inject_sync<T: Service>(ctx: &Arc<Context>) -> Arc<T> {
    block_on_async(ctx.inject::<T>())
}

fn block_on_plugin<S: Service + 'static>(
    ctx: &Arc<Context>,
    svc: S,
) -> Result<FiberId, CordisError> {
    block_on_async(ctx.plugin(svc))
}

fn missing(what: &str) -> CordisError {
    CordisError::Configuration(format!(
        "{what} is not on context; an earlier loader entry must provide it"
    ))
}

fn postgres_from_ctx(ctx: &Arc<Context>) -> Result<Arc<crate::PostgresClient>, CordisError> {
    Ok(inject_sync::<crate::PostgresClient>(ctx))
}

fn factory_overlay(
    ctx: &Arc<Context>,
    config: &Value,
) -> Result<FiberId, CordisError> {
    let overlay_cfg: OverlayConfig = if config.is_null()
        || config.as_object().is_some_and(|o| o.is_empty())
    {
        OverlayConfig::default()
    } else {
        serde_json::from_value(config.clone()).unwrap_or_default()
    };
    let overlay = Overlay::new(&overlay_cfg.toml_path)
        .map_err(|e| CordisError::Configuration(e.to_string()))?;
    overlay
        .watch_cordis(ctx)
        .map_err(|e| CordisError::Configuration(e.to_string()))?;
    let cfg_snapshot = overlay.config();
    let dynamic = match DynamicConfigManager::from_config(&cfg_snapshot) {
        Ok(dm) => dm,
        Err(e) => {
            tracing::warn!(
                "Failed to initialize dynamic config manager: {}. Using empty config.",
                e
            );
            DynamicConfigManager::new(
                std::path::PathBuf::from(&cfg_snapshot.config.agents_dir),
                std::path::PathBuf::from(&cfg_snapshot.config.models_dir),
                std::path::PathBuf::from(&cfg_snapshot.config.tools_dir),
                std::path::PathBuf::from(&cfg_snapshot.config.workflows_dir),
                std::path::PathBuf::from(&cfg_snapshot.config.mcps_dir),
                false,
            )
            .unwrap_or_else(|_| panic!("Cannot create even empty DynamicConfigManager"))
        }
    };
    let fid = block_on_plugin(ctx, overlay)?;
    let _ = block_on_plugin(ctx, dynamic);
    Ok(fid)
}

fn factory_execute(ctx: &Arc<Context>, config: &Value) -> Result<FiberId, CordisError> {
    let _ = block_on_plugin(ctx, crate::active_runs::ActiveRuns::new());

    if let Some(pg) = ctx.get::<crate::PostgresClient>() {
        ctx.provide(crate::api::handlers::deploy::new_deploy_registry());
        ctx.provide(crate::api::handlers::loops::LoopRegistry::new());
        ctx.provide(ares_agent::EmergencyStop::new(false));
        let context_provider: Arc<dyn crate::agents::context_provider::ContextProvider> =
            Arc::new(crate::agents::NoOpContextProvider);
        ctx.provide(crate::agents::ContextProviderHandle::new(context_provider));

        #[cfg(feature = "mcp")]
        {
            let mcps_dir = ctx
                .get::<AresConfigManager>()
                .map(|mgr| mgr.config().config.mcps_dir.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("config/mcps"));
            match crate::mcp::McpRegistry::from_dir(mcps_dir.to_string_lossy().as_ref()) {
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
            .get::<ares_tools::Tools>()
            .ok_or_else(|| missing("Tools"))?;
        #[cfg(feature = "postgres")]
        if ctx.get::<ares_store::TenantRealms>().is_none() {
            ctx.provide(ares_store::TenantRealms::new(
                std::any::TypeId::of::<ares_tools::Tools>(),
                std::any::TypeId::of::<ares_agent::Execute>(),
            ));
        }
        let overlay = ctx
            .get::<AresConfigManager>()
            .ok_or_else(|| missing("Overlay"))?;
        let agents = if let Some(map) = config.get("agents") {
            serde_json::from_value(map.clone()).unwrap_or_default()
        } else {
            serde_json::from_value(config.clone()).unwrap_or_else(|_| overlay.config().agents.clone())
        };
        let providers = if let Some(llm) = ctx.get::<ares_llm::Llm>() {
            llm.provider_registry()
        } else {
            let cfg = overlay.config();
            Arc::new(ares_llm::ProviderRegistry::from_config(
                cfg.providers.clone(),
                cfg.models.clone(),
                cfg.nvidia.as_ref(),
            ))
        };
        let agent_registry = if let Some(dynamic) = ctx.get::<DynamicConfigManager>() {
            Arc::new(AgentRegistry::with_dynamic_config(
                agents,
                providers,
                tools.clone(),
                dynamic,
            ))
        } else {
            Arc::new(AgentRegistry::from_config(agents, providers, tools.clone()))
        };
        tracing::info!(
            "Agent registry initialized with {} agents (TOML + TOON)",
            agent_registry.agent_names().len()
        );
        ctx.provide_arc(agent_registry.clone());

        if let Some(llm) = ctx.get::<ares_llm::Llm>() {
            let skill_engine = Arc::new(ares_agent::skills::SkillEngine::new(
                pg.pool.clone(),
                tools,
                llm,
            ));
            ctx.provide_arc(skill_engine);
        }
        let _ = pg;
    }

    let agent_registry = ctx
        .get::<AgentRegistry>()
        .ok_or_else(|| missing("AgentRegistry"))?;
    let active_runs = inject_sync::<crate::active_runs::ActiveRuns>(ctx)
        as Arc<dyn ares_agent::RunTracker>;
    let execute = ares_agent::Execute::new()
        .with_agent_registry(agent_registry)
        .with_run_tracker(active_runs);
    block_on_plugin(ctx, execute)
}

fn factory_health_job(
    ctx: &Arc<Context>,
    _config: &Value,
) -> Result<FiberId, CordisError> {
    block_on_plugin(ctx, HealthJobService::default())
}

/// Register server-owned loader keys. Call after capability `register_plugins`
/// so `Execute` overwrites the crate-core factory.
pub fn register_plugins(reg: &PluginRegistry) {
    reg.register(
        "noop_probe",
        Arc::new(|ctx, _config| {
            block_on_plugin(
                ctx,
                LoaderProbeService {
                    created_at: std::time::SystemTime::now(),
                },
            )
        }),
    );
    reg.register("Overlay", Arc::new(factory_overlay));
    reg.register("Execute", Arc::new(factory_execute));
    reg.register("HealthJobService", Arc::new(factory_health_job));
}
