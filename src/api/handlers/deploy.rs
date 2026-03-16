use crate::types::{AppError, Result};
use crate::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
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

pub type DeployRegistry = Arc<RwLock<HashMap<String, DeployStatus>>>;

pub fn new_deploy_registry() -> DeployRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

const VALID_TARGETS: &[&str] = &["ares", "admin", "eruka", "dotdot"];
const DEPLOY_SCRIPT: &str = "/opt/dirmacs-ops/deploy.sh";
const HEALTH_SCRIPT: &str = "/opt/dirmacs-ops/health.sh";

/// POST /api/admin/deploy — trigger a deployment
pub async fn trigger_deploy(
    State(state): State<AppState>,
    Json(req): Json<DeployRequest>,
) -> Result<Json<DeployResponse>> {
    let target = req.target.to_lowercase();
    if !VALID_TARGETS.contains(&target.as_str()) {
        return Err(AppError::InvalidInput(format!(
            "Invalid target '{}'. Valid: {}",
            target,
            VALID_TARGETS.join(", ")
        )));
    }

    let registry = &state.deploy_registry;

    // Check if there's already a running deploy for this target
    {
        let deploys = registry.read().await;
        for deploy in deploys.values() {
            if deploy.target == target && deploy.status == DeployState::Running {
                return Err(AppError::InvalidInput(format!(
                    "Deploy already running for '{}' (id: {})",
                    target, deploy.id
                )));
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
        let result = tokio::process::Command::new(DEPLOY_SCRIPT)
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
    State(state): State<AppState>,
    Path(deploy_id): Path<String>,
) -> Result<Json<DeployStatus>> {
    let deploys = state.deploy_registry.read().await;
    deploys
        .get(&deploy_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Deploy '{}' not found", deploy_id)))
}

/// GET /api/admin/deploys — list recent deploys
pub async fn list_deploys(State(state): State<AppState>) -> Json<Vec<DeployStatus>> {
    let deploys = state.deploy_registry.read().await;
    let mut list: Vec<DeployStatus> = deploys.values().cloned().collect();
    list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    list.truncate(20);
    Json(list)
}

/// GET /api/admin/services — health check all services
pub async fn get_services_health() -> Result<Json<HashMap<String, ServiceHealth>>> {
    let output = tokio::process::Command::new("bash")
        .arg(HEALTH_SCRIPT)
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
        return Err(AppError::InvalidInput(format!(
            "Unknown service: {}",
            service_name
        )));
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
