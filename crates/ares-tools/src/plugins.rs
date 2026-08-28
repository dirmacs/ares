//! Loader plugin factories for CalculatorService and Tools.
//!
//! Factories call `cordis::Context::plugin` via block_in_place + block_on.
//! ToolRegistry and RuntimeToolRegistry are not provided as Services.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::ToolConfig;
use crate::registry::ToolRegistry;
use crate::{Calculator, CalculatorConfig, CalculatorService, Tools};

fn block_on_plugin<S: cordis::Service + 'static>(
    ctx: &Arc<cordis::Context>,
    svc: S,
) -> Result<cordis::FiberId, cordis::CordisError> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.plugin(svc)))
}

/// Register the `CalculatorService` and `Tools` loader factories
/// (manual fallback path; inventory carries the same pair).
pub fn register_plugins(reg: &cordis::PluginRegistry) {
    reg.register("CalculatorService", Arc::new(factory_calculator));
    reg.register("Tools", Arc::new(factory_tools));
}

#[cfg(feature = "inventory")]
inventory::submit! {
    cordis::CordisPluginFactory { name: "CalculatorService", make: factory_calculator }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    cordis::CordisPluginFactory { name: "Tools", make: factory_tools }
}

fn factory_calculator(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let calculator_config = if config.is_null()
        || config.as_object().is_some_and(|object| object.is_empty())
    {
        CalculatorConfig
    } else {
        serde_json::from_value::<CalculatorConfig>(config.clone()).map_err(|error| {
            cordis::CordisError::Configuration(format!("invalid CalculatorService config: {error}"))
        })?
    };
    block_on_plugin(ctx, CalculatorService::with_config(calculator_config))
}

fn parse_tool_configs(config: &Value) -> Result<HashMap<String, ToolConfig>, cordis::CordisError> {
    if config.is_null() {
        return Ok(HashMap::new());
    }
    let Some(obj) = config.as_object() else {
        return Err(cordis::CordisError::Configuration(
            "invalid Tools config: expected object, {\"tools\": <map>}, empty, or null".into(),
        ));
    };
    if obj.is_empty() {
        return Ok(HashMap::new());
    }
    let map_value = if let Some(tools) = obj.get("tools") {
        if tools.is_null() {
            return Ok(HashMap::new());
        }
        tools.clone()
    } else {
        let mut stripped = config.clone();
        if let Some(map) = stripped.as_object_mut() {
            map.remove("mcps_dir");
        }
        stripped
    };
    if map_value.is_null() || map_value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(HashMap::new());
    }
    serde_json::from_value(map_value).map_err(|error| {
        cordis::CordisError::Configuration(format!("invalid Tools config: {error}"))
    })
}

fn factory_tools(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let map = parse_tool_configs(config)?;
    let mut tool_registry = ToolRegistry::with_config(&map);

    tool_registry.register(Arc::new(Calculator));

    #[cfg(feature = "search-tools")]
    {
        tool_registry.register(Arc::new(crate::search::WebSearch::new()));
        tool_registry.register(Arc::new(crate::web_scrape::WebScrape::new()));
    }

    #[cfg(feature = "postgres")]
    register_connector_tools(ctx, &mut tool_registry);

    #[cfg(feature = "mcp")]
    register_mcp_bridge_tools(config, &mut tool_registry);

    tracing::info!(
        "Tool registry initialized with {} tools",
        tool_registry.enabled_tool_names().len()
    );

    let static_reg = Arc::new(tool_registry);

    #[cfg(any(feature = "postgres", test))]
    let tools = Tools::with_runtime(static_reg, runtime_registry(ctx));
    #[cfg(not(any(feature = "postgres", test)))]
    let tools = Tools::new(static_reg);

    block_on_plugin(ctx, tools)
}

#[cfg(feature = "postgres")]
fn register_connector_tools(ctx: &Arc<cordis::Context>, tool_registry: &mut ToolRegistry) {
    match (
        ares_store::MasterKey::from_env(),
        ctx.get::<ares_store::PostgresClient>(),
    ) {
        (Some(master_key), Some(pg)) => {
            crate::connectors::register_prebuilt_connector_tools(
                tool_registry,
                pg.pool.clone(),
                master_key,
            );
        }
        (Some(_), None) => {
            tracing::warn!("PostgresClient missing; pre-built connector tools are not registered");
        }
        (None, _) => {
            tracing::warn!(
                "FLEET_SECRETS_KEY is not set; pre-built connector tools are not registered"
            );
        }
    }
}

#[cfg(feature = "mcp")]
fn register_mcp_bridge_tools(config: &Value, tool_registry: &mut ToolRegistry) {
    let mcps_dir = config
        .get("mcps_dir")
        .and_then(Value::as_str)
        .unwrap_or("config/mcps");
    let Ok(mcp_reg) = ares_mcp::McpRegistry::from_dir(mcps_dir) else {
        return;
    };
    for client_name in mcp_reg.client_names() {
        if mcp_reg.get_client(&client_name).is_some() {
            crate::mcp_bridge::register_mcp_tools(tool_registry, &client_name);
        }
    }
}

#[cfg(any(feature = "postgres", test))]
fn runtime_registry(ctx: &Arc<cordis::Context>) -> Option<Arc<crate::RuntimeToolRegistry>> {
    #[cfg(feature = "postgres")]
    {
        let pg = ctx.get::<ares_store::PostgresClient>()?;
        let runtime_tool_registry = crate::RuntimeToolRegistry::new(pg.pool.clone());
        if let Err(e) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(runtime_tool_registry.reload())
        }) {
            tracing::warn!("Failed to preload runtime tools on startup: {}", e);
        }
        Some(Arc::new(runtime_tool_registry))
    }
    #[cfg(not(feature = "postgres"))]
    {
        let _ = ctx;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_plugins_registers_exactly_calculator_and_tools() {
        let reg = cordis::PluginRegistry::new();
        register_plugins(&reg);
        let mut names = reg.names();
        names.sort();
        assert_eq!(
            names,
            vec!["CalculatorService".to_string(), "Tools".to_string()]
        );
    }

    #[test]
    fn parse_tool_configs_null_and_empty() {
        assert!(parse_tool_configs(&Value::Null).unwrap().is_empty());
        assert!(parse_tool_configs(&json!({})).unwrap().is_empty());
    }

    #[test]
    fn parse_tool_configs_accepts_tools_wrapper() {
        let map = parse_tool_configs(&json!({
            "tools": {
                "calculator": { "enabled": true }
            },
            "mcps_dir": "config/mcps"
        }))
        .unwrap();
        assert!(map.contains_key("calculator"));
        assert!(map.get("calculator").unwrap().enabled);
    }

    #[test]
    fn parse_tool_configs_flat_map_strips_mcps_dir() {
        let map = parse_tool_configs(&json!({
            "calculator": { "enabled": false },
            "mcps_dir": "elsewhere"
        }))
        .unwrap();
        assert_eq!(map.len(), 1);
        assert!(!map.get("calculator").unwrap().enabled);
    }
}
