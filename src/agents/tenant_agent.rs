use crate::agents::configurable::ConfigurableAgent;
use crate::agents::registry::AgentRegistry;
use crate::utils::toml_config::AgentConfig;
use sqlx::{PgPool, Row};
use std::collections::HashMap;

/// Converts tenant agent JSONB config to the AgentConfig struct used by AgentRegistry.
fn json_to_agent_config(json: &serde_json::Value) -> AgentConfig {
    AgentConfig {
        model: json["model"].as_str().unwrap_or("fast").to_string(),
        system_prompt: json["system_prompt"].as_str().map(|s| s.to_string()),
        tools: json["tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        max_tool_iterations: json["max_tool_iterations"].as_u64().unwrap_or(5) as usize,
        parallel_tools: json["parallel_tools"].as_bool().unwrap_or(false),
        extra: HashMap::new(),
    }
}

/// Loads a tenant's agent config from DB and creates a ready-to-execute ConfigurableAgent.
/// Returns None if agent not found or disabled.
pub async fn create_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
) -> Option<ConfigurableAgent> {
    let row = sqlx::query(
        "SELECT config FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2 AND enabled = true"
    )
    .bind(tenant_id)
    .bind(agent_name)
    .fetch_optional(pool)
    .await
    .ok()??;

    let config_json: serde_json::Value = row.get("config");
    let agent_config = json_to_agent_config(&config_json);

    agent_registry
        .create_agent_from_config(agent_name, &agent_config)
        .await
        .ok()
}
