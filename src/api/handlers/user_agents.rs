use crate::{
    db::postgres::UserAgent,
    db::traits::DatabaseClient,
    types::{AppError, Result},
    AppState,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct CreateUserAgentReq {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    #[serde(default = "default_max_iterations")]
    pub max_tool_iterations: i32,
    #[serde(default)]
    pub parallel_tools: bool,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_max_iterations() -> i32 {
    10
}

#[derive(Debug, Serialize)]
pub struct UserAgentResponse {
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
}

impl From<UserAgent> for UserAgentResponse {
    fn from(agent: UserAgent) -> Self {
        let tools = agent.tools_vec();
        let rating = agent.average_rating();
        Self {
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
            average_rating: rating,
            created_at: agent.created_at,
            updated_at: agent.updated_at,
        }
    }
}

pub async fn resolve_agent(
    state: &AppState,
    user_id: &str,
    agent_name: String,
) -> Result<(UserAgent, String)> {
    if let Some(agent) = state
        .db
        .get_user_agent_by_name(user_id, &agent_name)
        .await?
    {
        return Ok((agent, "user".to_string()));
    }

    if let Some(agent) = state.db.get_public_agent_by_name(&agent_name).await? {
        return Ok((agent, "community".to_string()));
    }

    // Tier 3: fall back to system agents from TOON/TOML config
    let config = state.config_manager.config();
    if let Some(agent_cfg) = config.get_agent(&agent_name) {
        let now = chrono::Utc::now().timestamp();
        let system_agent = crate::db::postgres::UserAgent {
            id: format!("system-{}", agent_name),
            user_id: "system".to_string(),
            name: agent_name.clone(),
            display_name: None,
            description: None,
            model: agent_cfg.model.clone(),
            system_prompt: agent_cfg.system_prompt.clone(),
            tools: serde_json::to_string(&agent_cfg.tools).unwrap_or_else(|_| "[]".to_string()),
            max_tool_iterations: agent_cfg.max_tool_iterations as i32,
            parallel_tools: agent_cfg.parallel_tools,
            extra: "{}".to_string(),
            is_public: true,
            usage_count: 0,
            rating_sum: 0,
            rating_count: 0,
            created_at: now,
            updated_at: now,
        };
        return Ok((system_agent, "system".to_string()));
    }

    Err(AppError::NotFound(format!("Agent '{}' not found", agent_name)))
}

// Dummy stubs to fix routing
pub async fn list_agents() {}
pub async fn create_agent() {}
pub async fn import_agent_toon() {}
pub async fn get_agent() {}
pub async fn update_agent() {}
pub async fn delete_agent() {}
pub async fn export_agent_toon() {}
