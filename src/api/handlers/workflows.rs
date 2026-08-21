//! Workflow execution handler
//!
//! Handles HTTP requests for executing declarative workflows defined in ares.toml.

use std::sync::Arc;
use crate::AppState;
use ares_cordis_core::Context;

use crate::{
    auth::middleware::AuthUser,
    types::{AgentContext, Result, WorkflowRequest},
    workflows::{WorkflowEngine, WorkflowOutput},
    AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
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
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(workflow_name): Path<String>,
    Json(payload): Json<WorkflowRequest>,
) -> Result<Json<WorkflowOutput>> {
    // Create workflow engine
    let workflow_engine = WorkflowEngine::new(ctx.clone());

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
    State(ctx): State<Arc<Context>>,
    AuthUser(_claims): AuthUser,
) -> Result<Json<Vec<WorkflowInfo>>> {
    let config = ctx.get::<crate::context_services::ConfigManagerService>().expect("not provided").0.config();

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
    /// Workflow name
    pub name: String,
    /// Agent that starts the workflow
    pub entry_agent: String,
    /// Fallback agent if entry fails
    pub fallback_agent: Option<String>,
    /// Maximum agent delegation depth
    pub max_depth: u8,
    /// Maximum workflow iterations
    pub max_iterations: u8,
    /// Whether subagents run in parallel
    pub parallel_subagents: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WorkflowRequest;

    #[test]
    fn workflow_info_serializes_expected_fields() {
        let info = WorkflowInfo {
            name: "research".into(),
            entry_agent: "orchestrator".into(),
            fallback_agent: Some("router".into()),
            max_depth: 3,
            max_iterations: 10,
            parallel_subagents: true,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["name"], "research");
        assert_eq!(json["entry_agent"], "orchestrator");
        assert_eq!(json["fallback_agent"], "router");
        assert_eq!(json["max_depth"], 3);
        assert_eq!(json["parallel_subagents"], true);
    }

    #[test]
    fn workflow_request_deserializes_with_default_context() {
        let req: WorkflowRequest = serde_json::from_str(r#"{"query":"summarize"}"#).unwrap();
        assert_eq!(req.query, "summarize");
        assert!(req.context.is_empty());
    }

    #[test]
    fn workflow_request_roundtrip_preserves_context() {
        let mut context = std::collections::HashMap::new();
        context.insert("locale".into(), serde_json::json!("en-GB"));
        let req = WorkflowRequest {
            query: "run".into(),
            context,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: WorkflowRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.query, "run");
        assert_eq!(back.context["locale"], "en-GB");
    }
}

