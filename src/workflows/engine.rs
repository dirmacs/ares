//! Workflow Engine
//!
//! Executes declarative workflows by orchestrating agent execution based on
//! TOML configuration.

use crate::agents::Agent;
use crate::api::handlers::user_agents::resolve_agent;
use crate::types::{AgentContext, AgentType, AppError, Result};
use crate::utils::toml_config::{AgentConfig, WorkflowConfig};
use crate::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Output from a workflow execution
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowOutput {
    /// The final response from the workflow
    pub final_response: String,
    /// Number of steps executed
    pub steps_executed: usize,
    /// List of agent names that were used
    pub agents_used: Vec<String>,
    /// Detailed reasoning path showing each step
    pub reasoning_path: Vec<WorkflowStep>,
}

/// A single step in the workflow execution
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowStep {
    /// The agent that executed this step
    pub agent_name: String,
    /// The input provided to the agent
    pub input: String,
    /// The output from the agent
    pub output: String,
    /// Unix timestamp when this step was executed
    pub timestamp: i64,
    /// Duration of this step in milliseconds
    pub duration_ms: u64,
}

/// Valid agent names for routing
const VALID_AGENTS: &[&str] = &[
    "product",
    "invoice",
    "sales",
    "finance",
    "hr",
    "orchestrator",
    "research",
    "router",
];

/// Workflow engine that orchestrates agent execution
pub struct WorkflowEngine {
    /// Application state for resolving agents
    state: AppState,
}

impl WorkflowEngine {
    /// Create a new workflow engine
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Parse routing decision from router output
    ///
    /// This handles various output formats:
    /// - Clean output: "product"
    /// - With whitespace: "  product  "
    /// - With extra text: "I would route this to product"
    /// - Agent suffix: "product agent"
    fn parse_routing_decision(output: &str) -> Option<String> {
        let trimmed = output.trim().to_lowercase();

        // First, try exact match
        if VALID_AGENTS.contains(&trimmed.as_str()) {
            return Some(trimmed);
        }

        // Try to extract valid agent name from output
        // Split by common delimiters and check each word
        for word in trimmed.split(|c: char| c.is_whitespace() || c == ':' || c == ',' || c == '.') {
            let word = word.trim();
            if VALID_AGENTS.contains(&word) {
                return Some(word.to_string());
            }
        }

        // Check if any valid agent name is contained in the output
        for agent in VALID_AGENTS {
            if trimmed.contains(agent) {
                return Some(agent.to_string());
            }
        }

        None
    }

    /// Execute a workflow by name
    ///
    /// # Arguments
    ///
    /// * `workflow_name` - The name of the workflow to execute (e.g., "default", "research")
    /// * `user_input` - The user's query or input
    /// * `context` - The agent context with user info and conversation history
    ///
    /// # Returns
    ///
    /// A `WorkflowOutput` containing the final response and execution details.
    pub async fn execute_workflow(
        &self,
        workflow_name: &str,
        user_input: &str,
        context: &AgentContext,
    ) -> Result<WorkflowOutput> {
        // Get workflow configuration
        let config = self.state.config_manager.config();
        let workflow = config.get_workflow(workflow_name).ok_or_else(|| {
            AppError::Configuration(format!(
                "Workflow '{}' not found in configuration",
                workflow_name
            ))
        })?;

        let mut steps = Vec::new();
        let mut agents_used = Vec::new();
        let current_input = user_input.to_string();
        let mut current_agent_name = workflow.entry_agent.clone();
        let mut depth = 0;

        // Execute workflow with depth limiting
        while depth < workflow.max_depth {
            let step_start = std::time::Instant::now();
            let timestamp = Utc::now().timestamp();

            // Resolve agent using the 3-tier hierarchy
            let (user_agent, _source) = match resolve_agent(&self.state, &context.user_id, &current_agent_name).await {
                Ok(res) => res,
                Err(e) => {
                    // Try fallback agent if available
                    if let Some(ref fallback) = workflow.fallback_agent {
                        tracing::warn!(
                            "Failed to resolve agent '{}', using fallback '{}'",
                            current_agent_name,
                            fallback
                        );
                        current_agent_name = fallback.clone();
                        resolve_agent(&self.state, &context.user_id, fallback).await?
                    } else {
                        return Err(e);
                    }
                }
            };

            // Convert UserAgent to AgentConfig
            let agent_config = AgentConfig {
                model: user_agent.model.clone(),
                system_prompt: user_agent.system_prompt.clone(),
                tools: user_agent.tools_vec(),
                max_tool_iterations: user_agent.max_tool_iterations as usize,
                parallel_tools: user_agent.parallel_tools,
                extra: std::collections::HashMap::new(),
            };

            // Create the agent
            let agent = self.state.agent_registry.create_agent_from_config(&current_agent_name, &agent_config).await?;

            // Execute the agent
            let output = agent.execute(&current_input, context).await?;
            let duration_ms = step_start.elapsed().as_millis() as u64;

            // Record this step
            steps.push(WorkflowStep {
                agent_name: current_agent_name.clone(),
                input: current_input.clone(),
                output: output.clone(),
                timestamp,
                duration_ms,
            });

            if !agents_used.contains(&current_agent_name) {
                agents_used.push(current_agent_name.clone());
            }

            // Check if the agent is a router and needs to delegate
            if agent.agent_type() == AgentType::Router {
                // Router's output should be an agent name
                // Use robust parsing to handle various output formats
                let next_agent = Self::parse_routing_decision(&output);

                if let Some(ref agent_name) = next_agent {
                    // Validate the routed agent exists (check hierarchy)
                    if resolve_agent(&self.state, &context.user_id, agent_name).await.is_ok() {
                        current_agent_name = agent_name.clone();
                        // Keep the original user input for the routed agent
                        depth += 1;
                        continue;
                    }
                }

                // Agent not found or couldn't parse - try fallback
                if let Some(ref fallback) = workflow.fallback_agent {
                    // Use fallback if routed agent doesn't exist
                    tracing::warn!(
                        "Routed agent '{:?}' not found or invalid, using fallback '{}'",
                        next_agent,
                        fallback
                    );
                    current_agent_name = fallback.clone();
                    depth += 1;
                    continue;
                } else {
                    // No fallback, return the router's output as final
                    break;
                }
            }

            // Non-router agent - this is the final response
            break;
        }

        // Build the final output
        let final_response = steps
            .last()
            .map(|s| s.output.clone())
            .unwrap_or_else(|| "No response generated".to_string());

        Ok(WorkflowOutput {
            final_response,
            steps_executed: steps.len(),
            agents_used,
            reasoning_path: steps,
        })
    }

    /// Get available workflow names
    pub fn available_workflows(&self) -> Vec<String> {
        self.state.config_manager.config().workflows.keys().cloned().collect()
    }

    /// Check if a workflow exists
    pub fn has_workflow(&self, name: &str) -> bool {
        self.state.config_manager.config().workflows.contains_key(name)
    }

    /// Get workflow configuration
    pub fn get_workflow_config(&self, name: &str) -> Option<WorkflowConfig> {
        self.state.config_manager.config().get_workflow(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ProviderRegistry;
    use crate::tools::registry::ToolRegistry;
    use crate::utils::toml_config::{
        AgentConfig, AuthConfig, DatabaseConfig, ModelConfig, ProviderConfig, RagConfig,
        ServerConfig, AresConfig,
    };
    use crate::{AgentRegistry, AresConfigManager, DynamicConfigManager};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn create_test_config() -> AresConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "ollama-local".to_string(),
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );

        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );

        let mut agents = HashMap::new();
        agents.insert(
            "router".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Route queries to the appropriate agent.".to_string()),
                tools: vec![],
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        agents.insert(
            "orchestrator".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Handle complex queries.".to_string()),
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        agents.insert(
            "product".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Handle product queries.".to_string()),
                tools: vec![],
                max_tool_iterations: 5,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        let mut workflows = HashMap::new();
        workflows.insert(
            "default".to_string(),
            WorkflowConfig {
                entry_agent: "router".to_string(),
                fallback_agent: Some("orchestrator".to_string()),
                max_depth: 3,
                max_iterations: 5,
                parallel_subagents: false,
            },
        );
        workflows.insert(
            "research".to_string(),
            WorkflowConfig {
                entry_agent: "orchestrator".to_string(),
                fallback_agent: None,
                max_depth: 3,
                max_iterations: 10,
                parallel_subagents: true,
            },
        );

        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            config: crate::utils::toml_config::DynamicConfigPaths::default(),
            providers,
            models,
            tools: HashMap::new(),
            agents,
            workflows,
            rag: RagConfig::default(),
        }
    }

    #[test]
    fn test_workflow_engine_creation() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        // Create a dummy AppState for testing
        let state = AppState {
            config_manager: Arc::new(AresConfigManager::from_config((*config).clone())),
            dynamic_config: Arc::new(DynamicConfigManager::new(
                std::path::PathBuf::from("config/agents"),
                std::path::PathBuf::from("config/models"),
                std::path::PathBuf::from("config/tools"),
                std::path::PathBuf::from("config/workflows"),
                std::path::PathBuf::from("config/mcps"),
                false,
            ).unwrap()),
            turso: Arc::new(futures::executor::block_on(crate::db::TursoClient::new_memory()).unwrap()),
            llm_factory: Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")),
            provider_registry,
            agent_registry,
            tool_registry,
            auth_service: Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800)),
        };

        let engine = WorkflowEngine::new(state);

        assert!(engine.has_workflow("default"));
        assert!(engine.has_workflow("research"));
        assert!(!engine.has_workflow("nonexistent"));
    }

    #[test]
    fn test_available_workflows() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        // Create a dummy AppState for testing
        let state = AppState {
            config_manager: Arc::new(AresConfigManager::from_config((*config).clone())),
            dynamic_config: Arc::new(DynamicConfigManager::new(
                std::path::PathBuf::from("config/agents"),
                std::path::PathBuf::from("config/models"),
                std::path::PathBuf::from("config/tools"),
                std::path::PathBuf::from("config/workflows"),
                std::path::PathBuf::from("config/mcps"),
                false,
            ).unwrap()),
            turso: Arc::new(futures::executor::block_on(crate::db::TursoClient::new_memory()).unwrap()),
            llm_factory: Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")),
            provider_registry,
            agent_registry,
            tool_registry,
            auth_service: Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800)),
        };

        let engine = WorkflowEngine::new(state);
        let workflows = engine.available_workflows();

        assert!(workflows.contains(&"default".to_string()));
        assert!(workflows.contains(&"research".to_string()));
    }

    #[test]
    fn test_get_workflow_config() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        // Create a dummy AppState for testing
        let state = AppState {
            config_manager: Arc::new(AresConfigManager::from_config((*config).clone())),
            dynamic_config: Arc::new(DynamicConfigManager::new(
                std::path::PathBuf::from("config/agents"),
                std::path::PathBuf::from("config/models"),
                std::path::PathBuf::from("config/tools"),
                std::path::PathBuf::from("config/workflows"),
                std::path::PathBuf::from("config/mcps"),
                false,
            ).unwrap()),
            turso: Arc::new(futures::executor::block_on(crate::db::TursoClient::new_memory()).unwrap()),
            llm_factory: Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")),
            provider_registry,
            agent_registry,
            tool_registry,
            auth_service: Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800)),
        };

        let engine = WorkflowEngine::new(state);

        let default_config = engine.get_workflow_config("default").unwrap();
        assert_eq!(default_config.entry_agent, "router");
        assert_eq!(
            default_config.fallback_agent,
            Some("orchestrator".to_string())
        );
        assert_eq!(default_config.max_depth, 3);

        let research_config = engine.get_workflow_config("research").unwrap();
        assert_eq!(research_config.entry_agent, "orchestrator");
        assert!(research_config.parallel_subagents);
    }

    #[test]
    fn test_workflow_output_serialization() {
        let output = WorkflowOutput {
            final_response: "Test response".to_string(),
            steps_executed: 2,
            agents_used: vec!["router".to_string(), "product".to_string()],
            reasoning_path: vec![
                WorkflowStep {
                    agent_name: "router".to_string(),
                    input: "What products do we have?".to_string(),
                    output: "product".to_string(),
                    timestamp: 1702500000,
                    duration_ms: 150,
                },
                WorkflowStep {
                    agent_name: "product".to_string(),
                    input: "What products do we have?".to_string(),
                    output: "Test response".to_string(),
                    timestamp: 1702500001,
                    duration_ms: 500,
                },
            ],
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("Test response"));
        assert!(json.contains("router"));
        assert!(json.contains("product"));

        let deserialized: WorkflowOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.steps_executed, 2);
    }
}
