//! Workflow Engine
//!
//! Executes declarative workflows by orchestrating agent execution based on
//! TOML configuration.

use crate::agents::{Agent, AgentRegistry};
use crate::types::{AgentContext, AgentType, AppError, Result};
use crate::utils::toml_config::{AresConfig, WorkflowConfig};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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

/// Workflow engine that orchestrates agent execution
pub struct WorkflowEngine {
    /// Agent registry for creating agents
    agent_registry: Arc<AgentRegistry>,
    /// Configuration reference
    config: Arc<AresConfig>,
}

impl WorkflowEngine {
    /// Create a new workflow engine
    pub fn new(agent_registry: Arc<AgentRegistry>, config: Arc<AresConfig>) -> Self {
        Self {
            agent_registry,
            config,
        }
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
        let workflow = self.config.get_workflow(workflow_name).ok_or_else(|| {
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

            // Create the agent
            let agent = match self.agent_registry.create_agent(&current_agent_name).await {
                Ok(agent) => agent,
                Err(e) => {
                    // Try fallback agent if available
                    if let Some(ref fallback) = workflow.fallback_agent {
                        tracing::warn!(
                            "Failed to create agent '{}', using fallback '{}'",
                            current_agent_name,
                            fallback
                        );
                        current_agent_name = fallback.clone();
                        self.agent_registry.create_agent(fallback).await?
                    } else {
                        return Err(e);
                    }
                }
            };

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
                let next_agent = output.trim().to_lowercase();

                // Validate the routed agent exists
                if self.agent_registry.has_agent(&next_agent) {
                    current_agent_name = next_agent;
                    // Keep the original user input for the routed agent
                    depth += 1;
                    continue;
                } else if let Some(ref fallback) = workflow.fallback_agent {
                    // Use fallback if routed agent doesn't exist
                    tracing::warn!(
                        "Routed agent '{}' not found, using fallback '{}'",
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
    pub fn available_workflows(&self) -> Vec<&str> {
        self.config.workflows.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a workflow exists
    pub fn has_workflow(&self, name: &str) -> bool {
        self.config.workflows.contains_key(name)
    }

    /// Get workflow configuration
    pub fn get_workflow_config(&self, name: &str) -> Option<&WorkflowConfig> {
        self.config.get_workflow(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ProviderRegistry;
    use crate::tools::registry::ToolRegistry;
    use crate::utils::toml_config::{
        AgentConfig, AuthConfig, DatabaseConfig, ModelConfig, ProviderConfig, RagConfig,
        ServerConfig, WorkflowConfig,
    };
    use std::collections::HashMap;

    fn create_test_config() -> AresConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "ollama-local".to_string(),
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "granite4:tiny-h".to_string(),
            },
        );

        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "granite4:tiny-h".to_string(),
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
            provider_registry,
            tool_registry,
        ));

        let engine = WorkflowEngine::new(agent_registry, config);

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
            provider_registry,
            tool_registry,
        ));

        let engine = WorkflowEngine::new(agent_registry, config);
        let workflows = engine.available_workflows();

        assert!(workflows.contains(&"default"));
        assert!(workflows.contains(&"research"));
    }

    #[test]
    fn test_get_workflow_config() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry,
            tool_registry,
        ));

        let engine = WorkflowEngine::new(agent_registry, config);

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
