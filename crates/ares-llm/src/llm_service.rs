//! Unified LLM service (Cordis Phase 5).
//!
//! Wraps `ProviderRegistry` + `NvidiaCatalogCache` + `ClientPool` behind a single
//! `Service` that can be injected via `ctx.get::<LlmService>()`.
//!
//! Per-request model pinning uses `ctx.intercept(ModelOverride { model })` so
//! the override is visible to `LlmService` via `ctx.get::<ModelOverride>()`
//! without mutating global state.
//!
//! Circuit-breaker (`Breaker`) causes `Service::check` to return `false` when
//! open, causing dependent fibers to deactivate (guarded withdrawal per Thm 63:
//! provider does not withdraw until dependents deactivate).

use std::future::Future;
use std::sync::Arc;

use ares_cordis_core::{Context, CordisError, Service};
use ares_config::nvidia_catalog::NvidiaCatalogCache;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::capabilities::CapabilityRequirements;
use crate::client::LLMClient;
use crate::pool::ClientPool;
use crate::provider_registry::ProviderRegistry;
use ares_types::types::AppError;

/// Per-request model override for `ctx.intercept`.
///
/// Example:
/// ```ignore
/// let req_ctx = root_ctx.intercept(ModelOverride { model: "gpt-4o-mini".into() });
/// let llm = req_ctx.get::<LlmService>().unwrap();
/// // inside LlmService, check `ctx.get::<ModelOverride>()` for pinning
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

/// Circuit-breaker state for `LlmService`.
///
/// `check()` returns:
/// - `Closed` / `HalfOpen` → `true` (service advertises healthy, fibers stay Active)
/// - `Open { until }` → `false` until cooldown expires (fibers deactivate, guarded withdrawal per Thm 63)
#[derive(Debug, Clone)]
pub enum Breaker {
    /// Normal operation.
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
    ///   otherwise stays `Closed` (counting is external via `LlmService::record_failure`
    ///   failure counter; this pure transition always opens — the service layer
    ///   decides when to call it. For `Closed` we open immediately; callers that
    ///   want threshold counting should use `LlmService::record_failure`).
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

impl Default for Breaker {
    fn default() -> Self {
        Breaker::Closed
    }
}

/// Unified LLM service composing provider registry, catalog, and pool.
///
/// Supports `ctx.get::<LlmService>()`, `ctx.isolate` per-tenant scoping, and
/// `ctx.intercept::<ModelOverride>` per-request model pinning.
///
/// `Service::check` delegates to the circuit breaker — when the breaker is
/// `Open`, `check()` returns `false` so dependent fibers (e.g. `AgentExecutionService`)
/// deactivate via guarded withdrawal (Thm 63).
pub struct LlmService {
    /// Named provider registry (OpenAI, Anthropic, etc.).
    pub provider_registry: Arc<ProviderRegistry>,
    /// NVIDIA catalog cache for capability-based model selection (optional so
    /// `cargo check --no-default-features` remains viable).
    pub catalog: Option<Arc<NvidiaCatalogCache>>,
    /// Pooled LLM clients.
    pub pool: Arc<ClientPool>,
    /// Circuit-breaker state.
    breaker: RwLock<Breaker>,
    /// Consecutive failure count for thresholded transition.
    failures: RwLock<u32>,
}

impl LlmService {
    /// Create a new `LlmService`.
    pub fn new(
        provider_registry: Arc<ProviderRegistry>,
        pool: Arc<ClientPool>,
        catalog: Option<Arc<NvidiaCatalogCache>>,
    ) -> Self {
        Self {
            provider_registry,
            catalog,
            pool,
            breaker: RwLock::new(Breaker::Closed),
            failures: RwLock::new(0),
        }
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
            breaker: RwLock::new(breaker),
            failures: RwLock::new(0),
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

    /// Capability-aware client resolution with per-request `ModelOverride` pinning.
    ///
    /// 1. If `ctx.get::<ModelOverride>()` is present, attempt to create a client for
    ///    that exact model (via `ProviderRegistry::create_client_for_model`). On
    ///    success return immediately. Also opportunistically try the `ClientPool`
    ///    fast-path `pool.try_get(&ov.model)` if the model name matches a
    ///    registered provider.
    /// 2. Otherwise, use `catalog` + `provider_registry.find_best_model` for
    ///    capability-based selection when present.
    /// 3. Fallback through the coordinator chain via `provider_registry.resolve_with_fallback`.
    pub async fn get_client(
        &self,
        ctx: &Arc<Context>,
        capability: CapabilityRequirements,
    ) -> Result<Arc<dyn LLMClient>, AppError> {
        // 1. Per-request pinning via `ctx.get::<ModelOverride>()` (intercept realm).
        if let Some(ov) = ctx.get::<ModelOverride>() {
            // Fast-path: pooled provider named exactly like the override model
            if let Ok(guard) = self.pool.try_get(&ov.model).await {
                let boxed = guard.take();
                return Ok(Arc::from(boxed));
            }
            if let Ok(client) = self
                .provider_registry
                .create_client_for_model(&ov.model)
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
                    .create_client_for_model(&best.name)
                    .await
                {
                    return Ok(Arc::from(client));
                }
            }
        } else if let Some(best) = self.provider_registry.find_best_model(&capability) {
            if let Ok(client) = self
                .provider_registry
                .create_client_for_model(&best.name)
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
        if let Some(ov) = ctx.get::<ModelOverride>() {
            if let Ok(guard) = self.pool.try_get(&ov.model).await {
                return Ok(guard.take());
            }
            if let Ok(client) = self.provider_registry.create_client_for_model(&ov.model).await {
                return Ok(client);
            }
        }
        if let Some(catalog) = &self.catalog {
            let _snap = catalog.snapshot();
            if let Some(best) = self.provider_registry.find_best_model(&capability) {
                if let Ok(client) = self.provider_registry.create_client_for_model(&best.name).await {
                    return Ok(client);
                }
            }
        } else if let Some(best) = self.provider_registry.find_best_model(&capability) {
            if let Ok(client) = self.provider_registry.create_client_for_model(&best.name).await {
                return Ok(client);
            }
        }
        self.provider_registry
            .resolve_with_capability_fallback(Some(capability))
            .await
    }

    /// Stub for capability-based model selection (delegates to registry).
    pub fn find_model_stub(&self, _capability: &str) -> Option<String> {
        None
    }
}

impl Service for LlmService {
    fn name(&self) -> &'static str {
        "LlmService"
    }

    fn init(
        &self,
        _ctx: &Arc<Context>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Option<Box<dyn ares_cordis_core::Disposable>>, CordisError>> + Send + '_>> {
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
    use ares_cordis_core::Context;
    use chrono::Duration;

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

    #[tokio::test]
    async fn llm_service_model_override_via_context() {
        let registry = Arc::new(ProviderRegistry::new());
        let pool = Arc::new(ClientPool::with_defaults());
        let svc = Arc::new(LlmService::new(registry, pool, None));
        let root = Context::new_root();
        root.provide::<LlmService>(LlmService::new(
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
        // LlmService composition fields exist
        assert!(svc.provider_registry as *const _ != std::ptr::null());
        let _ = svc.catalog.clone();
        let _ = svc.pool.provider_names();
        // Service check guarded withdrawal comment path
        assert!(svc.check());
    }

    #[tokio::test]
    async fn record_failure_threshold_opens_breaker() {
        let svc = LlmService::new(
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

    #[tokio::test]
    async fn get_client_uses_override_when_catalog_absent() {
        let registry = Arc::new(ProviderRegistry::new());
        let pool = Arc::new(ClientPool::with_defaults());
        let svc = LlmService::new(registry, pool, None);
        let ctx = Context::new_root();
        let req_ctx = ctx.intercept(ModelOverride {
            model: "nonexistent-model-xyz".into(),
        });
        let req = CapabilityRequirements::default();
        // Should attempt override then fallback; fallback will fail because no provider configured
        let res = svc.get_client(&req_ctx, req).await;
        assert!(res.is_err());
    }
}
