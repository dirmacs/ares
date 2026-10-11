//! Loader plugin factories for CalculatorService and Tools.
//!
//! Factories call `cordis::Context::plugin` via block_in_place + block_on.
//! ToolRegistry and RuntimeToolRegistry are not provided as Services.
//! A downstream binary provides [`ExtraStaticTools`] to add its own static
//! tools without rebuilding the registry the factory owns.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::config::ToolConfig;
use crate::registry::ToolRegistry;
use crate::{Calculator, CalculatorConfig, CalculatorService, ProviderWebSearch, Tool, Tools};

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

/// Static tools contributed by a downstream binary.
///
/// Provide this service on the context before the `Tools` plugin loads, for
/// example `ctx.provide(ExtraStaticTools(tools))`. [`factory_tools`] merges
/// the entries into the static registry right after the always-on tools, so
/// runtime tools, connector tools, and the MCP bridge stay intact. With no
/// provider, the factory behaves exactly as before.
pub struct ExtraStaticTools(pub Vec<Arc<dyn Tool>>);

impl cordis::Service for ExtraStaticTools {
    fn name(&self) -> &'static str {
        "extra_static_tools"
    }
}

fn factory_tools(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let map = parse_tool_configs(config)?;
    let mut tool_registry = ToolRegistry::with_config(&map);

    register_always_on_tools(&mut tool_registry);

    if let Some(extra) = ctx.get::<ExtraStaticTools>() {
        for tool in &extra.0 {
            tool_registry.register(Arc::clone(tool));
        }
    }

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

    // The OpenAPPA hook path, when configured. It is inert unless configured:
    // the `appa` key on this entry, or ARES_APPA_CONFIG naming a policy file.
    // A configured path fails loudly rather than starting unguarded — see
    // `appa_gate_from_config` and the EventsService requirement.
    match appa_gate_from_config(config) {
        Ok(Some(gate)) => {
            // The tools.execute event waterfall the policy's audit listeners
            // ride on is bypassed without EventsService, so a configured hook
            // path without it is a configuration fault, not a degraded mode.
            if ctx.get::<cordis::EventsService>().is_none() {
                return Err(cordis::CordisError::Configuration(
                    "the appa hook path is configured but EventsService is not on the \
                     context; cordis-entries.toml must load the events entry before \
                     the tools entry"
                        .to_string(),
                ));
            }
            tracing::info!("OpenAPPA gate attached to the tools funnel");
            Ok(block_on_plugin(ctx, tools.attach_appa(gate))?)
        }
        Ok(None) => block_on_plugin(ctx, tools),
        Err(error) => Err(error),
    }
}

/// The Tools entry config shape of the OpenAPPA hook path.
///
/// ```json
/// { "appa": {
///     "enabled": true,
///     "config": "config/appa.toml",
///     "batteries_dir": "config/appa-batteries",
///     "granted_env": ["ERUKA_CLASSIFIER_TOKEN"],
///     "workspace_root": "/srv/ares",
///     "fence_mode": "workspace_write",
///     "blocklist": [".env", "id_rsa"],
///     "read_tools": ["read_file"],
///     "write_tools": ["write_file"]
/// } }
/// ```
///
/// Every field but `enabled` and `config` defaults. `ARES_APPA_CONFIG` names
/// the policy file when the entry states none; with neither, the hook path
/// stays off.
fn appa_gate_from_config(config: &Value) -> Result<Option<crate::AppaGate>, cordis::CordisError> {
    let refuse = |detail: String| cordis::CordisError::Configuration(format!("appa: {detail}"));
    let section = config.get("appa");
    let path = match section {
        Some(section)
            if section
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            section
                .get("config")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| {
                    refuse("the appa section is enabled but names no config file".to_string())
                })?
        }
        _ => match std::env::var("ARES_APPA_CONFIG") {
            Ok(path) if !path.is_empty() => std::path::PathBuf::from(path),
            _ => return Ok(None),
        },
    };
    let section = section.cloned().unwrap_or(Value::Null);

    let root = std::fs::read_to_string(&path).map_err(|error| {
        refuse(format!(
            "the policy {} could not be read: {error}",
            path.display()
        ))
    })?;

    // Batteries compose under the root document the way a root file's include
    // composes them: every `*.toml` in the batteries directory, sorted, each
    // under its file stem.
    let mut batteries: Vec<appa::config::HostedBattery<'_>> = Vec::new();
    let battery_texts: Vec<(String, String)> =
        match section.get("batteries_dir").and_then(Value::as_str) {
            Some(dir) => {
                let mut entries: Vec<_> = std::fs::read_dir(dir)
                    .map_err(|error| {
                        refuse(format!(
                            "the batteries dir {dir} could not be read: {error}"
                        ))
                    })?
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        entry.path().extension().and_then(|ext| ext.to_str()) == Some("toml")
                    })
                    .map(|entry| {
                        let name = entry
                            .path()
                            .file_stem()
                            .map(|stem| stem.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let text = std::fs::read_to_string(entry.path()).map_err(|error| {
                            refuse(format!("battery {name} could not be read: {error}"))
                        })?;
                        Ok((name, text))
                    })
                    .collect::<Result<_, _>>()?;
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                entries
            }
            None => Vec::new(),
        };
    for (name, text) in &battery_texts {
        batteries.push(appa::config::HostedBattery {
            name,
            policy: text,
            // The runtime never reads a process variable the host did not
            // grant here; the lookup answers only what is listed.
            token_env: &[],
        });
    }

    let granted: Vec<String> = section
        .get("granted_env")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let lookup = move |var: &str| {
        granted
            .iter()
            .any(|name| name == var)
            .then(|| std::env::var(var).ok())
            .flatten()
    };

    let timeout_ms = section
        .get("consult_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(2000);
    let max_body_bytes = section
        .get("max_body_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65_536)
        .min(usize::MAX as u64) as usize;
    let defaults = appa::config::HostDefaults::new(
        std::time::Duration::from_millis(timeout_ms),
        max_body_bytes,
    );

    let fence_mode = match section.get("fence_mode").and_then(Value::as_str) {
        Some("read_only") => crate::fence::FenceMode::ReadOnly,
        Some("full") => crate::fence::FenceMode::Full,
        _ => crate::fence::FenceMode::WorkspaceWrite,
    };
    let workspace_root = section
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let blocklist: Vec<String> = section
        .get("blocklist")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| vec![".env".to_string(), "id_rsa".to_string()]);
    let fence_policy = crate::fence::FencePolicy::new(fence_mode, workspace_root, blocklist);

    let gate = crate::AppaGate::open(&root, &batteries, defaults, lookup, fence_policy)
        .map_err(|error| refuse(error.to_string()))?;
    Ok(Some(gate))
}

fn register_always_on_tools(tool_registry: &mut ToolRegistry) {
    tool_registry.register(Arc::new(Calculator));
    tool_registry.register(Arc::new(ProviderWebSearch));
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

    #[test]
    fn always_on_static_tools_include_provider_web_search() {
        let mut registry = ToolRegistry::new();
        register_always_on_tools(&mut registry);
        assert!(registry.has_tool("calculator"));
        assert!(registry.has_tool("provider_web_search"));
        assert!(!registry.has_tool("web_search"));
        let names: Vec<_> = registry
            .get_tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(names.contains(&"provider_web_search".to_string()));
    }

    #[cfg(feature = "search-tools")]
    #[test]
    fn search_tools_still_registers_daedra_web_search() {
        let mut registry = ToolRegistry::new();
        register_always_on_tools(&mut registry);
        registry.register(Arc::new(crate::search::WebSearch::new()));
        assert_eq!(
            registry.get("provider_web_search").unwrap().name(),
            "provider_web_search"
        );
        assert_eq!(registry.get("web_search").unwrap().name(), "web_search");
    }

    struct SeamTool;

    #[async_trait::async_trait]
    impl Tool for SeamTool {
        fn name(&self) -> &str {
            "seam_probe"
        }

        fn description(&self) -> &str {
            "Tool contributed through ExtraStaticTools"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }

        async fn execute(&self, _args: Value) -> ares_types::types::Result<Value> {
            Ok(json!({ "seam": true }))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extra_static_tools_resolve_through_the_tools_service() {
        let ctx = cordis::Context::new_root();
        ctx.provide(ExtraStaticTools(vec![Arc::new(SeamTool) as Arc<dyn Tool>]));

        factory_tools(&ctx, &Value::Null).expect("factory_tools should load");

        let tools = ctx.get::<Tools>().expect("factory provides Tools");
        let tool = tools
            .resolve(&ctx, "seam_probe")
            .expect("ExtraStaticTools entry should resolve through Tools");
        assert_eq!(tool.name(), "seam_probe");
        assert!(tools.list(&ctx).iter().any(|d| d.name == "seam_probe"));

        // Without a provider the factory still builds the always-on registry.
        let bare = cordis::Context::new_root();
        factory_tools(&bare, &Value::Null).expect("factory_tools should load without extra tools");
        let bare_tools = bare.get::<Tools>().expect("factory provides Tools");
        assert!(bare_tools.resolve(&bare, "calculator").is_some());
    }
}
