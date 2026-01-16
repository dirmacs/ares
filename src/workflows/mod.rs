//! Workflow Engine Module
//!
//! This module provides declarative workflow execution based on TOML configuration.
//! Workflows define how agents work together to handle complex requests.
//!
//! # Overview
//!
//! Workflows allow you to:
//! - Define agent routing and handoff patterns
//! - Set execution limits (depth, iterations)
//! - Enable parallel sub-agent execution
//! - Configure fallback behaviors
//!
//! # Configuration
//!
//! Workflows are defined in `ares.toml`:
//!
//! ```toml
//! [workflows.default]
//! entry_agent = "router"
//! fallback_agent = "orchestrator"
//! max_depth = 3
//! max_iterations = 5
//!
//! [workflows.research]
//! entry_agent = "orchestrator"
//! max_depth = 3
//! max_iterations = 10
//! parallel_subagents = true
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use ares::workflows::{WorkflowEngine, WorkflowOutput};
//!
//! let engine = WorkflowEngine::new(agent_registry, config);
//! let output = engine.execute_workflow("default", "What's our revenue?").await?;
//!
//! println!("Final response: {}", output.final_response);
//! println!("Agents used: {:?}", output.agents_used);
//! println!("Steps taken: {}", output.steps.len());
//! ```
//!
//! # Workflow Output
//!
//! The [`WorkflowOutput`] struct provides:
//! - `final_response` - The final synthesized response
//! - `agents_used` - List of agents that participated
//! - `steps` - Detailed log of each workflow step
//! - `total_tokens` - Aggregate token usage

pub mod engine;

pub use engine::{WorkflowEngine, WorkflowOutput, WorkflowStep};
