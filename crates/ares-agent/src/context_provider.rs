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

/// Runtime metadata available to managed context providers.
///
/// Public ARES treats these fields as caller-supplied metadata. Managed
/// providers must still validate workspace use against their own binding or
/// auth policy before using it for external memory fetches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentRuntimeContext {
    /// Tenant that owns the agent execution.
    pub tenant_id: String,
    /// Registry or tenant-agent name being executed.
    pub agent_name: String,
    /// Optional workspace selected by the authenticated upstream runtime.
    pub workspace_id: Option<String>,
    /// Optional end-user identifier for user-scoped products.
    pub user_id: Option<String>,
    /// Optional session or conversation identifier for this run.
    pub session_id: Option<String>,
    /// Logical source of the request, such as an API handler name.
    pub request_source: String,
}

impl AgentRuntimeContext {
    /// Build runtime metadata with required tenant, agent, and source fields.
    pub fn new(
        tenant_id: impl Into<String>,
        agent_name: impl Into<String>,
        request_source: impl Into<String>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            agent_name: agent_name.into(),
            request_source: request_source.into(),
            ..Self::default()
        }
    }
}

/// Trait for injecting external context into agent calls.
///
/// Called before every LLM invocation with the agent name and tenant ID.
/// Returns `None` if no external context is available.
#[async_trait]
pub trait ContextProvider: Send + Sync + 'static {
    /// Get context for a specific agent and tenant.
    async fn get_context(&self, agent_name: &str, tenant_id: &str) -> Option<String> {
        let runtime = AgentRuntimeContext::new(tenant_id, agent_name, "legacy_context_provider");
        self.get_context_for_run(&runtime).await
    }

    /// Get context using the full runtime metadata when available.
    async fn get_context_for_run(&self, runtime: &AgentRuntimeContext) -> Option<String>;
}

/// Default: no external context (pure OSS mode).
///
/// Agents run with only their configured system prompt.
pub struct NoOpContextProvider;

#[async_trait]
impl ContextProvider for NoOpContextProvider {
    async fn get_context_for_run(&self, _runtime: &AgentRuntimeContext) -> Option<String> {
        None
    }
}

/// Cordis-native handle for the process-wide ContextProvider.
#[derive(Clone)]
pub struct ContextProviderHandle(pub std::sync::Arc<dyn ContextProvider>);

impl cordis::Service for ContextProviderHandle {
    fn name(&self) -> &'static str {
        "context_provider"
    }
    fn init(
        &self,
        _ctx: &std::sync::Arc<cordis::Context>,
    ) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

impl ContextProviderHandle {
    pub fn new(inner: std::sync::Arc<dyn ContextProvider>) -> Self {
        Self(inner)
    }
    pub fn inner(&self) -> &std::sync::Arc<dyn ContextProvider> {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingContextProvider {
        last_runtime: Mutex<Option<AgentRuntimeContext>>,
        response: Option<String>,
    }

    #[async_trait]
    impl ContextProvider for RecordingContextProvider {
        async fn get_context_for_run(&self, runtime: &AgentRuntimeContext) -> Option<String> {
            *self.last_runtime.lock().unwrap() = Some(runtime.clone());
            self.response.clone()
        }
    }

    struct SelectiveContextProvider;

    #[async_trait]
    impl ContextProvider for SelectiveContextProvider {
        async fn get_context_for_run(&self, runtime: &AgentRuntimeContext) -> Option<String> {
            match (runtime.tenant_id.as_str(), runtime.agent_name.as_str()) {
                ("tenant-a", "agent-x") => Some(format!(
                    "workspace={:?}",
                    runtime.workspace_id.as_deref().unwrap_or("none")
                )),
                ("tenant-b", _) => Some("tenant-b default".into()),
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn test_noop_returns_none() {
        let provider = NoOpContextProvider;
        let result = provider.get_context("any_agent", "any_tenant").await;
        assert!(result.is_none(), "NoOp should always return None");
    }

    #[test]
    fn runtime_context_default_has_empty_strings_and_none_optionals() {
        let runtime = AgentRuntimeContext::default();
        assert_eq!(runtime.tenant_id, "");
        assert_eq!(runtime.agent_name, "");
        assert_eq!(runtime.request_source, "");
        assert_eq!(runtime.workspace_id, None);
        assert_eq!(runtime.user_id, None);
        assert_eq!(runtime.session_id, None);
    }

    #[test]
    fn runtime_context_new_sets_required_fields() {
        let runtime = AgentRuntimeContext::new("tenant-1", "agent-1", "api_v1_chat");
        assert_eq!(runtime.tenant_id, "tenant-1");
        assert_eq!(runtime.agent_name, "agent-1");
        assert_eq!(runtime.request_source, "api_v1_chat");
        assert_eq!(runtime.workspace_id, None);
        assert_eq!(runtime.user_id, None);
        assert_eq!(runtime.session_id, None);
    }

    #[tokio::test]
    async fn test_noop_get_context_for_run_returns_none() {
        let provider = NoOpContextProvider;
        let runtime = AgentRuntimeContext::new("tenant-1", "agent-1", "unit_test");
        assert!(provider.get_context_for_run(&runtime).await.is_none());
    }

    #[test]
    fn runtime_context_optional_fields_round_trip() {
        let runtime = AgentRuntimeContext {
            tenant_id: "tenant-1".into(),
            agent_name: "agent-1".into(),
            workspace_id: Some("ws-9".into()),
            user_id: Some("user-42".into()),
            session_id: Some("sess-7".into()),
            request_source: "orchestrator".into(),
        };
        assert_eq!(runtime.workspace_id.as_deref(), Some("ws-9"));
        assert_eq!(runtime.user_id.as_deref(), Some("user-42"));
        assert_eq!(runtime.session_id.as_deref(), Some("sess-7"));
    }

    #[tokio::test]
    async fn get_context_builds_legacy_runtime_for_resolution() {
        let provider = RecordingContextProvider {
            last_runtime: Mutex::new(None),
            response: Some("injected".into()),
        };

        let resolved = provider.get_context("my-agent", "my-tenant").await;
        assert_eq!(resolved.as_deref(), Some("injected"));

        let runtime = provider.last_runtime.lock().unwrap().take().unwrap();
        assert_eq!(runtime.tenant_id, "my-tenant");
        assert_eq!(runtime.agent_name, "my-agent");
        assert_eq!(runtime.request_source, "legacy_context_provider");
    }

    #[tokio::test]
    async fn context_resolution_uses_runtime_metadata() {
        let provider = SelectiveContextProvider;

        let mut runtime =
            AgentRuntimeContext::new("tenant-a", "agent-x", "managed_platform");
        runtime.workspace_id = Some("ws-1".into());

        let resolved = provider.get_context_for_run(&runtime).await;
        assert_eq!(resolved.as_deref(), Some("workspace=\"ws-1\""));

        let unknown =
            AgentRuntimeContext::new("tenant-z", "agent-x", "managed_platform");
        assert!(provider.get_context_for_run(&unknown).await.is_none());

        let tenant_default =
            AgentRuntimeContext::new("tenant-b", "any-agent", "managed_platform");
        assert_eq!(
            provider.get_context_for_run(&tenant_default).await.as_deref(),
            Some("tenant-b default")
        );
    }

    #[tokio::test]
    async fn test_noop_is_send_sync() {
        // Verify the trait object can be shared across threads
        let provider: Box<dyn ContextProvider> = Box::new(NoOpContextProvider);
        let arc = std::sync::Arc::new(provider);
        let _clone = arc.clone();
    }

    #[test]
    fn context_provider_handle_readable_via_cordis() {
        let ctx = cordis::Context::new_root();
        ctx.provide(ContextProviderHandle::new(std::sync::Arc::new(
            NoOpContextProvider,
        )));
        assert!(ctx.get::<ContextProviderHandle>().is_some());
    }
}
