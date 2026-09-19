//! AI agent orchestration and management.
//!
//! This module provides the agent system for A.R.E.S, including:
//!
//! - **Agent Trait** - Base trait that all agents implement
//! - **ConfigurableAgent** - Dynamic agent created from TOML/TOON configuration
//! - **AgentRegistry** - Registry for creating and managing agent instances
//! - **Router** - Routes requests to appropriate specialized agents
//! - **Orchestrator** - Coordinates multi-step agent workflows
//!
#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(deprecated)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::explicit_counter_loop)]

//! ## Architecture
//!
//! All agents are now created dynamically via `ConfigurableAgent`, which reads
//! configuration from TOML files. Legacy hardcoded agents have been removed.
//!
//! ## Example
//!
//! ```rust,ignore
//! use ares::agents::{Agent, AgentRegistry};
//!
//! // Create registry from configuration
//! let registry = AgentRegistry::from_config(config.agents, provider_registry, tools);
//!
//! // Get an agent instance
//! let agent = registry.get_agent("product")?;
//!
//! // Execute with context
//! let response = agent.execute("Help me with my order", &context).await?;
//! ```

pub mod config;
pub mod workflows_config;
pub use config::AgentConfig;
pub use workflows_config::{SkillsTomlConfig, WorkflowConfig};

/// Live TOON agent lookup used by [`AgentRegistry`] without depending on Overlay.
pub trait ToonAgents: Send + Sync {
    /// Agent config by name, already converted from TOON.
    fn get(&self, name: &str) -> Option<AgentConfig>;
    /// Names present in the TOON set.
    fn names(&self) -> Vec<String>;
}

pub mod admit;
/// Checkpoint/crash recovery — serialize agent state, restore on restart.
pub mod checkpoint;
pub mod configurable;
/// External context injection trait (OSS: NoOp, Managed: Eruka/custom).
pub mod context_provider;
pub mod emergency_stop;
pub mod execution;
pub mod external_context;
/// Loop detection for agent outputs — prevents repetitive/stuck agents.
pub mod loop_detector;
/// Long-running iteration mode — agents that run on a fixed interval.
pub mod loop_mode;
pub mod memory;
/// Multi-agent orchestration for complex tasks.
pub mod orchestrator;
pub mod plugins;
pub mod registry;
pub mod research;
#[cfg(feature = "postgres")]
pub(crate) mod resolver;
/// Request routing to specialized agents.
pub mod router;
/// Per-tenant agent creation from DB-stored configs.
#[cfg(feature = "postgres")]
pub mod tenant_agent;
pub use emergency_stop::EmergencyStop;
#[cfg(feature = "pipeline")]
pub mod pipeline;
#[cfg(feature = "scheduler")]
pub mod scheduler;
#[cfg(any(feature = "postgres", feature = "skills"))]
pub mod skills;
#[cfg(feature = "trigger")]
pub mod trigger;
#[cfg(feature = "workflows")]
pub mod workflows;
pub use admit::admit;
pub use execution::{
    request_tenant_ctx, request_user_scope, tenant_scope, user_id_from_ctx, AgentRequest,
    AgentSource, Execute, ExecutionResult, RunTracker,
};
pub use external_context::ExternalContext;
pub use plugins::register_plugins;

use ares_llm::client::TokenUsage;
use ares_types::types::{AgentContext, AgentType, AppError, Result};
use async_trait::async_trait;
use cordis::CordisError;
use std::sync::Arc;

// Re-export commonly used types
pub use configurable::ConfigurableAgent;
pub use context_provider::{ContextProvider, ContextProviderHandle, NoOpContextProvider};
pub use registry::{AgentRegistry, AgentRegistryBuilder};
#[cfg(feature = "postgres")]
pub use resolver::TenantId;

/// Response from agent execution, including content and optional token usage
#[derive(Debug, Clone, Default)]
pub struct AgentResponse {
    /// The generated text response
    pub content: String,
    /// Token usage from the LLM provider (None if unavailable)
    pub usage: Option<TokenUsage>,
    /// Metadata about the execution (model, provider, etc.)
    pub metadata: Option<ExecutionMetadata>,
}

/// Metadata about the execution of an agent
#[derive(Debug, Clone, Default)]
pub struct ExecutionMetadata {
    /// The name of the model used
    pub model_name: String,
    /// The name of the provider used
    pub provider_name: String,
}

/// Base trait for all agents
#[async_trait]
pub trait Agent: Send + Sync {
    /// Execute the agent with given input and context
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<AgentResponse>;

    /// Get the agent's system prompt
    fn system_prompt(&self) -> String;

    /// Get the agent type
    fn agent_type(&self) -> AgentType;
}

/// Side slot that carries a typed [`AppError`] across a cordis waterfall.
///
/// [`CordisError`] carries strings only, so errors are stringified on their
/// way through waterfall machinery. A core stores the typed error here
/// before converting; [`preserved_or_wrapped`] adopts it back when the
/// returned fiber proves provenance.
pub(crate) type TypedErrorSlot = parking_lot::Mutex<Option<(String, AppError)>>;

/// Create an empty [`TypedErrorSlot`] for one waterfall call.
pub(crate) fn typed_error_slot() -> Arc<TypedErrorSlot> {
    Arc::new(parking_lot::Mutex::new(None))
}

/// Recover the core's typed [`AppError`] when the waterfall handed back
/// exactly the fiber error the core produced.
///
/// The fiber message proves provenance; any listener rewrite of the error
/// falls back to the wrapped [`AppError::Internal`], which keeps the
/// pre-existing behavior for every no-match case.
pub(crate) fn preserved_or_wrapped(err: CordisError, slot: &TypedErrorSlot) -> AppError {
    if let Some((msg, app)) = slot.lock().take() {
        if matches!(&err, CordisError::Fiber(m) if m == &msg) {
            return app;
        }
    }
    AppError::Internal(err.to_string())
}

#[cfg(test)]
mod typed_error_slot_tests {
    use super::*;

    #[test]
    fn adopts_matching_fiber() {
        let slot = typed_error_slot();
        *slot.lock() = Some(("boom".to_string(), AppError::RateLimited("q".to_string())));
        let err = preserved_or_wrapped(CordisError::Fiber("boom".to_string()), &slot);
        assert!(
            matches!(err, AppError::RateLimited(_)),
            "matching fiber must adopt the typed error; got {err:?}"
        );
    }

    #[test]
    fn wraps_when_fiber_differs() {
        let slot = typed_error_slot();
        *slot.lock() = Some(("boom".to_string(), AppError::Unavailable("q".to_string())));
        let err = preserved_or_wrapped(CordisError::Fiber("other".to_string()), &slot);
        assert!(
            matches!(err, AppError::Internal(_)),
            "a rewritten fiber falls back to the wrapped error; got {err:?}"
        );
    }

    #[test]
    fn wraps_when_slot_empty() {
        let slot = typed_error_slot();
        let err = preserved_or_wrapped(CordisError::Fiber("x".to_string()), &slot);
        assert!(
            matches!(err, AppError::Internal(_)),
            "an empty slot falls back to the wrapped error; got {err:?}"
        );
    }
}
