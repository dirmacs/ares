//! Workflow Engine Module
//!
//! This module provides declarative workflow execution based on TOML configuration.
//! Workflows define how agents work together to handle complex requests.
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
//! let engine = WorkflowEngine::new(agent_registry, config);
//! let output = engine.execute_workflow("default", "What's our revenue?").await?;
//! println!("Final response: {}", output.final_response);
//! println!("Agents used: {:?}", output.agents_used);
//! ```

pub mod engine;

pub use engine::{WorkflowEngine, WorkflowOutput, WorkflowStep};
