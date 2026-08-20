//! Admin tools domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use crate::AppState;
use crate::db::audit_log;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, State},
};
use sha2::Digest;

pub async fn list_runtime_tools(
    State(state): State<AppState>,
) -> Result<Json<Vec<ares_db::runtime_tools::RuntimeTool>>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tools = store.get_all().await?;
    Ok(Json(tools))
}

/// Get a single runtime tool by its UUID.
pub async fn get_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ares_db::runtime_tools::RuntimeTool>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;
    Ok(Json(tool))
}

/// Create a new runtime tool. After a successful DB insert the in-memory
/// [`RuntimeToolRegistry`] is reloaded so agents see the tool immediately.
pub async fn create_runtime_tool(
    State(state): State<AppState>,
    Json(req): Json<CreateRuntimeToolRequest>,
) -> Result<Json<serde_json::Value>> {
    validate_runtime_tool_execution_config(&req.tool_type, &req.execution_config)?;

    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store.create(&req).await?;

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after create: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = tool.id.clone();
    let tool_name = tool.name.clone();
    let tool_type = tool.tool_type.clone();
    tokio::spawn(async move {
        let details = serde_json::json!({
            "name": tool_name,
            "tool_type": tool_type,
            "enabled": tool.enabled,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_create",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": tool.id,
        "name": tool.name,
        "version": tool.version,
        "created_at": tool.created_at,
    })))
}

/// Update a runtime tool. The DB row is patched and the change is automatically
/// snapshotted into `runtime_tool_versions` before the registry is reloaded.
pub async fn update_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRuntimeToolRequest>,
) -> Result<Json<serde_json::Value>> {
    validate_runtime_tool_update_scope_preflight(&req)?;
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    if let Some(execution_config) = &req.execution_config {
        let existing = store
            .get_by_id(&id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;
        validate_runtime_tool_execution_config(&existing.tool_type, execution_config)?;
    }
    let tool = store.update(&id, &req).await?;

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after update: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    let new_version = tool.version;
    tokio::spawn(async move {
        let details = serde_json::json!({ "version": new_version }).to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_update",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": tool.id,
        "name": tool.name,
        "version": tool.version,
        "updated_at": tool.updated_at,
    })))
}

/// Hard-delete a runtime tool (cascades to versions & executions).
pub async fn delete_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let affected = store.delete(&id).await?;
    if affected == 0 {
        return Err(AppError::NotFound(format!("runtime tool {id} not found")));
    }

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after delete: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_delete",
            "runtime_tool",
            &tool_id,
            None,
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": id,
        "deleted": true,
    })))
}

/// Execute a runtime tool with sample input and record the result.
pub async fn test_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TestRuntimeToolRequest>,
) -> Result<Json<TestRuntimeToolResponse>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

    let start = std::time::Instant::now();

    // If the registry doesn't yet contain this tool (e.g. first test after
    // creation), force a reload before executing.
    if state.runtime_tool_registry.get(&tool.name).is_none() {
        if let Err(e) = state.runtime_tool_registry.reload().await {
            tracing::warn!("Failed to reload runtime tools before test: {}", e);
        }
    }

    let result = state
        .runtime_tool_registry
        .execute(&tool.name, req.input_args.clone())
        .await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let (ok, output, error) = match result {
        Ok(v) => (true, Some(v), None),
        Err(e) => (false, None, Some(e.to_string())),
    };

    let log_result = store
        .log_execution(
            &id,
            None, // tenant_id
            None, // agent_run_id
            &req.input_args,
            output.as_ref(),
            if ok { "success" } else { "error" },
            error.as_deref(),
            latency_ms as i64,
        )
        .await;

    if let Err(e) = log_result {
        tracing::warn!("Failed to log runtime tool test execution: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    tokio::spawn(async move {
        let details = serde_json::json!({
            "ok": ok,
            "latency_ms": latency_ms,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_test",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(TestRuntimeToolResponse {
        ok,
        output,
        error,
        latency_ms,
    }))
}

/// Return the version history for a runtime tool.
pub async fn list_runtime_tool_versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ares_db::runtime_tools::RuntimeToolVersion>>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    // Verify the tool exists before returning versions.
    let _ = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

    let versions = store.get_versions(&id, 100).await?;
    Ok(Json(versions))
}

/// Rollback a runtime tool to a previous version.
///
/// The target version's `parameters_schema` and `execution_config` (and
/// `description`) are applied via the normal `update` path, which
/// automatically snapshots the current state as a new version entry.
pub async fn rollback_runtime_tool(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, i32)>,
) -> Result<Json<serde_json::Value>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());

    let _ = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

    let versions = store.get_versions(&id, 1000).await?;
    let target = versions
        .into_iter()
        .find(|v| v.version == version)
        .ok_or_else(|| AppError::NotFound(format!("version {version} not found for tool {id}")))?;

    let update_req = UpdateRuntimeToolRequest {
        display_name: None,
        description: target.description,
        parameters_schema: Some(target.parameters_schema),
        execution_config: Some(target.execution_config),
        enabled: None,
        is_public: None,
        created_by: None,
        tenant_id: None,
    };

    let updated = store.update(&id, &update_req).await?;

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after rollback: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    let new_version = updated.version;
    tokio::spawn(async move {
        let details = serde_json::json!({
            "rolled_back_to": version,
            "new_version": new_version,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_rollback",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": updated.id,
        "name": updated.name,
        "version": updated.version,
        "rolled_back_to": version,
        "updated_at": updated.updated_at,
    })))
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/tools/list_runtime_tools", get(list_runtime_tools))
        .route("/tools/get_runtime_tool", get(get_runtime_tool))
        .route("/tools/create_runtime_tool", post(create_runtime_tool))
        .route("/tools/update_runtime_tool", put(update_runtime_tool))
        .route("/tools/delete_runtime_tool", delete(delete_runtime_tool))
        .route("/tools/test_runtime_tool", post(test_runtime_tool))
        .route("/tools/list_runtime_tool_versions", get(list_runtime_tool_versions))
        .route("/tools/rollback_runtime_tool", post(rollback_runtime_tool))
}

// TODO: ctx.plugin(AdminToolsRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminToolsService;
// impl Service for AdminToolsService {
//     fn name(&self) -> &'static str { "admin_tools" }
//     fn check(&self) -> bool { true }
// }