use crate::agents::configurable::ConfigurableAgent;
use crate::agents::registry::AgentRegistry;
use crate::types::{AppError, Result};
use crate::utils::toml_config::AgentConfig;
use sqlx::{PgPool, Row};
use std::collections::HashMap;

/// Converts tenant agent JSONB config to the AgentConfig struct used by AgentRegistry.
pub(crate) fn agent_config_from_json(json: &serde_json::Value) -> Result<AgentConfig> {
    let obj = json.as_object().ok_or_else(|| {
        AppError::Configuration("Tenant agent config must be a JSON object".into())
    })?;

    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Configuration(
                "Tenant agent config is missing a valid non-empty 'model'".into(),
            )
        })?
        .to_string();

    let system_prompt = match obj.get("system_prompt") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(AppError::Configuration(
                "Tenant agent config field 'system_prompt' must be a string".into(),
            ));
        }
    };

    let tools = match obj.get("tools") {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(|s| s.to_string()).ok_or_else(|| {
                    AppError::Configuration(
                        "Tenant agent config field 'tools' must be an array of strings".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(AppError::Configuration(
                "Tenant agent config field 'tools' must be an array".into(),
            ));
        }
    };

    let max_tool_iterations = match obj.get("max_tool_iterations") {
        Some(serde_json::Value::Number(value)) => value.as_u64().ok_or_else(|| {
            AppError::Configuration(
                "Tenant agent config field 'max_tool_iterations' must be a non-negative integer"
                    .into(),
            )
        })? as usize,
        Some(serde_json::Value::Null) | None => 5,
        Some(_) => {
            return Err(AppError::Configuration(
                "Tenant agent config field 'max_tool_iterations' must be a number".into(),
            ));
        }
    };

    let parallel_tools = match obj.get("parallel_tools") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::Null) | None => false,
        Some(_) => {
            return Err(AppError::Configuration(
                "Tenant agent config field 'parallel_tools' must be a boolean".into(),
            ));
        }
    };

    Ok(AgentConfig {
        model,
        system_prompt,
        tools,
        max_tool_iterations,
        parallel_tools,
        extra: HashMap::new(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentConfigSource {
    TenantDb,
    Registry,
}

impl AgentConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TenantDb => "tenant_db",
            Self::Registry => "registry",
        }
    }
}

pub struct ResolvedAgent {
    pub agent: ConfigurableAgent,
    pub source: AgentConfigSource,
    pub agent_name: String,
    pub config_version: Option<String>,
}

fn tenant_config_version(config: &serde_json::Value, updated_at: i64) -> String {
    config
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("tenant-db:{}", updated_at))
}

async fn load_tenant_agent_config(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
) -> Result<Option<(AgentConfig, String)>> {
    let row = sqlx::query(
        "SELECT config, enabled, updated_at FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2",
    )
    .bind(tenant_id)
    .bind(agent_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    let Some(row) = row else {
        return Ok(None);
    };

    let enabled: bool = row.get("enabled");
    if !enabled {
        return Err(AppError::NotFound(format!(
            "Agent '{}' is disabled for tenant '{}'",
            agent_name, tenant_id
        )));
    }

    let config_json: serde_json::Value = row.get("config");
    let updated_at: i64 = row.get("updated_at");
    let config_version = tenant_config_version(&config_json, updated_at);
    let agent_config = agent_config_from_json(&config_json)?;

    Ok(Some((agent_config, config_version)))
}

pub async fn resolve_agent_for_tenant(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
) -> Result<ResolvedAgent> {
    if let Some((agent_config, config_version)) =
        load_tenant_agent_config(pool, tenant_id, agent_name).await?
    {
        let agent = agent_registry
            .create_agent_from_config(agent_name, &agent_config)
            .await?;

        return Ok(ResolvedAgent {
            agent,
            source: AgentConfigSource::TenantDb,
            agent_name: agent_name.to_string(),
            config_version: Some(config_version),
        });
    }

    let agent = agent_registry.create_agent(agent_name).await?;
    Ok(ResolvedAgent {
        agent,
        source: AgentConfigSource::Registry,
        agent_name: agent_name.to_string(),
        config_version: None,
    })
}

pub async fn resolve_required_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
) -> Result<ResolvedAgent> {
    let Some((agent_config, config_version)) =
        load_tenant_agent_config(pool, tenant_id, agent_name).await?
    else {
        return Err(AppError::NotFound(format!(
            "Agent '{}' not found for tenant '{}'",
            agent_name, tenant_id
        )));
    };

    let agent = agent_registry
        .create_agent_from_config(agent_name, &agent_config)
        .await?;

    Ok(ResolvedAgent {
        agent,
        source: AgentConfigSource::TenantDb,
        agent_name: agent_name.to_string(),
        config_version: Some(config_version),
    })
}

/// Legacy helper kept for backward compatibility with older callers.
/// New runtime code should use `resolve_agent_for_tenant` or `resolve_required_tenant_agent`.
pub async fn create_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
) -> Option<ConfigurableAgent> {
    match load_tenant_agent_config(pool, tenant_id, agent_name).await {
        Ok(Some((agent_config, _))) => agent_registry
            .create_agent_from_config(agent_name, &agent_config)
            .await
            .ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{agent_config_from_json, tenant_config_version};

    #[test]
    fn tenant_config_requires_model() {
        let err = agent_config_from_json(&serde_json::json!({
            "system_prompt": "hi"
        }))
        .expect_err("missing model should fail");

        assert!(err
            .to_string()
            .contains("missing a valid non-empty 'model'"));
    }

    #[test]
    fn tenant_config_rejects_non_string_tools() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "tools": ["ok", 123]
        }))
        .expect_err("non-string tool should fail");

        assert!(err
            .to_string()
            .contains("'tools' must be an array of strings"));
    }

    #[test]
    fn tenant_config_rejects_non_boolean_parallel_tools() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "parallel_tools": "yes"
        }))
        .expect_err("parallel_tools must be boolean");

        assert!(err
            .to_string()
            .contains("'parallel_tools' must be a boolean"));
    }

    #[test]
    fn tenant_config_version_uses_explicit_version_when_present() {
        let version = tenant_config_version(
            &serde_json::json!({
                "model": "default",
                "version": "fleet-42"
            }),
            123,
        );

        assert_eq!(version, "fleet-42");
    }

    #[test]
    fn tenant_config_version_falls_back_to_updated_at() {
        let version = tenant_config_version(
            &serde_json::json!({
                "model": "default"
            }),
            123,
        );

        assert_eq!(version, "tenant-db:123");
    }
}
