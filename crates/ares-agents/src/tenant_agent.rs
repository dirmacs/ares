use crate::configurable::ConfigurableAgent;
use crate::registry::AgentRegistry;
use ares_types::types::{AppError, Result};
use ares_config::toml_config::AgentConfig;
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

    let allowed_tools = match obj.get("allowed_tools") {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(|s| s.to_string()).ok_or_else(|| {
                    AppError::Configuration(
                        "Tenant agent config field 'allowed_tools' must be an array of strings".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(AppError::Configuration(
                "Tenant agent config field 'allowed_tools' must be an array".into(),
            ));
        }
    };

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
        extra: HashMap::new(),
    })
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

pub async fn resolve_agent_for_tenant(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
    fleet_secrets: &ares_config::fleet_secrets::FleetSecrets,
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

    let agent = agent_registry.create_agent(agent_name).await?;
    Ok(ResolvedAgent {
        agent,
        source: AgentConfigSource::Registry,
        agent_name: agent_name.to_string(),
        config_version: None,
        config: None,
    })
}

pub async fn resolve_required_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
    fleet_secrets: &ares_config::fleet_secrets::FleetSecrets,
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

/// Legacy helper kept for backward compatibility with older callers.
/// New runtime code should use `resolve_agent_for_tenant` or `resolve_required_tenant_agent`.
pub async fn create_tenant_agent(
    pool: &PgPool,
    agent_registry: &AgentRegistry,
    tenant_id: &str,
    agent_name: &str,
    fleet_secrets: &ares_config::fleet_secrets::FleetSecrets,
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
        AgentConfigSource,
    };
    use ares_config::toml_config::AgentConfig;
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
        assert_eq!(config.tools, vec!["calculator".to_string(), "search".to_string()]);
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
        assert!(err.to_string().contains("missing a valid non-empty 'model'"));
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
        assert!(err.to_string().contains("'max_tool_iterations' must be a non-negative integer"));

        let err = agent_config_from_json(&serde_json::json!({
            "model": "default",
            "max_tool_iterations": "five"
        }))
        .expect_err("string max_tool_iterations");
        assert!(err.to_string().contains("'max_tool_iterations' must be a number"));
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
        let version = tenant_config_version(
            &serde_json::json!({ "version": "" }),
            99,
        );
        assert_eq!(version, "tenant-db:99");
    }

    #[test]
    fn tenant_config_version_whitespace_only_falls_back_to_updated_at() {
        let version = tenant_config_version(
            &serde_json::json!({ "version": "   " }),
            77,
        );
        assert_eq!(version, "tenant-db:77");
    }

    #[test]
    fn tenant_config_version_trims_explicit_version() {
        let version = tenant_config_version(
            &serde_json::json!({ "version": "  fleet-9  " }),
            1,
        );
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
                extra: std::collections::HashMap::new(),
                allowed_tools: None,
},
            "v1".to_string(),
            serde_json::Value::Null,
        )));
        assert!(legacy_create_should_use_tenant_config(&ok_some));

        let ok_none: AresResult<Option<(AgentConfig, String, serde_json::Value)>> = Ok(None);
        assert!(!legacy_create_should_use_tenant_config(&ok_none));

        let err_load: AresResult<Option<(AgentConfig, String, serde_json::Value)>> = Err(AppError::Database("x".into()));
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

    #[cfg(feature = "postgres")]
    mod postgres_integration {
        use super::super::{
            agent_config_from_json, create_tenant_agent, resolve_agent_for_tenant,
            resolve_required_tenant_agent, AgentConfigSource,
        };
        use crate::registry::AgentRegistry;
        use crate::Agent;
        use ares_config::toml_config::{AgentConfig, ModelConfig, ProviderConfig};
        use ares_db::postgres::PostgresClient;
        use ares_db::tenant_agents::{
            create_tenant_agent as db_create_tenant_agent, update_tenant_agent,
            CreateTenantAgentRequest, UpdateTenantAgentRequest,
        };
        use ares_llm::ProviderRegistry;
        use ares_tools::registry::ToolRegistry;
        use ares_types::types::{AgentContext, AppError};
        use axum::{routing::post, Json, Router};
        use serde_json::{json, Value};
        use sqlx::PgPool;
        use std::collections::HashMap;
        use std::sync::{Arc, Once};

        static LOAD_ENV: Once = Once::new();
        static INIT_SCHEMA: std::sync::OnceLock<()> = std::sync::OnceLock::new();

        fn ensure_env_loaded() {
            LOAD_ENV.call_once(|| {
                let _ = dotenvy::dotenv();
            });
        }

        fn test_db_url() -> String {
            ensure_env_loaded();

            if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
                return url;
            }
            if let Ok(url) = std::env::var("DATABASE_URL") {
                if url.contains("/ares") && !url.contains("ares_test") {
                    return url.replace("/ares", "/ares_test");
                }
                return url;
            }
            "postgres://dirmacs@localhost:5432/ares_test".to_string()
        }

        async fn test_pool() -> PgPool {
            let url = test_db_url();
            let db = PostgresClient::new_remote(url, String::new())
                .await
                .expect("connect to ares_test");

            if INIT_SCHEMA.set(()).is_ok() {
                sqlx::migrate!("../../migrations")
                    .run(&db.pool)
                    .await
                    .expect("run migrations on ares_test");
            }

            db.pool
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
                axum::serve(listener, app)
                    .await
                    .expect("serve mock ollama");
            });

            format!("http://{}", addr)
        }

        fn registry_with_product(mock_ollama_url: &str) -> AgentRegistry {
            let mut provider_registry = ProviderRegistry::new();
            provider_registry.register_provider(
                "ollama-local",
                ProviderConfig::OpenAI {
                    api_key_env: "TEST_KEY".to_string(),
                    api_base: "https://test.example.com/v1".to_string(),
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
                Arc::new(ToolRegistry::new()),
            );
            registry.register(
                "product",
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("registry-product-prompt".to_string()),
                    tools: vec![],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    extra: HashMap::new(),
                    allowed_tools: None,
},
            );
            registry
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

            let agent = create_tenant_agent(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new()).await;
            assert!(agent.is_none());
        }

        #[tokio::test]
        async fn create_tenant_agent_builds_agent_from_tenant_config() {
            let mock_ollama = spawn_mock_ollama_server().await;
            let pool = test_pool().await;
            let registry = registry_with_product(&mock_ollama);
            let tenant_id = unique_id("create-legacy");
            insert_tenant_agent(&pool, &tenant_id, "product", "tenant-create-prompt").await;

            let agent = create_tenant_agent(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new())
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
            insert_tenant_agent(&pool, &tenant_id, "product", "tenant-db-prompt").await;

            let resolved = resolve_agent_for_tenant(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new())
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

            let resolved = resolve_agent_for_tenant(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new())
                .await
                .expect("resolve registry agent");

            assert_eq!(resolved.source, AgentConfigSource::Registry);
            assert!(resolved.config_version.is_none());
            assert_eq!(resolved.agent.system_prompt(), "registry-product-prompt");
        }

        #[tokio::test]
        async fn resolve_required_tenant_agent_errors_when_row_missing() {
            let pool = test_pool().await;
            let registry = registry_with_product("http://127.0.0.1:9");
            let tenant_id = unique_id("required-missing");

            let err = match resolve_required_tenant_agent(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new()).await {
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
            insert_tenant_agent(&pool, &tenant_id, "product", "tenant-execute-prompt").await;

            let resolved = resolve_agent_for_tenant(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new())
                .await
                .expect("resolve for execution");

            let response = resolved
                .agent
                .execute("hello", &test_agent_context())
                .await
                .expect("execute resolved tenant agent");

            assert!(response.content.contains("SYSTEM_PROMPT=tenant-execute-prompt"));
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

            let err = match resolve_agent_for_tenant(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new()).await {
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
            sqlx::query!(
                r#"
                INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)
                "#,
                &id,
                &tenant_id,
                "product",
                "Broken",
                Option::<String>::None,
                &serde_json::json!({ "model": "default", "parallel_tools": "yes" }),
                now_ts
            )
            .execute(&pool)
            .await
            .expect("insert invalid tenant config via raw SQL");
            let err = match resolve_agent_for_tenant(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new()).await {
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
            sqlx::query!(
                r#"
                INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)
                "#,
                &id,
                &tenant_id,
                "product",
                "Broken",
                Option::<String>::None,
                &serde_json::json!({ "model": "" }),
                now_ts
            )
            .execute(&pool)
            .await
            .expect("insert empty-model tenant config via raw SQL");

            let agent = create_tenant_agent(&pool, &registry, &tenant_id, "product", &ares_config::fleet_secrets::FleetSecrets::new()).await;
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
