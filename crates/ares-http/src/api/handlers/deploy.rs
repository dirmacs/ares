use ares_types::types::{AppError};
use crate::Result;
use crate::HttpError;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use cordis::Context;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeployStatus {
    pub id: String,
    pub target: String,
    pub status: DeployState,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeployState {
    Running,
    Success,
    Failed,
}

#[derive(Debug, Deserialize)]
pub struct DeployRequest {
    pub target: String,
}

#[derive(Debug, Serialize)]
pub struct DeployResponse {
    pub id: String,
    pub status: DeployState,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ServiceHealth {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

// ---------------------------------------------------------------------------
// Deploy registry — in-memory store for deploy status
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct DeployRegistry(Arc<RwLock<HashMap<String, DeployStatus>>>);

impl std::ops::Deref for DeployRegistry {
    type Target = RwLock<HashMap<String, DeployStatus>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl cordis::Service for DeployRegistry {
    fn name(&self) -> &'static str { "deploy_registry" }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool { true }
}

pub fn new_deploy_registry() -> DeployRegistry {
    DeployRegistry::default()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

const VALID_TARGETS: &[&str] = &["ares", "admin", "eruka", "dotdot"];
fn deploy_script() -> String {
    std::env::var("DEPLOY_SCRIPT").unwrap_or_else(|_| "./scripts/deploy.sh".to_string())
}
fn health_script() -> String {
    std::env::var("HEALTH_SCRIPT").unwrap_or_else(|_| "./scripts/health.sh".to_string())
}

/// POST /api/admin/deploy — trigger a deployment
pub async fn trigger_deploy(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<DeployRequest>,
) -> Result<Json<DeployResponse>> {
    let target = req.target.to_lowercase();
    if !VALID_TARGETS.contains(&target.as_str()) {
        return Err(HttpError::from(AppError::InvalidInput(format!(
            "Invalid target '{}'. Valid: {}",
            target,
            VALID_TARGETS.join(", ")
        ).into())));
    }

    let registry = ctx.get::<DeployRegistry>().expect("not provided");

    // Check if there's already a running deploy for this target
    {
        let deploys = registry.read().await;
        for deploy in deploys.values() {
            if deploy.target == target && deploy.status == DeployState::Running {
                return Err(HttpError::from(AppError::InvalidInput(format!(
                    "Deploy already running for '{}' (id: {})",
                    target, deploy.id
                ).into())));
            }
        }
    }

    let id = format!(
        "{}-{}",
        target,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let deploy = DeployStatus {
        id: id.clone(),
        target: target.clone(),
        status: DeployState::Running,
        started_at: now,
        finished_at: None,
        output: String::new(),
    };

    registry.write().await.insert(id.clone(), deploy);

    // Spawn the deploy process in background
    let reg = registry.clone();
    let deploy_id = id.clone();
    let deploy_target = target.clone();
    tokio::spawn(async move {
        let result = tokio::process::Command::new(deploy_script())
            .arg(&deploy_target)
            .output()
            .await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let mut deploys = reg.write().await;
        if let Some(deploy) = deploys.get_mut(&deploy_id) {
            match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    deploy.output = if stderr.is_empty() {
                        stdout
                    } else {
                        format!("{}\n--- stderr ---\n{}", stdout, stderr)
                    };
                    deploy.status = if output.status.success() {
                        DeployState::Success
                    } else {
                        DeployState::Failed
                    };
                }
                Err(e) => {
                    deploy.output = format!("Failed to execute deploy script: {}", e);
                    deploy.status = DeployState::Failed;
                }
            }
            deploy.finished_at = Some(now);
        }
    });

    Ok(Json(DeployResponse {
        id,
        status: DeployState::Running,
        message: format!("Deploy started for '{}'", target),
    }))
}

/// GET /api/admin/deploy/{deploy_id} — get deploy status
pub async fn get_deploy_status(
    State(ctx): State<Arc<Context>>,
    Path(deploy_id): Path<String>,
) -> Result<Json<DeployStatus>> {
    let registry = ctx.get::<DeployRegistry>().expect("not provided");
    let deploys = registry.read().await;
    deploys
        .get(&deploy_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("Deploy '{}' not found", deploy_id))))
}

/// GET /api/admin/deploys — list recent deploys
pub async fn list_deploys(State(ctx): State<Arc<Context>>) -> Json<Vec<DeployStatus>> {
    let registry = ctx.get::<DeployRegistry>().expect("not provided");
    let deploys = registry.read().await;
    let mut list: Vec<DeployStatus> = deploys.values().cloned().collect();
    list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    list.truncate(20);
    Json(list)
}

/// GET /api/admin/services — health check all services
pub async fn get_services_health() -> Result<Json<HashMap<String, ServiceHealth>>> {
    let output = tokio::process::Command::new("bash")
        .arg(health_script())
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to run health script: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: HashMap<String, serde_json::Value> =
        serde_json::from_str(&stdout).map_err(|e| {
            AppError::Internal(format!(
                "Failed to parse health output: {} — raw: {}",
                e, stdout
            ))
        })?;

    let mut result = HashMap::new();
    for (name, val) in parsed {
        let status = val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let pid = val
            .get("pid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let port = val.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);
        result.insert(name, ServiceHealth { status, pid, port });
    }

    Ok(Json(result))
}

/// GET /api/admin/services/{service_name}/logs — recent journalctl logs for a service
pub async fn get_service_logs(Path(service_name): Path<String>) -> Result<Json<serde_json::Value>> {
    if !["ares", "eruka", "caddy", "postgresql"].contains(&service_name.as_str()) {
        return Err(HttpError::from(AppError::InvalidInput(format!(
            "Unknown service: {}",
            service_name
        ).into())));
    }

    let output = tokio::process::Command::new("journalctl")
        .args([
            "-u",
            &service_name,
            "-n",
            "100",
            "--no-pager",
            "-o",
            "short-iso",
        ])
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read logs: {}", e)))?;

    let logs = String::from_utf8_lossy(&output.stdout).to_string();

    Ok(Json(serde_json::json!({
        "service": service_name,
        "lines": logs.lines().collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};

    #[test]
    fn deploy_state_serializes_snake_case() {
        let json = serde_json::to_string(&DeployState::Running).unwrap();
        assert_eq!(json, "\"running\"");
        let parsed: DeployState = serde_json::from_str("\"success\"").unwrap();
        assert_eq!(parsed, DeployState::Success);
    }

    #[test]
    fn deploy_request_deserializes_target() {
        let req: DeployRequest = serde_json::from_str(r#"{"target":"ARES"}"#).unwrap();
        assert_eq!(req.target, "ARES");
    }

    #[test]
    fn service_health_skips_none_fields_in_json() {
        let health = ServiceHealth {
            status: "ok".into(),
            pid: None,
            port: None,
        };
        let json = serde_json::to_string(&health).unwrap();
        assert!(json.contains("ok"));
        assert!(!json.contains("pid"));
        assert!(!json.contains("port"));
    }

    #[tokio::test]
    async fn new_deploy_registry_starts_empty() {
        let registry = new_deploy_registry();
        let deploys = registry.read().await;
        assert!(deploys.is_empty());
    }

    #[test]
    fn deploy_script_defaults_without_env() {
        std::env::remove_var("DEPLOY_SCRIPT");
        assert_eq!(deploy_script(), "./scripts/deploy.sh");
    }

    #[test]
    fn health_script_defaults_without_env() {
        std::env::remove_var("HEALTH_SCRIPT");
        assert_eq!(health_script(), "./scripts/health.sh");
    }

    #[cfg(test)]
    mod handler_tests {
        use super::*;
        use ares_agent::context_provider::NoOpContextProvider;
        use crate::auth::jwt::AuthService;
        use ares_store::{PostgresClient, TenantDb};
        use crate::overlay::{
            AgentConfig, AresConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths,
            ModelConfig, ProviderConfig, RagConfig,
        };
        use crate::config::{AuthConfig, ServerConfig};
        use ares_agent::AgentRegistry;
        use ares_llm::ProviderRegistry;
        use crate::{AresConfigManager, DynamicConfigManager};
        use std::collections::HashMap;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
use cordis::Context;

        fn minimal_config() -> AresConfig {
            let mut providers = HashMap::new();
            providers.insert(
                "p".into(),
                ProviderConfig::OpenAI {
                    api_key_env: "TEST_KEY".into(),
                    api_base: "https://test.example.com/v1".into(),
                    default_model: "m".into(),
                },
            );
            let mut models = HashMap::new();
            models.insert(
                "default".into(),
                ModelConfig {
                    provider: "p".into(),
                    model: "m".into(),
                    temperature: 0.7,
                    max_tokens: 512,
                },
            );
            let mut agents = HashMap::new();
            agents.insert(
                "a".into(),
                AgentConfig {
                                model: "default".into(),
                                system_prompt: None,
                                tools: vec![],
                                allowed_tools: None,
                                max_tool_iterations: 1,
                                parallel_tools: false,
                                extra: HashMap::new(),
                            },
            );
            AresConfig {
                server: ServerConfig::default(),
                auth: AuthConfig {
                    jwt_secret_env: "JWT_SECRET".into(),
                    jwt_access_expiry: 900,
                    jwt_refresh_expiry: 604800,
                    api_key_env: "API_KEY".into(),
                },
                database: DatabaseConfig::default(),
                nvidia: None,
                config: DynamicConfigPaths::default(),
                providers,
                models,
                tools: HashMap::new(),
                agents,
                workflows: HashMap::new(),
                rag: RagConfig::default(),
                billing: BillingConfig::default(),
                skills: None,
            }
        }

        fn test_app_state(deploy_registry: DeployRegistry) -> Arc<Context> {
            let ctx = cordis::Context::new_root();
            ctx.provide(deploy_registry);
            // Provide minimal other services needed for handler to avoid panic on expect
            let config = minimal_config();
            let config_manager = Arc::new(AresConfigManager::from_config(config));
            ctx.provide_arc(config_manager.clone());
            let db = Arc::new(PostgresClient::new_test());
            ctx.provide_arc(db.clone());
            ctx.provide(crate::api::handlers::loops::LoopRegistry::new());
            ctx
        }

        fn write_executable_script(dir: &std::path::Path, name: &str, body: &str) -> String {
            let path = dir.join(name);
            std::fs::write(&path, body).expect("write script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
            path.to_string_lossy().into_owned()
        }

        #[tokio::test]
        async fn trigger_deploy_rejects_invalid_target() {
            let state = test_app_state(new_deploy_registry());
            let err = trigger_deploy(
                State(state),
                Json(DeployRequest {
                    target: "not-a-service".into(),
                }),
            )
            .await
            .unwrap_err();
            assert!(matches!(err.0, AppError::InvalidInput(_)));
        }

        #[tokio::test]
        async fn trigger_deploy_rejects_duplicate_running() {
            let registry = new_deploy_registry();
            let now = 1_700_000_000_i64;
            registry.write().await.insert(
                "ares-1".into(),
                DeployStatus {
                    id: "ares-1".into(),
                    target: "ares".into(),
                    status: DeployState::Running,
                    started_at: now,
                    finished_at: None,
                    output: String::new(),
                },
            );
            let state = test_app_state(registry);
            let err = trigger_deploy(
                State(state),
                Json(DeployRequest {
                    target: "ares".into(),
                }),
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains("already running"));
        }

        #[tokio::test]
        async fn trigger_deploy_starts_and_background_script_updates_registry() {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_executable_script(
                dir.path(),
                "deploy.sh",
                "#!/bin/sh\necho deployed\nexit 0\n",
            );
            let previous_deploy_script = std::env::var("DEPLOY_SCRIPT").ok();
            std::env::set_var("DEPLOY_SCRIPT", &script);

            let registry = new_deploy_registry();
            let state = test_app_state(registry.clone());
            let resp = trigger_deploy(
                State(state),
                Json(DeployRequest {
                    target: "ares".into(),
                }),
            )
            .await
            .expect("trigger");
            assert_eq!(resp.0.status, DeployState::Running);

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            let deploys = registry.read().await;
            let deploy = deploys.get(&resp.0.id).expect("deploy row");
            assert_eq!(deploy.status, DeployState::Success);
            assert!(deploy.output.contains("deployed"));
            assert!(deploy.finished_at.is_some());

            match previous_deploy_script {
                Some(prev) => std::env::set_var("DEPLOY_SCRIPT", prev),
                None => std::env::remove_var("DEPLOY_SCRIPT"),
            }
        }

        #[tokio::test]
        async fn get_deploy_status_returns_not_found() {
            let state = test_app_state(new_deploy_registry());
            let err = get_deploy_status(State(state), Path("missing".into()))
                .await
                .unwrap_err();
            assert!(matches!(err.0, AppError::NotFound(_)));
        }

        #[tokio::test]
        async fn get_deploy_status_returns_existing_deploy() {
            let registry = new_deploy_registry();
            let status = DeployStatus {
                id: "ares-99".into(),
                target: "ares".into(),
                status: DeployState::Success,
                started_at: 1,
                finished_at: Some(2),
                output: "done".into(),
            };
            registry
                .write()
                .await
                .insert(status.id.clone(), status.clone());
            let state = test_app_state(registry);
            let got = get_deploy_status(State(state), Path("ares-99".into()))
                .await
                .expect("status");
            assert_eq!(got.0, status);
        }

        #[tokio::test]
        async fn list_deploys_sorts_newest_first_and_truncates_to_twenty() {
            let registry = new_deploy_registry();
            for i in 0..25 {
                registry.write().await.insert(
                    format!("ares-{i}"),
                    DeployStatus {
                        id: format!("ares-{i}"),
                        target: "ares".into(),
                        status: DeployState::Success,
                        started_at: i as i64,
                        finished_at: Some(i as i64),
                        output: String::new(),
                    },
                );
            }
            let state = test_app_state(registry);
            let list = list_deploys(State(state)).await.0;
            assert_eq!(list.len(), 20);
            assert_eq!(list[0].started_at, 24);
            assert_eq!(list[19].started_at, 5);
        }

        #[tokio::test]
        async fn get_services_health_parses_mock_script_output() {
            let dir = tempfile::tempdir().expect("tempdir");
            let script = write_executable_script(
                dir.path(),
                "health.sh",
                "#!/bin/sh\necho '{\"ares\":{\"status\":\"active\",\"pid\":\"123\",\"port\":3000}}'\n",
            );
            let previous_health_script = std::env::var("HEALTH_SCRIPT").ok();
            std::env::set_var("HEALTH_SCRIPT", &script);
            let result = get_services_health().await;
            match previous_health_script {
                Some(prev) => std::env::set_var("HEALTH_SCRIPT", prev),
                None => std::env::remove_var("HEALTH_SCRIPT"),
            }
            let result = result.expect("health");
            let ares = result.0.get("ares").expect("ares service");
            assert_eq!(ares.status, "active");
            assert_eq!(ares.pid.as_deref(), Some("123"));
            assert_eq!(ares.port, Some(3000));
        }

        #[tokio::test]
        async fn get_service_logs_rejects_unknown_service() {
            let err = get_service_logs(Path("unknown".into()))
                .await
                .unwrap_err();
            assert!(matches!(err.0, AppError::InvalidInput(_)));
        }

        #[test]
        fn deploy_registry_readable_via_cordis() {
            use cordis::Service;
            let ctx = cordis::Context::new_root();
            ctx.provide(new_deploy_registry());
            let got = ctx.get::<DeployRegistry>().expect("provided");
            assert_eq!(got.name(), "deploy_registry");
            assert!(got.check());
        }
    }
}
