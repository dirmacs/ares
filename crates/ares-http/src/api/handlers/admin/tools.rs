//! Admin tools domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use std::sync::Arc;
use ::cordis::Context;
use super::*;

use ares_store::audit_log;
use ares_types::types::{AppError};
use crate::Result;
use crate::HttpError;
use axum::{
    Json,
    extract::{Path, State},
};
use sha2::Digest;

async fn reload_runtime_tools(ctx: &Arc<Context>, when: &str) {
    let tools = ctx.get::<ares_tools::Tools>().expect("Tools not provided");
    if let Err(e) = tools.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools {when}: {e}");
    }
}

pub async fn list_runtime_tools(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<ares_store::runtime_tools::RuntimeTool>>> {
    let __pool_1 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_1);
    let tools = store.get_all().await?;
    Ok(Json(tools))
}

/// Get a single runtime tool by its UUID.
pub async fn get_runtime_tool(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<Json<ares_store::runtime_tools::RuntimeTool>> {
    let __pool_2 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_2);
    let tool = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("runtime tool {id} not found"))))?;
    Ok(Json(tool))
}

/// Create a new runtime tool. After a successful DB insert the in-memory
/// [`RuntimeToolRegistry`] is reloaded so agents see the tool immediately.
pub async fn create_runtime_tool(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<CreateRuntimeToolRequest>,
) -> Result<Json<serde_json::Value>> {
    validate_runtime_tool_execution_config(&req.tool_type, &req.execution_config)?;

    let __pool_3 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_3);
    let tool = store.create(&req).await?;

    reload_runtime_tools(&ctx, "after create").await;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
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
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRuntimeToolRequest>,
) -> Result<Json<serde_json::Value>> {
    validate_runtime_tool_update_scope_preflight(&req)?;
    let __pool_4 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_4);
    if let Some(execution_config) = &req.execution_config {
        let existing = store
            .get_by_id(&id)
            .await?
            .ok_or_else(|| HttpError::from(AppError::NotFound(format!("runtime tool {id} not found"))))?;
        validate_runtime_tool_execution_config(&existing.tool_type, execution_config)?;
    }
    let tool = store.update(&id, &req).await?;

    reload_runtime_tools(&ctx, "after update").await;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
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
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let __pool_5 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_5);
    let affected = store.delete(&id).await?;
    if affected == 0 {
        return Err(HttpError::from(AppError::NotFound(format!("runtime tool {id} not found").into())));
    }

    reload_runtime_tools(&ctx, "after delete").await;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
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
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<TestRuntimeToolRequest>,
) -> Result<Json<TestRuntimeToolResponse>> {
    let __pool_6 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_6);
    let tool = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("runtime tool {id} not found"))))?;

    let start = std::time::Instant::now();

    let tools = ctx.get::<ares_tools::Tools>().expect("Tools not provided");
    if tools.resolve(&ctx, &tool.name).is_none() {
        if let Err(e) = tools.reload().await {
            tracing::warn!("Failed to reload runtime tools before test: {}", e);
        }
    }

    let result = match tools.resolve(&ctx, &tool.name) {
        Some(runtime_tool) => runtime_tool.execute(req.input_args.clone()).await,
        None => Err(AppError::NotFound(format!(
            "Runtime tool not found: {}",
            tool.name
        ))),
    };
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

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
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
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ares_store::runtime_tools::RuntimeToolVersion>>> {
    let __pool_7 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_7);
    // Verify the tool exists before returning versions.
    let _ = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("runtime tool {id} not found"))))?;

    let versions = store.get_versions(&id, 100).await?;
    Ok(Json(versions))
}

/// Rollback a runtime tool to a previous version.
///
/// The target version's `parameters_schema` and `execution_config` (and
/// `description`) are applied via the normal `update` path, which
/// automatically snapshots the current state as a new version entry.
pub async fn rollback_runtime_tool(
    State(ctx): State<Arc<Context>>,
    Path((id, version)): Path<(String, i32)>,
) -> Result<Json<serde_json::Value>> {
    let __pool_8 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = RuntimeToolStore::new(&__pool_8);

    let _ = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("runtime tool {id} not found"))))?;

    let versions = store.get_versions(&id, 1000).await?;
    let target = versions
        .into_iter()
        .find(|v| v.version == version)
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("version {version} not found for tool {id}"))))?;

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

    reload_runtime_tools(&ctx, "after rollback").await;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
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

pub fn routes() -> axum::Router<Arc<Context>> {
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

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ::cordis::Service;
