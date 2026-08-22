//! Workflow Engine
//!
//! Executes declarative workflows by orchestrating agent execution based on
//! TOML configuration.

use crate::execution::{AgentRequest, Execute};
use crate::workflows_config::WorkflowConfig;
use ares_types::types::{AgentContext, AppError, Result};
use chrono::Utc;
use cordis::{Context, Plugin, Service};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Output from a workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// A single step in the workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Named workflow map provided on context or passed into [`WorkflowEngine`].
#[derive(Clone, Default)]
pub struct WorkflowSet {
    /// Name → workflow config.
    pub workflows: HashMap<String, WorkflowConfig>,
}

impl Service for WorkflowSet {
    fn name(&self) -> &'static str {
        "workflows"
    }
}

/// Valid agent names for routing.
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

/// Workflow engine that orchestrates agent execution via [`Execute`].
pub struct WorkflowEngine {
    /// Cordis context for resolving agents.
    pub ctx: Arc<Context>,
    workflows: HashMap<String, WorkflowConfig>,
}

impl Service for WorkflowEngine {
    fn name(&self) -> &'static str {
        "workflow"
    }
    fn check(&self) -> bool {
        true
    }
}

impl WorkflowEngine {
    /// Create a new workflow engine from a Cordis context.
    ///
    /// Workflows come from [`WorkflowSet`] on `ctx` when present.
    pub fn new(ctx: Arc<Context>) -> Self {
        let workflows = ctx
            .get::<WorkflowSet>()
            .map(|s| s.workflows.clone())
            .unwrap_or_default();
        Self { ctx, workflows }
    }

    /// Construct with an explicit workflow map (HTTP overlay copies toml here).
    pub fn with_config(ctx: Arc<Context>, workflows: HashMap<String, WorkflowConfig>) -> Self {
        Self { ctx, workflows }
    }

    /// Legacy alias for `new`.
    #[allow(dead_code)]
    pub fn from_ctx(ctx: Arc<Context>) -> Self {
        Self::new(ctx)
    }

    /// Parse routing decision from router output.
    fn parse_routing_decision(output: &str) -> Option<String> {
        let trimmed = output.trim().to_lowercase();

        if VALID_AGENTS.contains(&trimmed.as_str()) {
            return Some(trimmed);
        }

        for word in trimmed.split(|c: char| c.is_whitespace() || c == ':' || c == ',' || c == '.') {
            let word = word.trim();
            if VALID_AGENTS.contains(&word) {
                return Some(word.to_string());
            }
        }

        for agent in VALID_AGENTS {
            if trimmed.contains(agent) {
                return Some(agent.to_string());
            }
        }

        None
    }

    /// Execute a workflow by name using [`Execute::run`].
    pub async fn execute_workflow(
        &self,
        workflow_name: &str,
        user_input: &str,
        context: &AgentContext,
    ) -> Result<WorkflowOutput> {
        let workflow = self
            .workflows
            .get(workflow_name)
            .cloned()
            .ok_or_else(|| {
                AppError::Configuration(format!(
                    "Workflow '{workflow_name}' not found in configuration"
                ))
            })?;

        let exec = self.ctx.get::<Execute>().ok_or_else(|| {
            AppError::Configuration("Execute is not on context".into())
        })?;
        let scoped = crate::tenant_scope(&self.ctx, &context.user_id);

        let mut steps = Vec::new();
        let mut agents_used = Vec::new();
        let current_input = user_input.to_string();
        let mut current_agent_name = workflow.entry_agent.clone();
        let mut depth = 0;

        while depth < workflow.max_depth {
            let step_start = std::time::Instant::now();
            let timestamp = Utc::now().timestamp();

            let req = AgentRequest {
                agent_name: current_agent_name.clone(),
                message: current_input.clone(),
                history: Vec::new(),
                ctx_provider: None,
            };
            let run = match exec.run(&req, &scoped).await {
                Ok(r) => r,
                Err(e) => {
                    if let Some(fallback) = &workflow.fallback_agent {
                        tracing::warn!(
                            "Failed to run agent '{}', using fallback '{}'",
                            current_agent_name,
                            fallback
                        );
                        current_agent_name = fallback.clone();
                        let req = AgentRequest {
                            agent_name: current_agent_name.clone(),
                            message: current_input.clone(),
                            history: Vec::new(),
                            ctx_provider: None,
                        };
                        exec.run(&req, &scoped).await?
                    } else {
                        return Err(e);
                    }
                }
            };
            let output = run.response.content;
            let duration_ms = step_start.elapsed().as_millis() as u64;

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

            if current_agent_name == "router" {
                let next_agent = Self::parse_routing_decision(&output);
                if let Some(agent_name) = &next_agent {
                    current_agent_name = agent_name.clone();
                    depth += 1;
                    continue;
                }
                if let Some(fallback) = &workflow.fallback_agent {
                    tracing::warn!(
                        "Routed agent '{:?}' not found or invalid, using fallback '{}'",
                        next_agent,
                        fallback
                    );
                    current_agent_name = fallback.clone();
                    depth += 1;
                    continue;
                }
                break;
            }
            break;
        }

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

    /// Get available workflow names.
    pub fn available_workflows(&self) -> Vec<String> {
        let mut names: Vec<String> = self.workflows.keys().cloned().collect();
        names.sort();
        names
    }

    /// Check if a workflow exists.
    pub fn has_workflow(&self, name: &str) -> bool {
        self.workflows.contains_key(name)
    }

    /// Get workflow configuration.
    pub fn get_workflow_config(&self, name: &str) -> Option<WorkflowConfig> {
        self.workflows.get(name).cloned()
    }
}

/// Typed installer for [`WorkflowEngine`]. No loader key (constructed per request).
pub struct WorkflowPlugin;

impl Plugin for WorkflowPlugin {
    type Config = HashMap<String, WorkflowConfig>;
    type Provides = WorkflowEngine;

    fn apply(
        &self,
        ctx: &Arc<Context>,
        config: Self::Config,
    ) -> std::result::Result<Arc<Self::Provides>, cordis::CordisError> {
        ctx.provide(WorkflowSet {
            workflows: config.clone(),
        });
        Ok(Arc::new(WorkflowEngine::with_config(ctx.clone(), config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_routing_decision_exact_match() {
        assert_eq!(
            WorkflowEngine::parse_routing_decision("product").as_deref(),
            Some("product")
        );
        assert_eq!(
            WorkflowEngine::parse_routing_decision("  SALES  ").as_deref(),
            Some("sales")
        );
    }

    #[test]
    fn test_parse_routing_decision_word_split() {
        assert_eq!(
            WorkflowEngine::parse_routing_decision("route to: product.").as_deref(),
            Some("product")
        );
        assert_eq!(
            WorkflowEngine::parse_routing_decision("invoice agent").as_deref(),
            Some("invoice")
        );
    }

    #[test]
    fn test_parse_routing_decision_substring_match() {
        assert_eq!(
            WorkflowEngine::parse_routing_decision("I would route this to finance").as_deref(),
            Some("finance")
        );
    }

    #[test]
    fn test_parse_routing_decision_none_for_unknown() {
        assert!(WorkflowEngine::parse_routing_decision("unknown-agent").is_none());
        assert!(WorkflowEngine::parse_routing_decision("").is_none());
    }

    #[test]
    fn workflow_engine_reads_workflow_set() {
        let ctx = Context::new_root();
        let mut workflows = HashMap::new();
        workflows.insert(
            "default".into(),
            WorkflowConfig {
                entry_agent: "router".into(),
                fallback_agent: Some("orchestrator".into()),
                max_depth: 3,
                max_iterations: 5,
                parallel_subagents: false,
            },
        );
        ctx.provide(WorkflowSet {
            workflows: workflows.clone(),
        });
        let engine = WorkflowEngine::new(ctx);
        assert!(engine.has_workflow("default"));
        assert!(!engine.has_workflow("missing"));
        assert_eq!(engine.available_workflows(), vec!["default".to_string()]);
        assert_eq!(
            engine.get_workflow_config("default").unwrap().entry_agent,
            "router"
        );
    }
}
