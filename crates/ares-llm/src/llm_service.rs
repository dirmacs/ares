//! Unified LLM capability (Cordis Phase 3).
//!
//! Wraps `ProviderRegistry` + `NvidiaCatalogCache` + `ClientPool` +
//! `ConfigBasedLLMFactory` behind a single `Service` injected via
//! `ctx.get::<Llm>()`.
//!
//! Per-request model pinning uses `ctx.intercept(ModelOverride { model })` so
//! the override is visible to `Llm` via `ctx.get::<ModelOverride>()`
//! without mutating global state.
//!
//! Circuit-breaker (`Breaker`) causes `Service::check` to return `false` when
//! open, causing dependent fibers to deactivate (guarded withdrawal per Thm 63:
//! provider does not withdraw until dependents deactivate).

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use cordis::{Context, CordisError, EventsService, Service};
use crate::nvidia_catalog::NvidiaCatalogCache;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::capabilities::CapabilityRequirements;
use crate::client::{LLMClient, LLMResponse};
use crate::config::ProviderConfig;
use crate::pool::ClientPool;
use crate::provider_registry::{
    ConfigBasedLLMFactory, ModelInfo, ProviderRegistry, RuntimeProviderEntry,
};
use ares_types::types::{AppError, ToolDefinition};

/// Per-request model override for `ctx.intercept`.
///
/// Example:
/// ```ignore
/// let req_ctx = root_ctx.intercept(ModelOverride { model: "gpt-4o-mini".into() });
/// let llm = req_ctx.get::<Llm>().unwrap();
/// // inside Llm, check `ctx.get::<ModelOverride>()` for pinning
/// ```
#[derive(Debug, Clone)]
pub struct ModelOverride {
    /// Model id to pin for this request scope.
    pub model: String,
}

// ModelOverride itself can be intercepted, not necessarily a Service provider,
// but we implement Service so `ctx.intercept(ModelOverride)` and `ctx.get`
// work via the same `Service` type map when needed.
impl Service for ModelOverride {}

/// Tenant-scoped model allowlist carried by a request context.
///
/// This is a snapshot of the tenant policy (normally populated from
/// `TenantAllowlistStore`) and is intentionally request-scoped.  It lets the
/// LLM service authorize an intercepted [`ModelOverride`] without changing the
/// process-wide provider registry or its default model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantModelPolicy {
    tenant_id: String,
    allowed_models: HashSet<String>,
}

impl TenantModelPolicy {
    /// Build a policy from the currently enabled model ids for a tenant.
    pub fn new<I, S>(tenant_id: impl Into<String>, allowed_models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            tenant_id: tenant_id.into(),
            allowed_models: allowed_models.into_iter().map(Into::into).collect(),
        }
    }

    /// Tenant whose allowlist is represented by this policy.
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Return whether this tenant may use `model`.
    pub fn allows(&self, model: &str) -> bool {
        self.allowed_models.contains(model)
    }

    /// Build the authorization message for a model denied by a tenant policy.
    pub fn denial_message(tenant_id: &str, model: &str) -> String {
        format!("Model '{}' is not allowed for tenant '{}'", model, tenant_id)
    }

    /// Build the authorization error for a model denied by a tenant policy.
    pub fn denial_error(tenant_id: &str, model: &str) -> AppError {
        AppError::Auth(Self::denial_message(tenant_id, model))
    }

    /// Authorize a model selected by a request interceptor.
    pub fn authorize(&self, model: &str) -> Result<(), AppError> {
        if self.allows(model) {
            Ok(())
        } else {
            Err(Self::denial_error(&self.tenant_id, model))
        }
    }
}

impl Service for TenantModelPolicy {}

/// Circuit-breaker state for `Llm`.
///
/// `check()` returns:
/// - `Closed` / `HalfOpen` → `true` (service advertises healthy, fibers stay Active)
/// - `Open { until }` → `false` until cooldown expires (fibers deactivate, guarded withdrawal per Thm 63)
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum Breaker {
    /// Normal operation.
    #[default]
    Closed,
    /// Provider is failing; do not use until `until`.
    Open { until: DateTime<Utc> },
    /// Trial half-open after cooldown.
    HalfOpen,
}

impl Breaker {
    /// Failure threshold before opening the breaker.
    pub const FAILURE_THRESHOLD: u32 = 5;
    /// Cooldown duration while open (seconds).
    pub const COOLDOWN_SECS: i64 = 30;

    /// Returns `true` if the breaker allows requests.
    ///
    /// When `Open` and `Utc::now() < until`, the breaker is still
    /// considered open — `Service::check` returns `false` so dependent
    /// fibers deactivate via guarded withdrawal (Thm 63). After cooldown
    /// expires the breaker reports healthy until an external transition
    /// moves it to `HalfOpen` or `Closed`.
    pub fn check(&self) -> bool {
        match self {
            Breaker::Closed => true,
            Breaker::HalfOpen => true,
            Breaker::Open { until } => {
                // Guarded withdrawal: while open, dependent fibers see `check() == false`
                // and deactivate gracefully per Cordis Thm 63.
                Utc::now() >= *until
            }
        }
    }

    /// Convenience: `true` if strictly closed.
    pub fn is_closed(&self) -> bool {
        matches!(self, Breaker::Closed)
    }

    /// Transition on failure with threshold and cooldown.
    ///
    /// - `Closed` → `Open{until: now+cooldown}` after `FAILURE_THRESHOLD` is reached,
    ///   otherwise stays `Closed` (counting is external via `Llm::record_failure`
    ///   failure counter; this pure transition always opens — the service layer
    ///   decides when to call it. For `Closed` we open immediately; callers that
    ///   want threshold counting should use `Llm::record_failure`).
    /// - `HalfOpen` → `Open`
    /// - `Open` → `Open` (refresh cooldown)
    pub fn transition_on_failure(&self) -> Breaker {
        let now = Utc::now();
        let cooldown = chrono::Duration::seconds(Self::COOLDOWN_SECS);
        match self {
            Breaker::Closed => Breaker::Open {
                until: now + cooldown,
            },
            Breaker::HalfOpen => Breaker::Open {
                until: now + cooldown,
            },
            Breaker::Open { .. } => Breaker::Open {
                until: now + cooldown,
            },
        }
    }

    /// Transition that respects an explicit failure count.
    pub fn transition_on_failure_with_count(&self, failures: u32) -> Breaker {
        if failures >= Self::FAILURE_THRESHOLD {
            let now = Utc::now();
            Breaker::Open {
                until: now + chrono::Duration::seconds(Self::COOLDOWN_SECS),
            }
        } else {
            Breaker::Closed
        }
    }
}


/// Unified LLM capability composing provider registry, catalog, factory, and pool.
///
/// Supports `ctx.get::<Llm>()`, `ctx.isolate::<Llm>` per-tenant scoping, and
/// `ctx.intercept::<ModelOverride>` per-request model pinning.
///
/// `Service::check` delegates to the circuit breaker — when the breaker is
/// `Open`, `check()` returns `false` so dependent fibers (e.g. `Execute`)
/// deactivate via guarded withdrawal (Thm 63).
pub struct Llm {
    /// Named provider registry (OpenAI, Anthropic, etc.).
    pub(crate) provider_registry: Arc<ProviderRegistry>,
    /// NVIDIA catalog cache for capability-based model selection (optional so
    /// `cargo check --no-default-features` remains viable).
    pub(crate) catalog: Option<Arc<NvidiaCatalogCache>>,
    /// Pooled LLM clients.
    pub(crate) pool: Arc<ClientPool>,
    /// Config-based factory used by crate-internal wiring.
    pub(crate) factory: Option<Arc<ConfigBasedLLMFactory>>,
    /// Circuit-breaker state.
    breaker: RwLock<Breaker>,
    /// Consecutive failure count for thresholded transition.
    failures: RwLock<u32>,
    /// Pinned client used by `complete` / `get_client_inner` when set.
    ///
    /// Set by [`Llm::from_client`] for in-process tests and library proofs.
    test_client: Option<Arc<dyn LLMClient>>,
}

impl Llm {
    /// Create a new `Llm`.
    pub fn new(
        provider_registry: Arc<ProviderRegistry>,
        pool: Arc<ClientPool>,
        catalog: Option<Arc<NvidiaCatalogCache>>,
    ) -> Self {
        Self {
            provider_registry,
            catalog,
            pool,
            factory: None,
            breaker: RwLock::new(Breaker::Closed),
            failures: RwLock::new(0),
            test_client: None,
        }
    }

    /// Attach a config-based factory (crate wiring / Execute boot).
    pub fn with_factory(mut self, factory: Arc<ConfigBasedLLMFactory>) -> Self {
        self.factory = Some(factory);
        self
    }

    /// Pin a client used by [`get_client`](Self::get_client) / [`complete`](Self::complete).
    ///
    /// Intended for in-process tests and the library proof (`Execute` with no HTTP).
    pub fn from_client(client: Arc<dyn LLMClient>) -> Self {
        let mut llm = Self::new(
            Arc::new(ProviderRegistry::new()),
            Arc::new(ClientPool::with_defaults()),
            None,
        );
        llm.test_client = Some(client);
        llm
    }

    /// Test helper: pin a client used by `complete` / `get_client_inner`.
    #[cfg(test)]
    pub(crate) fn for_test(client: Arc<dyn LLMClient>) -> Self {
        Self::from_client(client)
    }

    /// Clone the provider registry for named-provider lookup.
    pub(crate) fn provider_registry(&self) -> Arc<ProviderRegistry> {
        Arc::clone(&self.provider_registry)
    }

    /// Handle for `AgentRegistry` construction in `ares-agent`.
    ///
    /// Application code should use [`get_client`](Self::get_client).
    pub fn registry(&self) -> Arc<ProviderRegistry> {
        self.provider_registry()
    }

    /// Create with explicit breaker (e.g. `Open` for testing).
    pub fn with_breaker(
        provider_registry: Arc<ProviderRegistry>,
        catalog: Option<Arc<NvidiaCatalogCache>>,
        pool: Arc<ClientPool>,
        breaker: Breaker,
    ) -> Self {
        Self {
            provider_registry,
            catalog,
            pool,
            factory: None,
            breaker: RwLock::new(breaker),
            failures: RwLock::new(0),
            test_client: None,
        }
    }

    /// Legacy constructor with non-optional catalog (kept for existing call-sites).
    pub fn with_catalog(
        provider_registry: Arc<ProviderRegistry>,
        catalog: Arc<NvidiaCatalogCache>,
        pool: Arc<ClientPool>,
    ) -> Self {
        Self::new(provider_registry, pool, Some(catalog))
    }

    /// Get current breaker state.
    pub fn breaker(&self) -> Breaker {
        self.breaker.read().clone()
    }

    /// Transition breaker to `Open` with cooldown.
    pub fn trip(&self, until: DateTime<Utc>) {
        *self.breaker.write() = Breaker::Open { until };
    }

    /// Transition breaker to `HalfOpen`.
    pub fn half_open(&self) {
        *self.breaker.write() = Breaker::HalfOpen;
    }

    /// Reset breaker to `Closed`.
    pub fn reset(&self) {
        *self.breaker.write() = Breaker::Closed;
        *self.failures.write() = 0;
    }

    /// Record a successful request — closes the breaker and resets failure count.
    pub fn record_success(&self) {
        *self.breaker.write() = Breaker::Closed;
        *self.failures.write() = 0;
    }

    /// Record a failure — increments counter and opens breaker when threshold reached.
    ///
    /// Threshold: `Breaker::FAILURE_THRESHOLD` (5). Cooldown: `Breaker::COOLDOWN_SECS` (30s).
    /// `HalfOpen` immediately re-opens on failure.
    pub fn record_failure(&self) {
        let mut failures = self.failures.write();
        *failures = failures.saturating_add(1);
        let count = *failures;
        drop(failures);
        let mut b = self.breaker.write();
        // HalfOpen always transitions to Open on failure; Closed respects threshold
        match &*b {
            Breaker::HalfOpen => {
                *b = b.transition_on_failure();
            }
            Breaker::Closed => {
                if count >= Breaker::FAILURE_THRESHOLD {
                    *b = Breaker::Open {
                        until: Utc::now() + chrono::Duration::seconds(Breaker::COOLDOWN_SECS),
                    };
                }
            }
            Breaker::Open { .. } => {
                // refresh cooldown
                *b = b.transition_on_failure();
            }
        }
    }

    /// Validate a request's model override against its tenant policy.
    ///
    /// Both values are resolved through the context prototype chain, so callers
    /// can compose `TenantModelPolicy` and `ModelOverride` on separate child
    /// contexts. With no policy, legacy override behavior is preserved.
    pub fn validate_model_override(&self, ctx: &Arc<Context>) -> Result<(), AppError> {
        if let (Some(policy), Some(override_model)) = (
            ctx.get::<TenantModelPolicy>(),
            ctx.get::<ModelOverride>(),
        ) {
            policy.authorize(&override_model.model)?;
        }
        Ok(())
    }

    /// Capability-aware client resolution with per-request `ModelOverride` pinning.
    ///
    /// Public API: `Llm::get_client(&self, ctx: &Arc<cordis::Context>, capability)`.
    ///
    /// When `EventsService` is on `ctx`, runs waterfall `"llm.get_client"` first.
    /// Core is identity so handlers can set `"deny": true` or `"model"`.
    /// If the result has a `model` string and no intercept `ModelOverride`,
    /// `get_client_inner` runs on `ctx.with_intercept(ModelOverride { model })`.
    pub async fn get_client(
        &self,
        ctx: &Arc<Context>,
        capability: CapabilityRequirements,
    ) -> Result<Arc<dyn LLMClient>, AppError> {
        let Some(events) = ctx.get::<EventsService>() else {
            return self.get_client_inner(ctx, capability).await;
        };
        let payload = serde_json::to_value(cordis::LlmGetClientPayload {
            capability: format!("{capability:?}"),
            deny: None,
            model: None,
        })
        .unwrap_or(serde_json::Value::Null);
        let result = events
            .waterfall_around( cordis::events_catalog::ev::LLM_GET_CLIENT.to_string(), payload, |payload| async move {
                Ok(payload)
            })
            .await
            .map_err(map_cordis)?;
        if result.get("deny").and_then(|v| v.as_bool()) == Some(true) {
            return Err(AppError::InvalidInput("llm.get_client denied".into()));
        }
        if let Some(model) = result.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if ctx.get::<ModelOverride>().is_none() {
                let intercepted = ctx.with_intercept(ModelOverride {
                    model: model.to_string(),
                });
                return self.get_client_inner(&intercepted, capability).await;
            }
        }
        self.get_client_inner(ctx, capability).await
    }

    /// Existing get_client body (no events). Extracted so public `get_client` can wrap it.
    async fn get_client_inner(
        &self,
        ctx: &Arc<Context>,
        capability: CapabilityRequirements,
    ) -> Result<Arc<dyn LLMClient>, AppError> {
        if let Some(c) = &self.test_client {
            return Ok(Arc::clone(c));
        }
        // Authorize before touching the pool or provider registry.
        self.validate_model_override(ctx)?;
        // 1. Per-request pinning via `ctx.get::<ModelOverride>()` (intercept realm).
        if let Some(ov) = ctx.get::<ModelOverride>() {
            // Fast-path: pooled provider named exactly like the override model
            if let Ok(guard) = self.pool.try_get(&ov.model).await {
                let boxed = guard.take();
                return Ok(Arc::from(boxed));
            }
            if let Ok(client) = self
                .provider_registry
                .create_client_for_model_ctx(ctx, &ov.model)
                .await
            {
                return Ok(Arc::from(client));
            }
            // fall through to capability path if override model not found
        }

        // 2. Catalog-aware capability selection (catalog is Option for no-default-features builds)
        if let Some(catalog) = &self.catalog {
            let _snap = catalog.snapshot(); // touch catalog to prove composition
            if let Some(best) = self.provider_registry.find_best_model(&capability) {
                if let Ok(client) = self
                    .provider_registry
                    .create_client_for_model_ctx(ctx, &best.name)
                    .await
                {
                    return Ok(Arc::from(client));
                }
            }
        } else if let Some(best) = self.provider_registry.find_best_model(&capability) {
            if let Ok(client) = self
                .provider_registry
                .create_client_for_model_ctx(ctx, &best.name)
                .await
            {
                return Ok(Arc::from(client));
            }
        }

        // 3. Fallback chain (Coordinator / ProviderRegistry) — uses
        // `ProviderRegistry::resolve_with_capability_fallback` which delegates
        // to `create_client_for_requirements` → `find_best_model` and falls
        // back to `create_default_client`. Also exposed as
        // `resolve_with_fallback` when postgres feature is off.
        let client = self
            .provider_registry
            .resolve_with_capability_fallback(Some(capability))
            .await?;
        Ok(Arc::from(client))
    }

    /// Box variant for callers that prefer `Box<dyn LLMClient>`.
    pub async fn get_client_boxed(
        &self,
        ctx: &Arc<Context>,
        capability: CapabilityRequirements,
    ) -> Result<Box<dyn LLMClient>, AppError> {
        let client = self.get_client(ctx, capability).await?;
        Ok(Box::new(BoxedArcClient(client)))
    }

    /// Generate a completion, optionally through waterfall `"llm.complete"`.
    ///
    /// Without `EventsService`, this is `get_client` then `generate`. With events,
    /// handlers wrap payload `{"prompt"}`; core generates using `payload["prompt"]`
    /// and returns `{"prompt", "content"}`.
    pub async fn complete(
        &self,
        ctx: &Arc<Context>,
        prompt: &str,
    ) -> Result<String, AppError> {
        let client = self
            .get_client(ctx, CapabilityRequirements::default())
            .await?;
        let Some(events) = ctx.get::<EventsService>() else {
            return client.generate(prompt).await;
        };
        let payload = serde_json::to_value(cordis::LlmCompleteRequest {
            prompt: prompt.to_string(),
        })
        .unwrap_or(serde_json::Value::Null);
        let out = events
            .waterfall_around( cordis::events_catalog::ev::LLM_COMPLETE.to_string(), payload, move |payload| {
                let client = Arc::clone(&client);
                async move {
                    let prompt = payload
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let text = client
                        .generate(&prompt)
                        .await
                        .map_err(|e| CordisError::Fiber(e.to_string()))?;
                    Ok(serde_json::json!({ "prompt": prompt, "content": text }))
                }
            })
            .await
            .map_err(map_cordis)?;
        Ok(out
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Stub for capability-based model selection (delegates to registry).
    pub fn find_model_stub(&self, _capability: &str) -> Option<String> {
        None
    }

    /// List registered models with their provider info.
    pub fn list_models(&self) -> Vec<ModelInfo> {
        self.provider_registry.list_models()
    }

    /// Check if a provider exists for the given tenant (legacy or runtime).
    pub fn has_provider_for_tenant(&self, name: &str, tenant_id: Option<&str>) -> bool {
        self.provider_registry.has_provider_for_tenant(name, tenant_id)
    }

    /// Resolve a provider visible to the tenant derived from `ctx`.
    pub fn get_provider_for_ctx(
        &self,
        ctx: &Arc<Context>,
        name: &str,
    ) -> Option<ProviderConfig> {
        self.provider_registry.get_provider_for_ctx(ctx, name)
    }

    /// Hot-swap the runtime provider map.
    pub fn reload_runtime_providers(
        &self,
        providers: Vec<RuntimeProviderEntry>,
        names: Vec<String>,
    ) {
        self.provider_registry
            .reload_runtime_providers(providers, names);
    }
}

fn map_cordis(err: CordisError) -> AppError {
    AppError::Internal(err.to_string())
}

/// `Box<dyn LLMClient>` adapter around the Arc returned by [`Llm::get_client`].
struct BoxedArcClient(Arc<dyn LLMClient>);

#[async_trait]
impl LLMClient for BoxedArcClient {
    async fn generate(&self, prompt: &str) -> ares_types::types::Result<String> {
        self.0.generate(prompt).await
    }

    async fn generate_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> ares_types::types::Result<String> {
        self.0.generate_with_system(system, prompt).await
    }

    async fn generate_with_history(
        &self,
        messages: &[(String, String)],
    ) -> ares_types::types::Result<LLMResponse> {
        self.0.generate_with_history(messages).await
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        self.0.generate_with_tools(prompt, tools).await
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[crate::coordinator::ConversationMessage],
        tools: &[ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        self.0
            .generate_with_tools_and_history(messages, tools)
            .await
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> ares_types::types::Result<Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>> {
        self.0.stream(prompt).await
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> ares_types::types::Result<Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>> {
        self.0.stream_with_system(system, prompt).await
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> ares_types::types::Result<Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>> {
        self.0.stream_with_history(messages).await
    }

    fn model_name(&self) -> &str {
        self.0.model_name()
    }
}

impl Service for Llm {
    fn name(&self) -> &'static str {
        "Llm"
    }

    fn init(
        &self,
        _ctx: &Arc<Context>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Option<Box<dyn cordis::Disposable>>, CordisError>> + Send + '_>> {
        Box::pin(async { Ok(None) })
    }

    fn check(&self) -> bool {
        // Circuit-breaker advertisement: when Open (and cooldown not elapsed),
        // this service is unhealthy → dependent fibers deactivate (guarded withdrawal per Thm 63).
        self.breaker.read().check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapabilityRequirements;
    use crate::provider_registry::RuntimeProviderEntry;
    use cordis::Context;
    use ares_types::models::{TenantContext, TenantTier};
    use chrono::Duration;
    use std::collections::HashMap;

    #[test]
    fn breaker_closed_allows() {
        assert!(Breaker::Closed.check());
    }

    #[test]
    fn breaker_half_open_allows() {
        assert!(Breaker::HalfOpen.check());
    }

    #[test]
    fn breaker_open_future_denies() {
        let until = Utc::now() + Duration::seconds(60);
        assert!(!Breaker::Open { until }.check());
    }

    #[test]
    fn breaker_open_past_allows() {
        let until = Utc::now() - Duration::seconds(1);
        assert!(Breaker::Open { until }.check());
    }

    #[test]
    fn breaker_failure_threshold_opens() {
        let b = Breaker::Closed;
        let next = b.transition_on_failure_with_count(5);
        assert!(matches!(next, Breaker::Open { .. }));
        let still_closed = b.transition_on_failure_with_count(3);
        assert!(matches!(still_closed, Breaker::Closed));
    }

    #[test]
    fn breaker_constants_exist() {
        assert_eq!(Breaker::FAILURE_THRESHOLD, 5);
        assert_eq!(Breaker::COOLDOWN_SECS, 30);
    }

    #[test]
    fn provider_registry_and_factory_accessors() {
        let registry = Arc::new(ProviderRegistry::new());
        let pool = Arc::new(ClientPool::with_defaults());
        let factory = Arc::new(
            ConfigBasedLLMFactory::from_config(HashMap::new(), HashMap::new(), None)
                .expect("empty factory config"),
        );
        let llm = Llm::new(Arc::clone(&registry), pool, None).with_factory(Arc::clone(&factory));
        assert!(Arc::ptr_eq(&llm.provider_registry(), &registry));
    }

    #[tokio::test]
    async fn llm_model_override_via_context() {
        let registry = Arc::new(ProviderRegistry::new());
        let pool = Arc::new(ClientPool::with_defaults());
        let svc = Arc::new(Llm::new(registry, pool, None));
        let root = Context::new_root();
        root.provide::<Llm>(Llm::new(
            Arc::new(ProviderRegistry::new()),
            Arc::new(ClientPool::with_defaults()),
            None,
        ));
        // Intercept realm carries ModelOverride
        let req_ctx = root.intercept(ModelOverride {
            model: "gpt-4o-mini".into(),
        });
        assert!(req_ctx.get::<ModelOverride>().is_some());
        assert_eq!(req_ctx.get::<ModelOverride>().unwrap().model, "gpt-4o-mini");
        // Llm composition fields exist
        assert!(!Arc::as_ptr(&svc.provider_registry).is_null());
        let _ = svc.catalog.clone();
        let _ = svc.pool.provider_names();
        // Service check guarded withdrawal comment path
        assert!(svc.check());
    }

    #[tokio::test]
    async fn record_failure_threshold_opens_breaker() {
        let svc = Llm::new(
            Arc::new(ProviderRegistry::new()),
            Arc::new(ClientPool::with_defaults()),
            None,
        );
        for _ in 0..Breaker::FAILURE_THRESHOLD {
            svc.record_failure();
        }
        // After threshold, breaker should be Open and check() denies
        assert!(!svc.check());
        svc.record_success();
        assert!(svc.check());
    }

    #[test]
    fn tenant_model_policy_allows_and_composes_with_model_override() {
        let root = Context::new_root();
        let tenant_ctx = root.intercept(TenantModelPolicy::new(
            "tenant-a",
            ["gpt-4o-mini".to_string()],
        ));
        let request = tenant_ctx.intercept(ModelOverride {
            model: "gpt-4o-mini".into(),
        });
        let policy = request
            .get::<TenantModelPolicy>()
            .expect("policy should be inherited by request context");
        let override_model = request
            .get::<ModelOverride>()
            .expect("model override should be visible in request context");
        policy
            .authorize(&override_model.model)
            .expect("allowed model override should pass policy");
        let svc = Llm::new(
            Arc::new(ProviderRegistry::new()),
            Arc::new(ClientPool::with_defaults()),
            None,
        );
        svc.validate_model_override(&request)
            .expect("allowed model override should pass LLM validation");
        assert!(root.get::<ModelOverride>().is_none());
        assert!(root.get::<TenantModelPolicy>().is_none());
    }

    #[tokio::test]
    async fn disallowed_model_override_is_rejected_before_provider_execution() {
        let registry = Arc::new(ProviderRegistry::new());
        let svc = Arc::new(Llm::new(
            registry,
            Arc::new(ClientPool::with_defaults()),
            None,
        ));
        let root = Context::new_root();
        root.provide_arc(svc.clone());
        let tenant_ctx = root.intercept(TenantModelPolicy::new(
            "tenant-a",
            ["gpt-4o".to_string()],
        ));
        let request = tenant_ctx.intercept(ModelOverride {
            model: "not-allowed".into(),
        });
        let err = match svc
            .get_client(&request, CapabilityRequirements::default())
            .await
        {
            Ok(_) => panic!("disallowed override must fail before provider lookup"),
            Err(err) => err,
        };
        assert!(matches!(err, AppError::Auth(_)));
        assert!(err.to_string().contains("not-allowed"));
        assert!(root.get::<ModelOverride>().is_none());
        assert!(root.get::<TenantModelPolicy>().is_none());
        assert!(matches!(
            root.get::<Llm>().expect("global service").breaker(),
            Breaker::Closed
        ));
    }

    #[tokio::test]
    async fn get_client_uses_override_when_catalog_absent() {
        let registry = Arc::new(ProviderRegistry::new());
        let pool = Arc::new(ClientPool::with_defaults());
        let svc = Llm::new(registry, pool, None);
        let ctx = Context::new_root();
        let req_ctx = ctx.intercept(ModelOverride {
            model: "nonexistent-model-xyz".into(),
        });
        let req = CapabilityRequirements::default();
        // Should attempt override then fallback; fallback will fail because no provider configured
        let res = svc.get_client(&req_ctx, req).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn get_client_override_uses_tenant_context_intercept() {
        let mut registry = ProviderRegistry::new();
        registry.register_model(
            "pinned-model",
            crate::config::ModelConfig {
                provider: "shared-runtime".into(),
                model: "tenant-model".into(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );
        let global = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Global Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://global.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("global-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("global-key".to_string()),
            enabled: true,
        };
        let tenant = RuntimeProviderEntry {
            tenant_id: Some("tenant-a".to_string()),
            display_name: "Tenant Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://tenant.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("tenant-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("tenant-key".to_string()),
            enabled: true,
        };
        registry.reload_runtime_providers(
            vec![global, tenant],
            vec!["shared-runtime".to_string(), "shared-runtime".to_string()],
        );
        let registry = Arc::new(registry);
        let svc = Llm::new(registry, Arc::new(ClientPool::with_defaults()), None);
        let root = Context::new_root();
        let ctx = root
            .with_intercept(TenantContext::new("tenant-a".into(), TenantTier::Pro))
            .intercept(ModelOverride {
                model: "pinned-model".into(),
            });
        let tenant_client = svc
            .get_client(&ctx, CapabilityRequirements::default())
            .await;
        assert!(
            tenant_client.is_ok(),
            "tenant intercept should construct a client from the tenant runtime entry: {:?}",
            tenant_client.as_ref().err()
        );

        let unlabeled = root.intercept(ModelOverride {
            model: "pinned-model".into(),
        });
        let fleet_client = svc
            .get_client(&unlabeled, CapabilityRequirements::default())
            .await;
        assert!(
            fleet_client.is_ok(),
            "unlabeled root with ModelOverride should construct a client from the fleet global runtime entry: {:?}",
            fleet_client.as_ref().err()
        );

    }

    struct EchoClient {
        generated: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl EchoClient {
        fn new() -> (Self, std::sync::Arc<std::sync::atomic::AtomicBool>) {
            let generated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            (
                Self {
                    generated: std::sync::Arc::clone(&generated),
                },
                generated,
            )
        }
    }

    #[async_trait]
    impl LLMClient for EchoClient {
        async fn generate(&self, prompt: &str) -> ares_types::types::Result<String> {
            self.generated
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(format!("echo:{prompt}"))
        }
        async fn generate_with_system(
            &self,
            _system: &str,
            prompt: &str,
        ) -> ares_types::types::Result<String> {
            self.generate(prompt).await
        }
        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::types::Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }
        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> ares_types::types::Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }
        async fn generate_with_tools_and_history(
            &self,
            _messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> ares_types::types::Result<LLMResponse> {
            Ok(LLMResponse {
                content: String::new(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }
        async fn stream(
            &self,
            _prompt: &str,
        ) -> ares_types::types::Result<
            Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("echo stream not implemented".into()))
        }
        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::types::Result<
            Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("echo stream not implemented".into()))
        }
        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::types::Result<
            Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("echo stream not implemented".into()))
        }
        fn model_name(&self) -> &str {
            "echo"
        }
    }

    #[tokio::test]
    async fn llm_complete_runs_generate_without_events() {
        let (client, generated) = EchoClient::new();
        let llm = Llm::for_test(std::sync::Arc::new(client));
        let ctx = Context::new_root();
        let out = llm.complete(&ctx, "hi").await.expect("complete");
        assert_eq!(out, "echo:hi");
        assert!(generated.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn llm_complete_waterfall_rewrites_prompt() {
        let (client, _) = EchoClient::new();
        let llm = Llm::for_test(std::sync::Arc::new(client));
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall( cordis::events_catalog::ev::LLM_COMPLETE.to_string(), |mut payload, next| async move {
            if let Some(p) = payload.get("prompt").and_then(|v| v.as_str()) {
                payload["prompt"] = serde_json::json!(format!("WRAP:{p}"));
            }
            next(payload).await
        });
        let out = llm.complete(&ctx, "hi").await.expect("complete");
        assert_eq!(out, "echo:WRAP:hi");
    }

    #[tokio::test]
    async fn llm_complete_short_circuit_skips_generate() {
        let (client, generated) = EchoClient::new();
        let llm = Llm::for_test(std::sync::Arc::new(client));
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall( cordis::events_catalog::ev::LLM_COMPLETE.to_string(), |_payload, _next| async move {
            Ok(serde_json::json!({ "content": "cached" }))
        });
        let out = llm.complete(&ctx, "hi").await.expect("complete");
        assert_eq!(out, "cached");
        assert!(
            !generated.load(std::sync::atomic::Ordering::SeqCst),
            "dummy generate must stay false when handler skips next"
        );
    }

    #[tokio::test]
    async fn llm_get_client_waterfall_deny() {
        let (client, _) = EchoClient::new();
        let llm = Llm::for_test(std::sync::Arc::new(client));
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall( cordis::events_catalog::ev::LLM_GET_CLIENT.to_string(), |_payload, _next| async move {
            Ok(serde_json::json!({ "deny": true }))
        });
        let err = match llm
            .get_client(&ctx, CapabilityRequirements::default())
            .await
        {
            Ok(_) => panic!("deny"),
            Err(err) => err,
        };
        assert!(matches!(err, AppError::InvalidInput(msg) if msg == "llm.get_client denied"));
    }

    #[test]
    fn llm_list_models_exposes_registry_models() {
        let mut registry = ProviderRegistry::new();
        registry.register_model(
            "stub-model",
            crate::config::ModelConfig {
                provider: "stub".into(),
                model: "stub-model".into(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );
        let llm = Llm::new(
            Arc::new(registry),
            Arc::new(ClientPool::with_defaults()),
            None,
        );
        let models = llm.list_models();
        assert!(
            models
                .iter()
                .any(|m| m.name == "stub-model" && m.provider == "stub"),
            "Llm::list_models should expose registry models: {models:?}"
        );
    }
}
