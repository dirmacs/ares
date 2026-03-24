//! External context injection for agents.
//!
//! The `ContextProvider` trait allows external systems to inject context
//! into agent calls before LLM invocation. This is the extension point
//! that separates generic ARES from managed platform features.
//!
//! ## OSS Mode
//!
//! By default, ARES uses `NoOpContextProvider` which returns `None`.
//! Agents run with only their system prompt — no external context.
//!
//! ## Managed Mode
//!
//! Platform extensions (e.g., dirmacs-core) implement `ContextProvider`
//! to inject knowledge states, gap constraints, or any external context
//! into the system prompt before every LLM call.

use async_trait::async_trait;

/// Trait for injecting external context into agent calls.
///
/// Called before every LLM invocation with the agent name and tenant ID.
/// Returns `None` if no external context is available.
#[async_trait]
pub trait ContextProvider: Send + Sync + 'static {
    /// Get context for a specific agent and tenant.
    async fn get_context(
        &self,
        agent_name: &str,
        tenant_id: &str,
    ) -> Option<String>;
}

/// Default: no external context (pure OSS mode).
///
/// Agents run with only their configured system prompt.
pub struct NoOpContextProvider;

#[async_trait]
impl ContextProvider for NoOpContextProvider {
    async fn get_context(&self, _agent_name: &str, _tenant_id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_noop_returns_none() {
        let provider = NoOpContextProvider;
        let result = provider.get_context("any_agent", "any_tenant").await;
        assert!(result.is_none(), "NoOp should always return None");
    }

    #[tokio::test]
    async fn test_noop_is_send_sync() {
        // Verify the trait object can be shared across threads
        let provider: Box<dyn ContextProvider> = Box::new(NoOpContextProvider);
        let arc = std::sync::Arc::new(provider);
        let _clone = arc.clone();
    }
}
