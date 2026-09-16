use crate::configurable::ConfigurableAgent;
use crate::registry::AgentRegistry;
use crate::AgentConfig;
use ares_types::types::{AppError, Result};
use sqlx::{PgPool, Row};
use std::collections::HashMap;

/// Converts tenant agent JSONB config to the AgentConfig struct used by AgentRegistry.
pub fn agent_config_from_json(json: &serde_json::Value) -> Result<AgentConfig> {
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

    let system_prompt = optional_string(obj, "system_prompt")?;

    let tools = string_array_or_empty(obj, "tools")?;

    let max_tool_iterations = optional_usize(obj, "max_tool_iterations")?.unwrap_or(5);

    let parallel_tools = optional_bool(obj, "parallel_tools")?.unwrap_or(false);

    let allowed_tools = string_array_or_empty(obj, "allowed_tools")?;

    let temperature = optional_f32_in_range(obj, "temperature", 0.0, 2.0, "0.0 and 2.0")?;

    let max_tokens = optional_positive_u32(obj, "max_tokens")?;

    let stop = optional_string_list(obj, "stop")?;

    let top_p = optional_f32_in_range(obj, "top_p", 0.0, 1.0, "0.0 and 1.0")?;

    let frequency_penalty =
        optional_f32_in_range(obj, "frequency_penalty", -2.0, 2.0, "-2.0 and 2.0")?;

    let presence_penalty =
        optional_f32_in_range(obj, "presence_penalty", -2.0, 2.0, "-2.0 and 2.0")?;

    Ok(AgentConfig {
        model,
        system_prompt,
        tools,
        max_tool_iterations,
        parallel_tools,
        allowed_tools: if allowed_tools.is_empty() {
            None
        } else {
            Some(allowed_tools)
        },
        compaction_enabled: None,
        temperature,
        max_tokens,
        stop,
        top_p,
        frequency_penalty,
        presence_penalty,
        extra: HashMap::new(),
    })
}

/// JSON object handle as returned by `serde_json::Value::as_object`.
type JsonObject = serde_json::Map<String, serde_json::Value>;

/// Read an optional string field; `null` and absent both mean `None`.
fn optional_string(obj: &JsonObject, field: &str) -> Result<Option<String>> {
    match obj.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a string"
        ))),
    }
}

/// Read an optional array-of-strings field; absent means an empty vec.
fn string_array_or_empty(obj: &JsonObject, field: &str) -> Result<Vec<String>> {
    match obj.get(field) {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(|s| s.to_string()).ok_or_else(|| {
                    AppError::Configuration(format!(
                        "Tenant agent config field '{field}' must be an array of strings"
                    ))
                })
            })
            .collect::<Result<Vec<_>>>(),
        Some(serde_json::Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be an array"
        ))),
    }
}

/// Read an optional non-negative integer field.
fn optional_usize(obj: &JsonObject, field: &str) -> Result<Option<usize>> {
    match obj.get(field) {
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .map(|n| n as usize)
            .ok_or_else(|| {
                AppError::Configuration(format!(
                    "Tenant agent config field '{field}' must be a non-negative integer"
                ))
            })
            .map(Some),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a number"
        ))),
    }
}

/// Read an optional boolean field; `null` and absent both mean `None`.
fn optional_bool(obj: &JsonObject, field: &str) -> Result<Option<bool>> {
    match obj.get(field) {
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a boolean"
        ))),
    }
}

/// Read an optional positive `u32` field.
fn optional_positive_u32(obj: &JsonObject, field: &str) -> Result<Option<u32>> {
    match obj.get(field) {
        Some(serde_json::Value::Number(value)) => {
            let n = value.as_u64().ok_or_else(|| {
                AppError::Configuration(format!(
                    "Tenant agent config field '{field}' must be a positive integer"
                ))
            })?;
            checked_positive_u32(n, field).map(Some)
        }
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a number"
        ))),
    }
}

/// Range check for [`optional_positive_u32`].
fn checked_positive_u32(n: u64, field: &str) -> Result<u32> {
    if n == 0 || n > u32::MAX as u64 {
        return Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a positive integer"
        )));
    }
    Ok(n as u32)
}

/// Read an optional `f32` field constrained to a finite `[min, max]` range.
fn optional_f32_in_range(
    obj: &JsonObject,
    field: &str,
    min: f32,
    max: f32,
    range_text: &str,
) -> Result<Option<f32>> {
    match obj.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(value @ serde_json::Value::Number(_)) => {
            f32_field_value(value, field, min, max, range_text).map(Some)
        }
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a number"
        ))),
    }
}

/// Numeric check for [`optional_f32_in_range`]; keeps the original error
/// precedence: a non-convertible number first, then the range check.
fn f32_field_value(
    value: &serde_json::Value,
    field: &str,
    min: f32,
    max: f32,
    range_text: &str,
) -> Result<f32> {
    let f = value.as_f64().ok_or_else(|| {
        AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a number"
        ))
    })? as f32;
    if !f.is_finite() || f < min || f > max {
        return Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be between {range_text}"
        )));
    }
    Ok(f)
}

/// Read an optional string-or-string-array field.
fn optional_string_list(obj: &JsonObject, field: &str) -> Result<Option<Vec<String>>> {
    match obj.get(field) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(vec![value.clone()])),
        Some(serde_json::Value::Array(values)) => string_entries(values, field).map(Some),
        Some(_) => Err(AppError::Configuration(format!(
            "Tenant agent config field '{field}' must be a string or array of strings"
        ))),
    }
}

/// Convert a JSON array of strings into `Vec<String>`; any other entry fails.
fn string_entries(values: &[serde_json::Value], field: &str) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for entry in values {
        match entry {
            serde_json::Value::String(s) => out.push(s.clone()),
            _ => {
                return Err(AppError::Configuration(format!(
                    "Tenant agent config field '{field}' must be a string or array of strings"
                )));
            }
        }
    }
    Ok(out)
}

pub(crate) fn tenant_agent_disabled_error(agent_name: &str, tenant_id: &str) -> AppError {
    AppError::NotFound(format!(
        "Agent '{}' is disabled for tenant '{}'",
        agent_name, tenant_id
    ))
}

pub(crate) fn tenant_agent_not_found_error(agent_name: &str, tenant_id: &str) -> AppError {
    AppError::NotFound(format!(
        "Agent '{}' not found for tenant '{}'",
        agent_name, tenant_id
    ))
}

pub(crate) fn legacy_create_should_use_tenant_config(
    load_result: &Result<Option<(AgentConfig, String, serde_json::Value)>>,
) -> bool {
    matches!(load_result, Ok(Some(_)))
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
    pub config: Option<serde_json::Value>,
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
) -> Result<Option<(AgentConfig, String, serde_json::Value)>> {
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
        return Err(tenant_agent_disabled_error(agent_name, tenant_id));
    }

    let config_json: serde_json::Value = row.get("config");
    let updated_at: i64 = row.get("updated_at");
    let config_version = tenant_config_version(&config_json, updated_at);
    let agent_config = agent_config_from_json(&config_json)?;

    Ok(Some((agent_config, config_version, config_json)))
}

pub(crate) async fn resolve_agent_for_tenant(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
    fleet_secrets: &ares_store::FleetSecrets,
) -> Result<ResolvedAgent> {
    if let Some((agent_config, config_version, config_json)) =
        load_tenant_agent_config(pool, tenant_id, agent_name).await?
    {
        let agent = agent_registry
            .create_agent_from_config_with_fallbacks(
                agent_name,
                &agent_config,
                tenant_id,
                pool,
                fleet_secrets,
            )
            .await?;

        return Ok(ResolvedAgent {
            agent,
            source: AgentConfigSource::TenantDb,
            agent_name: agent_name.to_string(),
            config_version: Some(config_version),
            config: Some(config_json),
        });
    }

    let registry_config = agent_registry.get_config_any(agent_name).ok_or_else(|| {
        AppError::Configuration(format!(
            "Agent '{}' not found in TOML or TOON configuration",
            agent_name
        ))
    })?;
    let agent = agent_registry
        .create_agent_from_config_with_fallbacks(
            agent_name,
            &registry_config,
            tenant_id,
            pool,
            fleet_secrets,
        )
        .await?;
    Ok(ResolvedAgent {
        agent,
        source: AgentConfigSource::Registry,
        agent_name: agent_name.to_string(),
        config_version: None,
        config: None,
    })
}

/// Tenant id from Cordis isolate (`Execute`) then TenantContext intercept.
pub fn tenant_id_from_agent_ctx(ctx: &std::sync::Arc<cordis::Context>) -> Option<String> {
    let id = crate::resolver::user_id_from_ctx(ctx, "");
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

pub async fn resolve_required_tenant_agent_from_ctx(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    ctx: &std::sync::Arc<cordis::Context>,
    agent_name: &str,
    fleet_secrets: &ares_store::FleetSecrets,
) -> Result<ResolvedAgent> {
    let tenant_id = tenant_id_from_agent_ctx(ctx)
        .ok_or_else(|| AppError::Auth("Missing tenant context".to_string()))?;
    resolve_required_tenant_agent(pool, agent_registry, &tenant_id, agent_name, fleet_secrets).await
}

pub(crate) async fn resolve_required_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
    fleet_secrets: &ares_store::FleetSecrets,
) -> Result<ResolvedAgent> {
    let Some((agent_config, config_version, config_json)) =
        load_tenant_agent_config(pool, tenant_id, agent_name).await?
    else {
        return Err(tenant_agent_not_found_error(agent_name, tenant_id));
    };

    let agent = agent_registry
        .create_agent_from_config_with_fallbacks(
            agent_name,
            &agent_config,
            tenant_id,
            pool,
            fleet_secrets,
        )
        .await?;

    Ok(ResolvedAgent {
        agent,
        source: AgentConfigSource::TenantDb,
        agent_name: agent_name.to_string(),
        config_version: Some(config_version),
        config: Some(config_json),
    })
}

/// Construct a tenant-configured agent for legacy callers.
///
/// Runtime execution goes through `Execute::run`; this helper remains only for
/// compatibility with storage-focused callers and tests.
pub async fn create_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
    fleet_secrets: &ares_store::FleetSecrets,
) -> Option<ConfigurableAgent> {
    let load_result = load_tenant_agent_config(pool, tenant_id, agent_name).await;
    if !legacy_create_should_use_tenant_config(&load_result) {
        return None;
    }
    let (agent_config, _, _) = load_result
        .ok()
        .and_then(|loaded| loaded)
        .expect("legacy_create_should_use_tenant_config implies Ok(Some(_))");
    agent_registry
        .create_agent_from_config_with_fallbacks(
            agent_name,
            &agent_config,
            tenant_id,
            pool,
            fleet_secrets,
        )
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::{
        agent_config_from_json, legacy_create_should_use_tenant_config,
        tenant_agent_disabled_error, tenant_agent_not_found_error, tenant_config_version,
        tenant_id_from_agent_ctx, AgentConfigSource,
    };
    use crate::AgentConfig;
    use ares_types::types::{AppError, Result as AresResult};

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

    #[test]
    fn tenant_config_parses_full_valid_config() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "system_prompt": "You are helpful",
            "tools": ["calculator", "search"],
            "max_tool_iterations": 7,
            "parallel_tools": true
        }))
        .expect("valid config");

        assert_eq!(config.model, "default");
        assert_eq!(config.system_prompt.as_deref(), Some("You are helpful"));
        assert_eq!(
            config.tools,
            vec!["calculator".to_string(), "search".to_string()]
        );
        assert_eq!(config.max_tool_iterations, 7);
        assert!(config.parallel_tools);
    }

    #[test]
    fn tenant_config_rejects_non_object_root() {
        let err = agent_config_from_json(&serde_json::json!("not-an-object"))
            .expect_err("root must be object");
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[test]
    fn tenant_config_rejects_empty_model() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "   "
        }))
        .expect_err("empty model");
        assert!(err
            .to_string()
            .contains("missing a valid non-empty 'model'"));
    }

    #[test]
    fn tenant_config_allows_null_optional_fields() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "system_prompt": null,
            "tools": null,
            "max_tool_iterations": null,
            "parallel_tools": null
        }))
        .expect("null optional fields");

        assert!(config.system_prompt.is_none());
        assert!(config.tools.is_empty());
        assert_eq!(config.max_tool_iterations, 5);
        assert!(!config.parallel_tools);
    }

    #[test]
    fn tenant_config_rejects_invalid_system_prompt_type() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "system_prompt": 42
        }))
        .expect_err("system_prompt must be string");
        assert!(err.to_string().contains("'system_prompt' must be a string"));
    }

    #[test]
    fn tenant_config_rejects_invalid_tools_container() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "tools": "calculator"
        }))
        .expect_err("tools must be array");
        assert!(err.to_string().contains("'tools' must be an array"));
    }

    #[test]
    fn tenant_config_rejects_invalid_max_tool_iterations() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "max_tool_iterations": -1
        }))
        .expect_err("negative max_tool_iterations");
        assert!(err
            .to_string()
            .contains("'max_tool_iterations' must be a non-negative integer"));

        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "max_tool_iterations": "five"
        }))
        .expect_err("string max_tool_iterations");
        assert!(err
            .to_string()
            .contains("'max_tool_iterations' must be a number"));
    }

    #[test]
    fn tenant_config_trims_model_name() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "  custom-model  "
        }))
        .expect("trimmed model");
        assert_eq!(config.model, "custom-model");
    }

    #[test]
    fn agent_config_source_as_str() {
        assert_eq!(AgentConfigSource::TenantDb.as_str(), "tenant_db");
        assert_eq!(AgentConfigSource::Registry.as_str(), "registry");
    }

    #[test]
    fn tenant_config_version_empty_string_falls_back_to_updated_at() {
        let version = tenant_config_version(&serde_json::json!({ "version": "" }), 99);
        assert_eq!(version, "tenant-db:99");
    }

    #[test]
    fn tenant_config_version_whitespace_only_falls_back_to_updated_at() {
        let version = tenant_config_version(&serde_json::json!({ "version": "   " }), 77);
        assert_eq!(version, "tenant-db:77");
    }

    #[test]
    fn tenant_config_version_trims_explicit_version() {
        let version = tenant_config_version(&serde_json::json!({ "version": "  fleet-9  " }), 1);
        assert_eq!(version, "fleet-9");
    }

    #[test]
    fn tenant_config_minimal_json_uses_defaults() {
        let config = agent_config_from_json(&serde_json::json!({ "model": "default" }))
            .expect("model-only config");

        assert_eq!(config.model, "default");
        assert!(config.system_prompt.is_none());
        assert!(config.tools.is_empty());
        assert_eq!(config.max_tool_iterations, 5);
        assert!(!config.parallel_tools);
        assert!(config.temperature.is_none());
        assert!(config.max_tokens.is_none());
        assert!(config.stop.is_none());
        assert!(config.top_p.is_none());
        assert!(config.frequency_penalty.is_none());
        assert!(config.presence_penalty.is_none());
        assert!(config.extra.is_empty());
    }

    #[test]
    fn tenant_config_accepts_empty_system_prompt_string() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "system_prompt": ""
        }))
        .expect("empty system_prompt string");

        assert_eq!(config.system_prompt.as_deref(), Some(""));
    }

    #[test]
    fn tenant_config_accepts_empty_tools_array() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "tools": []
        }))
        .expect("empty tools");

        assert!(config.tools.is_empty());
    }

    #[test]
    fn tenant_config_accepts_zero_max_tool_iterations() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "max_tool_iterations": 0
        }))
        .expect("zero iterations");

        assert_eq!(config.max_tool_iterations, 0);
    }

    #[test]
    fn tenant_config_ignores_unknown_top_level_fields() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "memory": { "enabled": true },
            "custom_flag": true
        }))
        .expect("unknown fields ignored");

        assert_eq!(config.model, "default");
        assert!(config.extra.is_empty());
    }

    #[test]
    fn tenant_config_accepts_gen_params() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "temperature": 1.5,
            "max_tokens": 1024,
            "stop": ["END", "STOP"],
            "top_p": 0.9,
            "frequency_penalty": 0.5,
            "presence_penalty": -0.5
        }))
        .expect("gen params accept");

        assert_eq!(config.temperature, Some(1.5));
        assert_eq!(config.max_tokens, Some(1024));
        assert_eq!(
            config.stop,
            Some(vec!["END".to_string(), "STOP".to_string()])
        );
        assert_eq!(config.top_p, Some(0.9));
        assert_eq!(config.frequency_penalty, Some(0.5));
        assert_eq!(config.presence_penalty, Some(-0.5));
    }

    #[test]
    fn tenant_config_stop_normalizes_string_and_array() {
        let from_string = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "stop": "END"
        }))
        .expect("stop string");
        assert_eq!(from_string.stop, Some(vec!["END".to_string()]));

        let from_array = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "stop": ["A", "B"]
        }))
        .expect("stop array");
        assert_eq!(
            from_array.stop,
            Some(vec!["A".to_string(), "B".to_string()])
        );

        let missing = agent_config_from_json(&serde_json::json!({
            "model": "default"
        }))
        .expect("stop missing");
        assert!(missing.stop.is_none());
    }

    #[test]
    fn tenant_config_null_gen_params_default_to_none() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "temperature": null,
            "max_tokens": null,
            "stop": null,
            "top_p": null,
            "frequency_penalty": null,
            "presence_penalty": null
        }))
        .expect("null gen params");

        assert!(config.temperature.is_none());
        assert!(config.max_tokens.is_none());
        assert!(config.stop.is_none());
        assert!(config.top_p.is_none());
        assert!(config.frequency_penalty.is_none());
        assert!(config.presence_penalty.is_none());
    }

    #[test]
    fn tenant_config_rejects_out_of_range_gen_params() {
        for payload in [
            serde_json::json!({"model": "default", "temperature": -0.1}),
            serde_json::json!({"model": "default", "temperature": 2.1}),
            serde_json::json!({"model": "default", "temperature": "hot"}),
            serde_json::json!({"model": "default", "max_tokens": 0}),
            serde_json::json!({"model": "default", "max_tokens": "many"}),
            serde_json::json!({"model": "default", "top_p": -0.1}),
            serde_json::json!({"model": "default", "top_p": 1.1}),
            serde_json::json!({"model": "default", "stop": 42}),
            serde_json::json!({"model": "default", "stop": ["ok", 7]}),
            serde_json::json!({"model": "default", "frequency_penalty": 2.5}),
            serde_json::json!({"model": "default", "presence_penalty": -2.5}),
        ] {
            agent_config_from_json(&payload).expect_err("out-of-range gen param");
        }
    }

    #[test]
    fn agent_config_source_debug_clone_partial_eq() {
        let tenant = AgentConfigSource::TenantDb;
        let registry = AgentConfigSource::Registry;
        assert_eq!(tenant, tenant);
        assert_ne!(tenant, registry);
        assert_eq!(format!("{:?}", tenant), "TenantDb");
        let copied = tenant;
        assert_eq!(copied.as_str(), "tenant_db");
    }

    #[test]
    fn agent_config_from_json_roundtrips_through_serde_json() {
        let config = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "system_prompt": "roundtrip",
            "tools": ["search"],
            "max_tool_iterations": 3,
            "parallel_tools": true
        }))
        .expect("parse config");

        let encoded = serde_json::to_string(&config).expect("encode");
        let decoded: AgentConfig = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded.model, "default");
        assert_eq!(decoded.system_prompt.as_deref(), Some("roundtrip"));
        assert_eq!(decoded.tools, vec!["search".to_string()]);
        assert_eq!(decoded.max_tool_iterations, 3);
        assert!(decoded.parallel_tools);
    }

    #[test]
    fn tenant_agent_disabled_error_is_not_found() {
        let err = tenant_agent_disabled_error("product", "tenant-1");
        assert!(matches!(err, AppError::NotFound(_)));
        assert!(err.to_string().contains("disabled"));
        assert!(err.to_string().contains("product"));
        assert!(err.to_string().contains("tenant-1"));
    }

    #[test]
    fn tenant_agent_not_found_error_is_not_found() {
        let err = tenant_agent_not_found_error("product", "tenant-1");
        assert!(matches!(err, AppError::NotFound(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn legacy_create_should_use_tenant_config_only_for_ok_some() {
        let ok_some: AresResult<Option<(AgentConfig, String, serde_json::Value)>> = Ok(Some((
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 5,
                parallel_tools: false,
                compaction_enabled: None,
                temperature: None,
                max_tokens: None,
                stop: None,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
                extra: std::collections::HashMap::new(),
                allowed_tools: None,
            },
            "v1".to_string(),
            serde_json::Value::Null,
        )));
        assert!(legacy_create_should_use_tenant_config(&ok_some));

        let ok_none: AresResult<Option<(AgentConfig, String, serde_json::Value)>> = Ok(None);
        assert!(!legacy_create_should_use_tenant_config(&ok_none));

        let err_load: AresResult<Option<(AgentConfig, String, serde_json::Value)>> =
            Err(AppError::Database("x".into()));
        assert!(!legacy_create_should_use_tenant_config(&err_load));
    }

    #[test]
    fn tenant_config_rejects_non_integer_max_tool_iterations_float() {
        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "max_tool_iterations": 1.5
        }))
        .expect_err("float max_tool_iterations");
        assert!(err
            .to_string()
            .contains("'max_tool_iterations' must be a non-negative integer"));
    }

    #[test]
    fn tenant_id_from_agent_ctx_reads_intercept() {
        use ares_types::models::{TenantContext, TenantTier};

        let root = cordis::Context::new_root();
        assert_eq!(tenant_id_from_agent_ctx(&root), None);

        let ctx = root.with_intercept(TenantContext::new("acme".into(), TenantTier::Pro));
        assert_eq!(tenant_id_from_agent_ctx(&ctx).as_deref(), Some("acme"));
    }

    #[test]
    fn tenant_id_from_agent_ctx_isolate_wins_over_intercept() {
        use ares_types::models::{TenantContext, TenantTier};

        let root = cordis::Context::new_root();
        let intercepted =
            root.with_intercept(TenantContext::new("from-intercept".into(), TenantTier::Pro));
        let isolated = crate::tenant_scope(&intercepted, "from-isolate");
        assert_eq!(
            tenant_id_from_agent_ctx(&isolated).as_deref(),
            Some("from-isolate")
        );
    }

    #[cfg(feature = "postgres")]
    mod postgres_integration {
        use super::super::{
            agent_config_from_json, create_tenant_agent, resolve_agent_for_tenant,
            resolve_required_tenant_agent, AgentConfigSource,
        };
        use crate::registry::AgentRegistry;
        use crate::Agent;
        use crate::AgentConfig;
        use ares_llm::ProviderRegistry;
        use ares_llm::{ModelConfig, ProviderConfig};
        use ares_store::tenant_agents::{
            create_tenant_agent as db_create_tenant_agent, update_tenant_agent,
            CreateTenantAgentRequest, UpdateTenantAgentRequest,
        };
        use ares_store::tenant_allowlist::TenantAllowlistStore;
        use ares_tools::{Tool, Tools};
        use ares_types::types::{AgentContext, AppError};
        use axum::{routing::post, Json, Router};
        use serde_json::{json, Value};
        use sqlx::PgPool;
        use std::collections::HashMap;
        use std::sync::Arc;

        async fn test_pool() -> PgPool {
            ares_test_support::pool().await
        }

        fn unique_id(prefix: &str) -> String {
            format!("{}-{}", prefix, uuid::Uuid::new_v4())
        }

        async fn fake_ollama_chat(Json(payload): Json<Value>) -> Json<Value> {
            let system_prompt = payload["messages"]
                .as_array()
                .and_then(|messages| {
                    messages.iter().find_map(|message| {
                        (message.get("role").and_then(Value::as_str) == Some("system"))
                            .then(|| message.get("content").and_then(Value::as_str))
                            .flatten()
                    })
                })
                .unwrap_or("missing-system-prompt");

            Json(json!({
                "model": payload["model"].as_str().unwrap_or("test-model"),
                "created_at": "2026-05-21T00:00:00Z",
                "message": {
                    "role": "assistant",
                    "content": format!("SYSTEM_PROMPT={}", system_prompt)
                },
                "done": true,
                "total_duration": 1,
                "load_duration": 1,
                "prompt_eval_count": 1,
                "prompt_eval_duration": 1,
                "eval_count": 1,
                "eval_duration": 1
            }))
        }

        async fn spawn_mock_ollama_server() -> String {
            let app = Router::new().route("/api/chat", post(fake_ollama_chat));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock ollama");
            let addr = listener.local_addr().expect("mock ollama addr");

            tokio::spawn(async move {
                axum::serve(listener, app).await.expect("serve mock ollama");
            });

            format!("http://{}", addr)
        }

        fn registry_with_product(mock_ollama_url: &str) -> AgentRegistry {
            let mut provider_registry = ProviderRegistry::new();
            provider_registry.register_provider(
                "ollama-local",
                ProviderConfig::Ollama {
                    api_key_env: "OLLAMA_API_KEY".to_string(),
                    base_url: mock_ollama_url.to_string(),
                    default_model: "mock-model".to_string(),
                },
            );
            provider_registry.register_model(
                "default",
                ModelConfig {
                    provider: "ollama-local".to_string(),
                    model: "mock-model".to_string(),
                    temperature: 0.0,
                    max_tokens: 512,
                },
            );

            let mut registry = AgentRegistry::new(
                Arc::new(provider_registry),
                Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
            );
            registry.register(
                "product",
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("registry-product-prompt".to_string()),
                    tools: vec![],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    compaction_enabled: None,
                    temperature: None,
                    max_tokens: None,
                    stop: None,
                    top_p: None,
                    frequency_penalty: None,
                    presence_penalty: None,
                    extra: HashMap::new(),
                    allowed_tools: None,
                },
            );
            registry
        }

        async fn allow_mock_model(pool: &PgPool, tenant_id: &str) {
            TenantAllowlistStore::new(pool)
                .allow_model(tenant_id, "mock-model")
                .await
                .expect("allow mock model");
        }

        async fn insert_tenant_agent(
            pool: &PgPool,
            tenant_id: &str,
            agent_name: &str,
            system_prompt: &str,
        ) {
            db_create_tenant_agent(
                pool,
                tenant_id,
                CreateTenantAgentRequest {
                    agent_name: agent_name.to_string(),
                    display_name: format!("{agent_name} display"),
                    description: Some(format!("{agent_name} description")),
                    config: json!({
                        "model": "default",
                        "system_prompt": system_prompt,
                        "tools": [],
                        "max_tool_iterations": 5,
                        "parallel_tools": false
                    }),
                },
            )
            .await
            .expect("insert tenant agent row");
        }

        fn test_agent_context() -> AgentContext {
            AgentContext {
                user_id: "user-1".to_string(),
                session_id: "session-1".to_string(),
                conversation_history: vec![],
                user_memory: None,
            }
        }

        #[tokio::test]
        async fn create_tenant_agent_returns_none_without_db_row() {
            let pool = test_pool().await;
            let registry = registry_with_product("http://127.0.0.1:9");
            let tenant_id = unique_id("missing-row");

            let agent = create_tenant_agent(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await;
            assert!(agent.is_none());
        }

        #[tokio::test]
        async fn create_tenant_agent_builds_agent_from_tenant_config() {
            let mock_ollama = spawn_mock_ollama_server().await;
            let pool = test_pool().await;
            let registry = registry_with_product(&mock_ollama);
            let tenant_id = unique_id("create-legacy");
            allow_mock_model(&pool, &tenant_id).await;
            insert_tenant_agent(&pool, &tenant_id, "product", "tenant-create-prompt").await;

            let agent = create_tenant_agent(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            .expect("tenant row should produce an agent");

            assert_eq!(agent.system_prompt(), "tenant-create-prompt");
        }

        #[tokio::test]
        async fn resolve_agent_for_tenant_prefers_tenant_db_config() {
            let mock_ollama = spawn_mock_ollama_server().await;
            let pool = test_pool().await;
            let registry = registry_with_product(&mock_ollama);
            let tenant_id = unique_id("tenant-db-wins");
            allow_mock_model(&pool, &tenant_id).await;
            insert_tenant_agent(&pool, &tenant_id, "product", "tenant-db-prompt").await;

            let resolved = resolve_agent_for_tenant(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            .expect("resolve tenant agent");

            assert_eq!(resolved.source, AgentConfigSource::TenantDb);
            assert_eq!(resolved.agent_name, "product");
            assert_eq!(resolved.agent.system_prompt(), "tenant-db-prompt");
            assert!(resolved.config_version.is_some());
        }

        #[tokio::test]
        async fn resolve_agent_for_tenant_falls_back_to_registry() {
            let mock_ollama = spawn_mock_ollama_server().await;
            let pool = test_pool().await;
            let registry = registry_with_product(&mock_ollama);
            let tenant_id = unique_id("registry-fallback");
            allow_mock_model(&pool, &tenant_id).await;

            let resolved = resolve_agent_for_tenant(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            .expect("resolve registry agent");

            assert_eq!(resolved.source, AgentConfigSource::Registry);
            assert!(resolved.config_version.is_none());
            assert_eq!(resolved.agent.system_prompt(), "registry-product-prompt");
            assert!(matches!(
                resolved.agent.allowed_tools(),
                Some(tools) if tools.is_empty()
            ));
        }

        #[tokio::test]
        async fn resolve_agent_for_tenant_rejects_unallowed_fallback_model() {
            let mock_ollama = spawn_mock_ollama_server().await;
            let pool = test_pool().await;
            let tenant_id = unique_id("fallback-deny");
            allow_mock_model(&pool, &tenant_id).await;

            let mut provider_registry = ProviderRegistry::new();
            provider_registry.register_provider(
                "ollama-primary",
                ProviderConfig::Ollama {
                    api_key_env: "OLLAMA_API_KEY".to_string(),
                    base_url: mock_ollama.clone(),
                    default_model: "mock-model".to_string(),
                },
            );
            provider_registry.register_provider(
                "ollama-fallback",
                ProviderConfig::Ollama {
                    api_key_env: "OLLAMA_API_KEY".to_string(),
                    base_url: mock_ollama,
                    default_model: "fallback-model".to_string(),
                },
            );
            provider_registry.register_model(
                "default",
                ModelConfig {
                    provider: "ollama-primary".to_string(),
                    model: "mock-model".to_string(),
                    temperature: 0.0,
                    max_tokens: 512,
                },
            );

            let mut registry = AgentRegistry::new(
                Arc::new(provider_registry),
                Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
            );
            registry.register(
                "product",
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("registry-product-prompt".to_string()),
                    tools: vec![],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    compaction_enabled: None,
                    temperature: None,
                    max_tokens: None,
                    stop: None,
                    top_p: None,
                    frequency_penalty: None,
                    presence_penalty: None,
                    extra: HashMap::new(),
                    allowed_tools: None,
                },
            );

            let mut overrides = HashMap::new();
            overrides.insert(
                "ollama-primary".to_string(),
                ares_store::ProviderOverride {
                    fallback_providers: vec!["ollama-fallback".to_string()],
                    ..Default::default()
                },
            );
            let fleet_secrets = ares_store::FleetSecrets::from_providers(overrides);

            let err = match resolve_agent_for_tenant(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &fleet_secrets,
            )
            .await
            {
                Err(err) => err,
                Ok(_) => panic!("unallowed fallback model should fail"),
            };

            assert!(matches!(err, AppError::Auth(_)));
            assert!(err.to_string().contains("fallback-model"));
        }

        #[tokio::test]
        async fn resolve_required_tenant_agent_errors_when_row_missing() {
            let pool = test_pool().await;
            let registry = registry_with_product("http://127.0.0.1:9");
            let tenant_id = unique_id("required-missing");

            let err = match resolve_required_tenant_agent(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            {
                Err(err) => err,
                Ok(_) => panic!("missing tenant row should fail"),
            };

            assert!(matches!(err, AppError::NotFound(_)));
            assert!(err.to_string().contains("not found"));
        }

        #[tokio::test]
        async fn resolved_agent_execute_uses_tenant_system_prompt() {
            let mock_ollama = spawn_mock_ollama_server().await;
            let pool = test_pool().await;
            let registry = registry_with_product(&mock_ollama);
            let tenant_id = unique_id("execute-flow");
            allow_mock_model(&pool, &tenant_id).await;
            insert_tenant_agent(&pool, &tenant_id, "product", "tenant-execute-prompt").await;

            let resolved = resolve_agent_for_tenant(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            .expect("resolve for execution");

            let response = resolved
                .agent
                .execute("hello", &test_agent_context())
                .await
                .expect("execute resolved tenant agent");

            assert!(response
                .content
                .contains("SYSTEM_PROMPT=tenant-execute-prompt"));
        }

        #[tokio::test]
        async fn resolve_agent_for_tenant_errors_when_agent_disabled() {
            let pool = test_pool().await;
            let registry = registry_with_product("http://127.0.0.1:9");
            let tenant_id = unique_id("disabled-agent");
            insert_tenant_agent(&pool, &tenant_id, "product", "disabled-prompt").await;

            update_tenant_agent(
                &pool,
                &tenant_id,
                "product",
                UpdateTenantAgentRequest {
                    display_name: None,
                    description: None,
                    config: None,
                    enabled: Some(false),
                },
            )
            .await
            .expect("disable tenant agent");

            let err = match resolve_agent_for_tenant(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            {
                Err(err) => err,
                Ok(_) => panic!("disabled tenant agent should not resolve"),
            };

            assert!(matches!(err, AppError::NotFound(_)));
            assert!(err.to_string().contains("disabled"));
        }

        #[tokio::test]
        async fn resolve_agent_for_tenant_errors_on_invalid_tenant_config() {
            let pool = test_pool().await;
            let registry = registry_with_product("http://127.0.0.1:9");
            let tenant_id = unique_id("invalid-config");

            // Insert invalid config directly via SQL to bypass validation
            let id = format!("{}-invalid", tenant_id);
            let now_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            sqlx::query(
                r#"
                INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)
                "#,
            )
            .bind(&id)
            .bind(&tenant_id)
            .bind("product")
            .bind("Broken")
            .bind(Option::<String>::None)
            .bind(serde_json::json!({ "model": "default", "parallel_tools": "yes" }))
            .bind(now_ts)
            .execute(&pool)
            .await
            .expect("insert invalid tenant config via raw SQL");
            let err = match resolve_agent_for_tenant(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await
            {
                Err(err) => err,
                Ok(_) => panic!("invalid tenant config should fail"),
            };

            assert!(matches!(err, AppError::Configuration(_)));
        }

        #[tokio::test]
        async fn create_tenant_agent_returns_none_on_invalid_config() {
            let pool = test_pool().await;
            let registry = registry_with_product("http://127.0.0.1:9");
            let tenant_id = unique_id("legacy-invalid");

            // Insert invalid config directly via SQL to bypass validation
            let id = format!("{}-invalid", tenant_id);
            let now_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            sqlx::query(
                r#"
                INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)
                "#,
            )
            .bind(&id)
            .bind(&tenant_id)
            .bind("product")
            .bind("Broken")
            .bind(Option::<String>::None)
            .bind(serde_json::json!({ "model": "" }))
            .bind(now_ts)
            .execute(&pool)
            .await
            .expect("insert empty-model tenant config via raw SQL");

            let agent = create_tenant_agent(
                &pool,
                &registry,
                &tenant_id,
                "product",
                &ares_store::FleetSecrets::new(),
            )
            .await;
            assert!(agent.is_none());
        }
        #[test]
        fn agent_config_from_json_matches_tenant_db_shape() {
            let config = agent_config_from_json(&json!({
                "model": "default",
                "system_prompt": "tenant shape",
                "tools": [],
                "max_tool_iterations": 5,
                "parallel_tools": false
            }))
            .expect("tenant db config shape");

            assert_eq!(config.model, "default");
            assert_eq!(config.system_prompt.as_deref(), Some("tenant shape"));
        }
    }
}
