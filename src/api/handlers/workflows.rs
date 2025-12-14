//! Workflow execution handler
//!
//! Handles HTTP requests for executing declarative workflows defined in ares.toml.

use crate::{
    AppState,
    auth::middleware::AuthUser,
    types::{AgentContext, Result, WorkflowRequest},
    workflows::{WorkflowEngine, WorkflowOutput},
};
use axum::{
    Json,
    extract::{Path, State},
};
use std::sync::Arc;
use uuid::Uuid;

/// Execute a workflow by name
///
/// This endpoint executes a workflow defined in ares.toml. The workflow determines
/// which agents are used and how they interact to process the request.
#[utoipa::path(
    post,
    path = "/api/workflows/{workflow_name}",
    request_body = WorkflowRequest,
    responses(
        (status = 200, description = "Workflow executed successfully", body = WorkflowOutput),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Workflow not found")
    ),
    params(
        ("workflow_name" = String, Path, description = "Name of the workflow to execute")
    ),
    tag = "workflows",
    security(("bearer" = []))
)]
pub async fn execute_workflow(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(workflow_name): Path<String>,
    Json(payload): Json<WorkflowRequest>,
) -> Result<Json<WorkflowOutput>> {
    // Create workflow engine
    let config = state.config_manager.config();
    let workflow_engine = WorkflowEngine::new(
        Arc::clone(&state.agent_registry),
        config,
    );

    // Check if workflow exists
    if !workflow_engine.has_workflow(&workflow_name) {
        return Err(crate::types::AppError::NotFound(format!(
            "Workflow '{}' not found",
            workflow_name
        )));
    }

    // Create agent context
    let context = AgentContext {
        user_id: claims.sub.clone(),
        session_id: Uuid::new_v4().to_string(),
        conversation_history: vec![],
        user_memory: None,
    };

    // Execute the workflow
    let output = workflow_engine
        .execute_workflow(&workflow_name, &payload.query, &context)
        .await?;

    Ok(Json(output))
}

/// List available workflows
///
/// Returns a list of workflow names that are defined in the configuration.
#[utoipa::path(
    get,
    path = "/api/workflows",
    responses(
        (status = 200, description = "List of available workflows"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "workflows",
    security(("bearer" = []))
)]
pub async fn list_workflows(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
) -> Result<Json<Vec<WorkflowInfo>>> {
    let config = state.config_manager.config();
    
    let workflows: Vec<WorkflowInfo> = config
        .workflows
        .iter()
        .map(|(name, wf)| WorkflowInfo {
            name: name.clone(),
            entry_agent: wf.entry_agent.clone(),
            fallback_agent: wf.fallback_agent.clone(),
            max_depth: wf.max_depth,
            max_iterations: wf.max_iterations,
            parallel_subagents: wf.parallel_subagents,
        })
        .collect();

    Ok(Json(workflows))
}

/// Information about a workflow
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct WorkflowInfo {
    pub name: String,
    pub entry_agent: String,
    pub fallback_agent: Option<String>,
    pub max_depth: u8,
    pub max_iterations: u8,
    pub parallel_subagents: bool,
}
