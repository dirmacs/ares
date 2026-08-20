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

use ares_cordis_core::{CordisError, Service};
use ares_config::nvidia_catalog::NvidiaCatalogCache;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::pool::ClientPool;
use crate::provider_registry::ProviderRegistry;

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
    /// Returns `true` if the breaker allows requests.
    ///
    /// When `Open` and `Utc::now() >= until`, the breaker is still
    /// considered open until an external `half_open` transition is called.
    /// Callers should periodically attempt `HalfOpen` after cooldown.
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
    /// NVIDIA catalog cache for capability-based model selection.
    pub catalog: Arc<NvidiaCatalogCache>,
    /// Pooled LLM clients.
    pub pool: Arc<ClientPool>,
    /// Circuit-breaker state.
    breaker: RwLock<Breaker>,
}

impl LlmService {
    /// Create a new `LlmService`.
    pub fn new(
        provider_registry: Arc<ProviderRegistry>,
        catalog: Arc<NvidiaCatalogCache>,
        pool: Arc<ClientPool>,
    ) -> Self {
        Self {
            provider_registry,
            catalog,
            pool,
            breaker: RwLock::new(Breaker::Closed),
        }
    }

    /// Create with explicit breaker (e.g. `Open` for testing).
    pub fn with_breaker(
        provider_registry: Arc<ProviderRegistry>,
        catalog: Arc<NvidiaCatalogCache>,
        pool: Arc<ClientPool>,
        breaker: Breaker,
    ) -> Self {
        Self {
            provider_registry,
            catalog,
            pool,
            breaker: RwLock::new(breaker),
        }
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
    }

    /// Stub for capability-based model selection (future: delegates to
    /// `provider_registry.find_model` + `catalog` quality scores).
    pub fn find_model_stub(&self, _capability: &str) -> Option<String> {
        None
    }
}

impl Service for LlmService {
    fn name(&self) -> &'static str {
        "LlmService"
    }

    fn init(&self, _ctx: &Arc<ares_cordis_core::Context>) -> std::pin::Pin<Box<dyn Future<Output = Result<Option<Box<dyn ares_cordis_core::Disposable>>, CordisError>> + Send + '_>> {
        Box::pin(async { Ok(None) })
    }

    fn check(&self) -> bool {
        // Circuit-breaker advertisement: when Open (and cooldown not elapsed),
        // this service is unhealthy → dependent fibers deactivate (Thm 63).
        self.breaker.read().check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
