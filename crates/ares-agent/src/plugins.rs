//! Loader plugin registration for standalone and test hosts.
//!
//! The server overwrites the same `Execute` key after calling
//! [`register_plugins`].

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::AgentConfig;
use crate::execution::Execute;
use crate::registry::AgentRegistry;
use cordis::Plugin;

fn block_on_plugin<S: cordis::Service + 'static>(
    ctx: &std::sync::Arc<cordis::Context>,
    svc: S,
) -> Result<cordis::FiberId, cordis::CordisError> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.plugin(svc)))
}

/// Parse `entry.config` as a map of [`AgentConfig`].
///
/// Accepts a raw map of agent name → config, or `{"agents": map}`.
/// Empty objects and JSON null become an empty map.
fn parse_agents(config: &Value) -> Result<HashMap<String, AgentConfig>, cordis::CordisError> {
    if config.is_null() {
        return Ok(HashMap::new());
    }
    let Some(obj) = config.as_object() else {
        return Err(cordis::CordisError::Configuration(
            "Execute config must be a JSON object or null".into(),
        ));
    };
    if obj.is_empty() {
        return Ok(HashMap::new());
    }
    let source = match obj.get("agents") {
        Some(Value::Null) => return Ok(HashMap::new()),
        Some(agents) => agents,
        None => config,
    };
    if source.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(HashMap::new());
    }
    serde_json::from_value(source.clone()).map_err(|e| {
        cordis::CordisError::Configuration(format!("invalid Execute agents config: {e}"))
    })
}

fn factory_execute(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let tools = ctx.get::<ares_tools::Tools>().ok_or_else(|| {
        cordis::CordisError::Configuration("Tools is not on context".into())
    })?;
    #[cfg(feature = "postgres")]
    if ctx.get::<ares_store::TenantRealms>().is_none() {
        ctx.provide(ares_store::TenantRealms::new(
            std::any::TypeId::of::<ares_tools::Tools>(),
            std::any::TypeId::of::<Execute>(),
        ));
    }
    let agents = parse_agents(config)?;
    let providers = match ctx.get::<ares_llm::Llm>() {
        Some(llm) => llm.provider_registry(),
        None => Arc::new(ares_llm::ProviderRegistry::new()),
    };
    let registry = Arc::new(AgentRegistry::from_config(agents, providers, tools));
    ctx.provide_arc(registry.clone());
    ctx.provide(crate::EmergencyStop::new(false));
    let execute = Execute::new().with_agent_registry(registry);
    block_on_plugin(ctx, execute)
}

/// Register the `Execute` loader factory. Does not register `AgentRegistry`
/// or `ExecutionStack`. Engine keys are feature-gated.
pub fn register_plugins(reg: &cordis::PluginRegistry) {
    reg.register("Execute", Arc::new(factory_execute));
    #[cfg(feature = "scheduler")]
    reg.register("SchedulerService", Arc::new(factory_scheduler));
    #[cfg(feature = "pipeline")]
    reg.register("PipelineService", Arc::new(factory_pipeline));
    #[cfg(feature = "trigger")]
    reg.register("TriggerService", Arc::new(factory_trigger));
}

fn inject_sync<T: cordis::Service + 'static>(
    ctx: &Arc<cordis::Context>,
) -> Arc<T> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.inject::<T>()))
}

#[cfg(feature = "scheduler")]
fn factory_scheduler(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let cfg: crate::scheduler::SchedulerConfig = if config.is_null()
        || config.as_object().is_some_and(|o| o.is_empty())
    {
        crate::scheduler::SchedulerConfig::default()
    } else {
        serde_json::from_value(config.clone()).unwrap_or_default()
    };
    let db = inject_sync::<ares_store::PostgresClient>(ctx);
    let execution = inject_sync::<Execute>(ctx);
    block_on_plugin(
        ctx,
        crate::scheduler::SchedulerService::new(db, execution, cfg.tick_ms),
    )
}

#[cfg(feature = "pipeline")]
fn factory_pipeline(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let cfg: crate::pipeline::PipelineConfig = if config.is_null()
        || config.as_object().is_some_and(|o| o.is_empty())
    {
        crate::pipeline::PipelineConfig::default()
    } else {
        serde_json::from_value(config.clone()).unwrap_or_default()
    };
    let _ = crate::pipeline::PipelinePlugin.apply(ctx, cfg)?;
    let db = inject_sync::<ares_store::PostgresClient>(ctx);
    let execution = inject_sync::<Execute>(ctx);
    block_on_plugin(ctx, crate::pipeline::PipelineService::new(db, execution))
}

#[cfg(feature = "trigger")]
fn factory_trigger(
    ctx: &Arc<cordis::Context>,
    _config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let db = inject_sync::<ares_store::PostgresClient>(ctx);
    let execution = inject_sync::<Execute>(ctx);
    block_on_plugin(ctx, crate::trigger::TriggerService::new(db, execution))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordis::PluginRegistry;
    use serde_json::json;

    #[test]
    fn register_plugins_registers_execute() {
        let reg = PluginRegistry::new();
        register_plugins(&reg);
        assert!(reg.get("Execute").is_some());
        assert!(reg.get("AgentRegistry").is_none());
        assert!(reg.get("ExecutionStack").is_none());
        #[cfg(feature = "scheduler")]
        assert!(reg.get("SchedulerService").is_some());
        #[cfg(not(feature = "scheduler"))]
        assert!(reg.get("SchedulerService").is_none());
        #[cfg(feature = "pipeline")]
        assert!(reg.get("PipelineService").is_some());
        #[cfg(feature = "trigger")]
        assert!(reg.get("TriggerService").is_some());
    }

    #[test]
    fn parse_agents_null_and_empty() {
        assert!(parse_agents(&Value::Null).unwrap().is_empty());
        assert!(parse_agents(&json!({})).unwrap().is_empty());
        assert!(parse_agents(&json!({"agents": null})).unwrap().is_empty());
        assert!(parse_agents(&json!({"agents": {}})).unwrap().is_empty());
    }

    #[test]
    fn parse_agents_raw_map_and_wrapped() {
        let raw = json!({
            "research": { "model": "gpt-4", "system_prompt": "be helpful" }
        });
        let wrapped = json!({
            "agents": {
                "research": { "model": "gpt-4", "system_prompt": "be helpful" }
            }
        });
        let from_raw = parse_agents(&raw).unwrap();
        let from_wrapped = parse_agents(&wrapped).unwrap();
        assert_eq!(from_raw["research"].model, "gpt-4");
        assert_eq!(from_wrapped["research"].model, "gpt-4");
        assert_eq!(
            from_raw["research"].system_prompt.as_deref(),
            Some("be helpful")
        );
    }
}
