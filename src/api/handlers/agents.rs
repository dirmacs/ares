//! Built-in agent listing handler.

use crate::{types::AgentType, AppState};
use axum::{extract::State, Json};
use serde::Serialize;

/// Lists all available built-in agents.
pub async fn list_agents(State(_state): State<AppState>) -> Json<Vec<AgentInfo>> {
    Json(vec![
        AgentInfo {
            agent_type: AgentType::Product,
            name: "Product Agent".to_string(),
            description: "Handles product-related queries and recommendations".to_string(),
        },
        AgentInfo {
            agent_type: AgentType::Invoice,
            name: "Invoice Agent".to_string(),
            description: "Processes invoice queries and operations".to_string(),
        },
        AgentInfo {
            agent_type: AgentType::Sales,
            name: "Sales Agent".to_string(),
            description: "Analyzes sales data and provides insights".to_string(),
        },
        AgentInfo {
            agent_type: AgentType::Finance,
            name: "Finance Agent".to_string(),
            description: "Handles financial analysis and reporting".to_string(),
        },
        AgentInfo {
            agent_type: AgentType::HR,
            name: "HR Agent".to_string(),
            description: "Manages human resources queries".to_string(),
        },
    ])
}

/// Information about an available agent.
#[derive(Serialize)]
pub struct AgentInfo {
    /// Type identifier for the agent
    pub agent_type: AgentType,
    /// Display name
    pub name: String,
    /// Description of agent capabilities
    pub description: String,
}
