//! Built-in agent listing handler.

use crate::types::AgentType;
use axum::{extract::State, Json};
use cordis::Context;
use serde::Serialize;
use std::sync::Arc;

/// Static catalog of built-in agent types exposed by the API.
pub(crate) fn builtin_agent_catalog() -> Vec<AgentInfo> {
    vec![
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
    ]
}

/// Lists all available built-in agents.
pub async fn list_agents(State(_ctx): State<Arc<Context>>) -> Json<Vec<AgentInfo>> {
    Json(builtin_agent_catalog())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_five_builtin_agents() {
        let agents = builtin_agent_catalog();
        assert_eq!(agents.len(), 5);
        let types: Vec<_> = agents.iter().map(|a| a.agent_type.clone()).collect();
        assert!(types.contains(&AgentType::Product));
        assert!(types.contains(&AgentType::HR));
    }

    #[test]
    fn catalog_entries_have_names_and_descriptions() {
        for agent in builtin_agent_catalog() {
            assert!(!agent.name.is_empty());
            assert!(!agent.description.is_empty());
        }
    }
}
