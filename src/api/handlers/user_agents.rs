//! User agents API handlers
//!
//! This module provides CRUD endpoints for user-created agents with TOON import/export support.

use crate::auth::middleware::AuthUser;
use crate::db::turso::UserAgent;
use crate::types::{AppError, Result};
use crate::utils::toon_config::ToonAgentConfig;
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use toon_format::{decode_default, encode_default};

// ============= Request/Response Types =============

/// Request to create a new user agent
#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: i32,
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default)]
    pub is_public: bool,
}

fn default_max_tool_iterations() -> i32 {
    10
}

/// Response after creating an agent
#[derive(Debug, Serialize)]
pub struct CreateAgentResponse {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub created_at: i64,
    pub api_endpoint: String,
    /// TOON serialization for preview/export
    pub toon_preview: String,
}

/// Response for agent details
#[derive(Debug, Serialize)]
pub struct AgentResponse {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub max_tool_iterations: i32,
    pub parallel_tools: bool,
    pub is_public: bool,
    pub usage_count: i32,
    pub average_rating: Option<f32>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Source of the agent: "user", "community", or "system"
    pub source: String,
}

/// Query parameters for get agent
#[derive(Debug, Deserialize, Default)]
pub struct GetAgentQuery {
    /// Format: "json" (default) or "toon"
    #[serde(default)]
    pub format: Option<String>,
}

/// Query parameters for listing agents
#[derive(Debug, Deserialize, Default)]
pub struct ListAgentsQuery {
    /// Include public/community agents
    #[serde(default)]
    pub include_public: bool,
    /// Limit results
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Offset for pagination
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    50
}

/// Request to update an agent
#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub max_tool_iterations: Option<i32>,
    #[serde(default)]
    pub parallel_tools: Option<bool>,
    #[serde(default)]
    pub is_public: Option<bool>,
}

// ============= Handlers =============

/// Create a new user agent
///
/// POST /api/user/agents
pub async fn create_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<CreateAgentResponse>> {
    // Validate agent name (alphanumeric, hyphens, underscores)
    if !is_valid_agent_name(&req.name) {
        return Err(AppError::InvalidInput(
            "Agent name must be alphanumeric with hyphens and underscores only".to_string(),
        ));
    }

    // Check if agent name already exists for this user
    if state
        .turso
        .get_user_agent_by_name(&user.0.sub, &req.name)
        .await?
        .is_some()
    {
        return Err(AppError::InvalidInput(format!(
            "Agent '{}' already exists",
            req.name
        )));
    }

    // Validate model exists (check TOON config first, then TOML config)
    let model_exists = state.dynamic_config.model(&req.model).is_some()
        || state.config_manager.config().get_model(&req.model).is_some();

    if !model_exists {
        return Err(AppError::InvalidInput(format!(
            "Model '{}' not found. Available models: {:?}",
            req.model,
            state.dynamic_config.model_names()
        )));
    }

    // Validate tools exist
    for tool in &req.tools {
        let tool_exists = state.dynamic_config.tool(tool).is_some()
            || state.config_manager.config().get_tool(tool).is_some();

        if !tool_exists {
            return Err(AppError::InvalidInput(format!(
                "Tool '{}' not found. Available tools: {:?}",
                tool,
                state.dynamic_config.tool_names()
            )));
        }
    }

    // Create ToonAgentConfig for TOON serialization
    let agent_config = ToonAgentConfig {
        name: req.name.clone(),
        model: req.model.clone(),
        system_prompt: req.system_prompt.clone(),
        tools: req.tools.clone(),
        max_tool_iterations: req.max_tool_iterations as usize,
        parallel_tools: req.parallel_tools,
        extra: std::collections::HashMap::new(),
    };

    // Generate TOON preview
    let toon_preview = encode_default(&agent_config)
        .map_err(|e| AppError::Internal(format!("TOON encode error: {}", e)))?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    let agent = UserAgent {
        id: id.clone(),
        user_id: user.0.sub.clone(),
        name: req.name.clone(),
        display_name: req.display_name.clone(),
        description: req.description,
        model: req.model,
        system_prompt: req.system_prompt,
        tools: serde_json::to_string(&req.tools).unwrap_or_else(|_| "[]".to_string()),
        max_tool_iterations: req.max_tool_iterations,
        parallel_tools: req.parallel_tools,
        extra: "{}".to_string(),
        is_public: req.is_public,
        usage_count: 0,
        rating_sum: 0,
        rating_count: 0,
        created_at: now,
        updated_at: now,
    };

    state.turso.create_user_agent(&agent).await?;

    Ok(Json(CreateAgentResponse {
        id,
        name: req.name.clone(),
        display_name: req.display_name,
        created_at: now,
        api_endpoint: format!("/api/user/agents/{}/chat", req.name),
        toon_preview,
    }))
}

/// Import agent from TOON format
///
/// POST /api/user/agents/import
/// Content-Type: text/x-toon
pub async fn import_agent_toon(
    State(state): State<AppState>,
    user: AuthUser,
    body: String,
) -> Result<Json<CreateAgentResponse>> {
    // Parse TOON
    let agent_config: ToonAgentConfig = decode_default(&body)
        .map_err(|e| AppError::InvalidInput(format!("Invalid TOON format: {}", e)))?;

    // Convert to CreateAgentRequest and delegate
    let req = CreateAgentRequest {
        name: agent_config.name,
        display_name: None,
        description: None,
        model: agent_config.model,
        system_prompt: agent_config.system_prompt,
        tools: agent_config.tools,
        max_tool_iterations: agent_config.max_tool_iterations as i32,
        parallel_tools: agent_config.parallel_tools,
        is_public: false,
    };

    create_agent(State(state), user, Json(req)).await
}

/// Get a user agent by name
///
/// GET /api/user/agents/:name
pub async fn get_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<GetAgentQuery>,
    user: AuthUser,
) -> Result<Response> {
    // Resolve agent using the three-tier hierarchy
    let (agent, source) = resolve_agent(&state, &user.0.sub, &name).await?;

    match params.format.as_deref() {
        Some("toon") => {
            // Convert to ToonAgentConfig and serialize
            let config = ToonAgentConfig {
                name: agent.name.clone(),
                model: agent.model.clone(),
                system_prompt: agent.system_prompt.clone(),
                tools: agent.tools_vec(),
                max_tool_iterations: agent.max_tool_iterations as usize,
                parallel_tools: agent.parallel_tools,
                extra: std::collections::HashMap::new(),
            };

            let toon = encode_default(&config)
                .map_err(|e| AppError::Internal(format!("TOON encode error: {}", e)))?;

            Ok(([(header::CONTENT_TYPE, "text/x-toon")], toon).into_response())
        }
        _ => {
            let tools = agent.tools_vec();
            let avg_rating = agent.average_rating();
            Ok(Json(AgentResponse {
                id: agent.id,
                name: agent.name,
                display_name: agent.display_name,
                description: agent.description,
                model: agent.model,
                system_prompt: agent.system_prompt,
                tools,
                max_tool_iterations: agent.max_tool_iterations,
                parallel_tools: agent.parallel_tools,
                is_public: agent.is_public,
                usage_count: agent.usage_count,
                average_rating: avg_rating,
                created_at: agent.created_at,
                updated_at: agent.updated_at,
                source,
            })
            .into_response())
        }
    }
}

/// List user agents
///
/// GET /api/user/agents
pub async fn list_agents(
    State(state): State<AppState>,
    Query(params): Query<ListAgentsQuery>,
    user: AuthUser,
) -> Result<Json<Vec<AgentResponse>>> {
    let mut agents = Vec::new();

    // Get user's own agents
    let user_agents = state.turso.list_user_agents(&user.0.sub).await?;
    for agent in user_agents {
        agents.push(user_agent_to_response(agent, "user".to_string()));
    }

    // Include public agents if requested
    if params.include_public {
        let public_agents = state
            .turso
            .list_public_agents(params.limit, params.offset)
            .await?;

        for agent in public_agents {
            // Skip if user already owns this agent
            if agents.iter().any(|a| a.name == agent.name) {
                continue;
            }

            agents.push(user_agent_to_response(agent, "community".to_string()));
        }
    }

    Ok(Json(agents))
}

/// Update a user agent
///
/// PUT /api/user/agents/:name
pub async fn update_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
    user: AuthUser,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<AgentResponse>> {
    // Get existing agent
    let mut agent = state
        .turso
        .get_user_agent_by_name(&user.0.sub, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Agent '{}' not found", name)))?;

    // Verify ownership
    if agent.user_id != user.0.sub {
        return Err(AppError::Auth(
            "You can only update your own agents".to_string(),
        ));
    }

    // Apply updates
    if let Some(display_name) = req.display_name {
        agent.display_name = Some(display_name);
    }
    if let Some(description) = req.description {
        agent.description = Some(description);
    }
    if let Some(model) = req.model {
        // Validate model exists
        let model_exists = state.dynamic_config.model(&model).is_some()
            || state.config_manager.config().get_model(&model).is_some();
        if !model_exists {
            return Err(AppError::InvalidInput(format!("Model '{}' not found", model)));
        }
        agent.model = model;
    }
    if let Some(system_prompt) = req.system_prompt {
        agent.system_prompt = Some(system_prompt);
    }
    if let Some(tools) = req.tools {
        // Validate tools exist
        for tool in &tools {
            let tool_exists = state.dynamic_config.tool(tool).is_some()
                || state.config_manager.config().get_tool(tool).is_some();
            if !tool_exists {
                return Err(AppError::InvalidInput(format!("Tool '{}' not found", tool)));
            }
        }
        agent.set_tools(tools);
    }
    if let Some(max_tool_iterations) = req.max_tool_iterations {
        agent.max_tool_iterations = max_tool_iterations;
    }
    if let Some(parallel_tools) = req.parallel_tools {
        agent.parallel_tools = parallel_tools;
    }
    if let Some(is_public) = req.is_public {
        agent.is_public = is_public;
    }

    agent.updated_at = Utc::now().timestamp();

    state.turso.update_user_agent(&agent).await?;

    Ok(Json(user_agent_to_response(agent, "user".to_string())))
}

/// Delete a user agent
///
/// DELETE /api/user/agents/:name
pub async fn delete_agent(
    State(state): State<AppState>,
    Path(name): Path<String>,
    user: AuthUser,
) -> Result<StatusCode> {
    // Get agent to verify ownership
    let agent = state
        .turso
        .get_user_agent_by_name(&user.0.sub, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Agent '{}' not found", name)))?;

    // Delete agent
    let deleted = state
        .turso
        .delete_user_agent(&agent.id, &user.0.sub)
        .await?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!("Agent '{}' not found", name)))
    }
}

/// Export agent as TOON file
///
/// GET /api/user/agents/:name/export
pub async fn export_agent_toon(
    State(state): State<AppState>,
    Path(name): Path<String>,
    user: AuthUser,
) -> Result<Response> {
    let (agent, _) = resolve_agent(&state, &user.0.sub, &name).await?;

    let tools = agent.tools_vec();
    let filename = format!("{}.toon", agent.name);
    let config = ToonAgentConfig {
        name: agent.name,
        model: agent.model,
        system_prompt: agent.system_prompt,
        tools,
        max_tool_iterations: agent.max_tool_iterations as usize,
        parallel_tools: agent.parallel_tools,
        extra: std::collections::HashMap::new(),
    };

    let toon = encode_default(&config)
        .map_err(|e| AppError::Internal(format!("TOON encode error: {}", e)))?;

    Ok((
        [
            (header::CONTENT_TYPE, "text/x-toon"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        toon,
    )
        .into_response())
}

// ============= Helper Functions =============

/// Validate agent name (alphanumeric, hyphens, underscores)
fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Convert UserAgent to AgentResponse
fn user_agent_to_response(agent: UserAgent, source: String) -> AgentResponse {
    let tools = agent.tools_vec();
    let avg_rating = agent.average_rating();
    AgentResponse {
        id: agent.id,
        name: agent.name,
        display_name: agent.display_name,
        description: agent.description,
        model: agent.model,
        system_prompt: agent.system_prompt,
        tools,
        max_tool_iterations: agent.max_tool_iterations,
        parallel_tools: agent.parallel_tools,
        is_public: agent.is_public,
        usage_count: agent.usage_count,
        average_rating: avg_rating,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
        source,
    }
}

/// Resolve agent by checking: user private -> user public -> community -> system
/// Returns (UserAgent, source) where source is "user", "community", or "system"
pub async fn resolve_agent(
    state: &AppState,
    user_id: &str,
    name: &str,
) -> Result<(UserAgent, String)> {
    // 1. Check user's agents (private + public)
    if let Some(agent) = state.turso.get_user_agent_by_name(user_id, name).await? {
        return Ok((agent, "user".to_string()));
    }

    // 2. Check community agents (public agents from other users)
    if let Some(agent) = state.turso.get_public_agent_by_name(name).await? {
        return Ok((agent, "community".to_string()));
    }

    // 3. Check system agents (TOON config)
    if let Some(config) = state.dynamic_config.agent(name) {
        // Convert ToonAgentConfig to UserAgent for consistent response
        let agent = UserAgent {
            id: format!("system-{}", name),
            user_id: "system".to_string(),
            name: config.name,
            display_name: None,
            description: None,
            model: config.model,
            system_prompt: config.system_prompt,
            tools: serde_json::to_string(&config.tools).unwrap_or_else(|_| "[]".to_string()),
            max_tool_iterations: config.max_tool_iterations as i32,
            parallel_tools: config.parallel_tools,
            extra: "{}".to_string(),
            is_public: true, // System agents are always "public"
            usage_count: 0,
            rating_sum: 0,
            rating_count: 0,
            created_at: 0,
            updated_at: 0,
        };
        return Ok((agent, "system".to_string()));
    }

    // 4. Fallback: Check TOML config (legacy)
    if let Some(config) = state.config_manager.config().get_agent(name) {
        let agent = UserAgent {
            id: format!("system-{}", name),
            user_id: "system".to_string(),
            name: name.to_string(),
            display_name: None,
            description: None,
            model: config.model.clone(),
            system_prompt: config.system_prompt.clone(),
            tools: serde_json::to_string(&config.tools).unwrap_or_else(|_| "[]".to_string()),
            max_tool_iterations: config.max_tool_iterations as i32,
            parallel_tools: config.parallel_tools,
            extra: "{}".to_string(),
            is_public: true,
            usage_count: 0,
            rating_sum: 0,
            rating_count: 0,
            created_at: 0,
            updated_at: 0,
        };
        return Ok((agent, "system".to_string()));
    }

    Err(AppError::NotFound(format!("Agent '{}' not found", name)))
}
