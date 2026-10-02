//! Item 2.5a, fix round 1: `POST /v1/research` honours the per-tenant pause.
//!
//! The handler (`v1_research`, mounted behind API-key auth) never passes through
//! `ares_agent::admit`, so it makes the pause check itself, through the same function `admit`
//! calls (`ares_agent::admit::ensure_tenant_not_paused`), after the global emergency stop and
//! before the config lookup, the model and the research coordinator.
//!
//! "Before" is measured two ways:
//! - a stub model counts every call it receives: the count is zero on a refusal;
//! - one test leaves the config service off the context, so a handler that went on to the config
//!   lookup (`.expect("not provided")`) would panic instead of refusing.
//!
//! The handler is called directly with the extractors the router would hand it (the
//! `TenantContext` the API-key middleware attaches), the way `research.rs`'s own tests drive
//! theirs. The refusal is `AppError::Unavailable`, which `HttpError` maps to HTTP 503.
//!
//! Every test needs the scratch database named by `TEST_DATABASE_URL`: `ares_test_support::pool()`
//! panics when it is unreachable, so nothing here skips.
#![cfg(feature = "postgres")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ares_http::api::handlers::v1::v1_research;
use ares_http::config::{AuthConfig, ServerConfig};
use ares_http::overlay::{
    AresConfig, AresConfigManager, BillingConfig, DatabaseConfig, DynamicConfigPaths, RagConfig,
};
use ares_http::HttpError;
use ares_llm::{LLMClient, LLMResponse, Llm, LlmStreamEvent};
use ares_store::{PostgresClient, TenantDb};
use ares_types::models::{TenantContext, TenantTier};
use ares_types::types::{AppError, ResearchRequest, ToolDefinition};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{extract::State, Extension, Json};
use cordis::Context;
use sqlx::PgPool;

// ---------------------------------------------------------------------------
// A model client that counts every call.
// ---------------------------------------------------------------------------

struct CountingLlm {
    calls: Arc<AtomicUsize>,
}

impl CountingLlm {
    fn response(&self) -> LLMResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        LLMResponse {
            content: "1. a question\n2. another question".to_string(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: None,
            reasoning_content: None,
            response_id: None,
        }
    }
}

type StringStream =
    Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>;

#[async_trait::async_trait]
impl LLMClient for CountingLlm {
    fn model_name(&self) -> &str {
        MODEL
    }
    async fn generate(&self, _: &str) -> ares_types::types::Result<String> {
        Ok(self.response().content)
    }
    async fn generate_with_system(&self, _: &str, _: &str) -> ares_types::types::Result<String> {
        Ok(self.response().content)
    }
    async fn generate_with_history(
        &self,
        _: &[(String, String)],
    ) -> ares_types::types::Result<LLMResponse> {
        Ok(self.response())
    }
    async fn generate_with_tools(
        &self,
        _: &str,
        _: &[ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        Ok(self.response())
    }
    async fn generate_with_tools_and_history(
        &self,
        _: &[ares_llm::coordinator::ConversationMessage],
        _: &[ToolDefinition],
    ) -> ares_types::types::Result<LLMResponse> {
        Ok(self.response())
    }
    async fn stream(&self, _: &str) -> ares_types::types::Result<StringStream> {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![
            Ok::<String, AppError>(text),
        ])))
    }
    async fn stream_with_system(
        &self,
        _: &str,
        _: &str,
    ) -> ares_types::types::Result<StringStream> {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![
            Ok::<String, AppError>(text),
        ])))
    }
    async fn stream_with_history(
        &self,
        _: &[(String, String)],
    ) -> ares_types::types::Result<StringStream> {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![
            Ok::<String, AppError>(text),
        ])))
    }
    async fn stream_with_tools_and_history(
        &self,
        _: &[ares_llm::coordinator::ConversationMessage],
        _: &[ToolDefinition],
    ) -> ares_types::types::Result<
        Box<dyn futures::Stream<Item = ares_types::types::Result<LlmStreamEvent>> + Send + Unpin>,
    > {
        let text = self.response().content;
        Ok(Box::new(futures::stream::iter(vec![Ok::<
            LlmStreamEvent,
            AppError,
        >(
            LlmStreamEvent::Text(text),
        )])))
    }
}

/// The stub's model name: what the handler's tenant model allowlist is asked about.
const MODEL: &str = "counting-llm";

// ---------------------------------------------------------------------------
// The fixture: the services the handler resolves, and the counting model.
// ---------------------------------------------------------------------------

/// An `AresConfig` with nothing configured: the handler falls back to its defaults.
fn empty_config() -> AresConfig {
    AresConfig {
        server: ServerConfig::default(),
        auth: AuthConfig::default(),
        database: DatabaseConfig::default(),
        nvidia: None,
        config: DynamicConfigPaths::default(),
        providers: Default::default(),
        models: Default::default(),
        tools: Default::default(),
        agents: Default::default(),
        workflows: Default::default(),
        rag: RagConfig::default(),
        billing: BillingConfig::default(),
        skills: None,
    }
}

struct Fixture {
    ctx: Arc<Context>,
    pool: PgPool,
    llm_calls: Arc<AtomicUsize>,
}

impl Fixture {
    /// `emergency_stop`: the global stop's state. `with_config`: whether the config service is on
    /// the context (off, a handler that reaches the config lookup panics).
    async fn new(emergency_stop: bool, with_config: bool) -> Self {
        let pool = ares_test_support::pool().await;
        let llm_calls = Arc::new(AtomicUsize::new(0));
        let ctx = Context::new_root();
        ctx.provide(ares_agent::EmergencyStop::new(emergency_stop));
        ctx.provide(TenantDb::new(Arc::new(PostgresClient {
            pool: pool.clone(),
        })));
        ctx.provide(Llm::from_client(Arc::new(CountingLlm {
            calls: Arc::clone(&llm_calls),
        })));
        if with_config {
            ctx.provide(AresConfigManager::from_config(empty_config()));
        }
        Self {
            ctx,
            pool,
            llm_calls,
        }
    }

    /// A `tenants` row written the way code that predates migration 037 writes it.
    async fn tenant_row(&self) -> String {
        let tenant = format!("v1-research-pause-{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) \
             VALUES ($1, $1, 'free', 1, 1)",
        )
        .bind(&tenant)
        .execute(&self.pool)
        .await
        .expect("insert the scratch tenant");
        tenant
    }

    async fn set_paused(&self, tenant: &str, paused: bool) {
        sqlx::query("UPDATE tenants SET paused = $2 WHERE id = $1")
            .bind(tenant)
            .bind(paused)
            .execute(&self.pool)
            .await
            .expect("set tenants.paused on the scratch tenant");
    }

    /// Let `tenant` use the stub model: the handler's allowlist is deny-by-default.
    async fn allow_the_stub_model(&self, tenant: &str) {
        ares_store::tenant_allowlist::TenantAllowlistStore::new(&self.pool)
            .allow_model(tenant, MODEL)
            .await
            .expect("allow the stub model for the scratch tenant");
    }

    fn calls(&self) -> usize {
        self.llm_calls.load(Ordering::SeqCst)
    }

    /// One `POST /v1/research` for `tenant`, as the handler is called after API-key auth.
    ///
    /// The handler runs on its own task: a handler that goes on past the pause point to the
    /// config lookup panics there, and that is reported as the bypass it is, not as a harness
    /// failure.
    async fn research(&self, tenant: &str) -> Result<StatusCode, HttpError> {
        let state = Arc::clone(&self.ctx);
        let tenant = tenant.to_string();
        let handle = tokio::spawn(async move {
            v1_research(
                State(state),
                Some(Extension(TenantContext::new(tenant, TenantTier::Free))),
                None,
                Json(ResearchRequest {
                    query: "what does the pause bind?".to_string(),
                    depth: None,
                    max_iterations: None,
                }),
            )
            .await
            .map(|response| response.status())
        });
        match handle.await {
            Ok(result) => result,
            Err(join) if join.is_panic() => {
                let panic = join.into_panic();
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                panic!("v1_research went past the pause point and panicked: {message}")
            }
            Err(join) => panic!("the handler task did not finish: {join:?}"),
        }
    }

    async fn cleanup(&self, tenants: &[&str]) {
        for tenant in tenants {
            let _ = sqlx::query("DELETE FROM tenant_model_allowlist WHERE tenant_id = $1")
                .bind(tenant)
                .execute(&self.pool)
                .await;
            let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
                .bind(tenant)
                .execute(&self.pool)
                .await;
        }
    }
}

/// The refusal is the typed `Unavailable` kind, names the pause and nothing about the tenant.
fn assert_pause_refusal(err: &AppError, tenant: &str) {
    match err {
        AppError::Unavailable(message) => {
            assert!(
                message.to_lowercase().contains("paused"),
                "the message must name the pause: {message:?}"
            );
            assert!(
                !message.contains(tenant),
                "the message must not carry the tenant id: {message:?}"
            );
        }
        other => panic!("expected AppError::Unavailable, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A paused tenant is refused, before any model or coordinator call.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_paused_tenant_is_refused_with_a_503_before_any_model_call() {
    let f = Fixture::new(false, true).await;
    let tenant = f.tenant_row().await;
    f.allow_the_stub_model(&tenant).await;
    f.set_paused(&tenant, true).await;

    let err = f
        .research(&tenant)
        .await
        .expect_err("a paused tenant must be refused on /v1/research");
    assert_pause_refusal(&err.0, &tenant);
    assert_eq!(
        err.into_response().status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the refusal must be an HTTP 503"
    );
    assert_eq!(
        f.calls(),
        0,
        "the stub model was called for a paused tenant"
    );

    f.cleanup(&[&tenant]).await;
}

/// With no config service on the context, a handler that reached the config lookup would panic
/// (`.expect("not provided")`). The refusal must come first.
#[tokio::test]
async fn the_refusal_comes_before_the_config_lookup() {
    let f = Fixture::new(false, false).await;
    let tenant = f.tenant_row().await;
    f.set_paused(&tenant, true).await;

    let err = f
        .research(&tenant)
        .await
        .expect_err("a paused tenant must be refused before the config lookup");
    assert_pause_refusal(&err.0, &tenant);
    assert_eq!(f.calls(), 0, "the stub model was called");

    f.cleanup(&[&tenant]).await;
}

// ---------------------------------------------------------------------------
// The check is after the global stop, and an unpaused tenant passes it.
// ---------------------------------------------------------------------------

/// The global emergency stop is checked first, so with both in force the stop's message wins.
#[tokio::test]
async fn the_global_emergency_stop_still_comes_first() {
    let f = Fixture::new(true, true).await;
    let tenant = f.tenant_row().await;
    f.set_paused(&tenant, true).await;

    let err = f
        .research(&tenant)
        .await
        .expect_err("the emergency stop must refuse the run");
    match &err.0 {
        AppError::Unavailable(message) => assert!(
            message.contains("human review"),
            "the stop's own message must come first: {message:?}"
        ),
        other => panic!("expected AppError::Unavailable, got {other:?}"),
    }
    assert_eq!(f.calls(), 0, "the stub model was called");

    f.cleanup(&[&tenant]).await;
}

/// An unpaused tenant is not refused: the handler goes on, through the config lookup and the
/// allowlist, to the research coordinator, which calls the stub model.
#[tokio::test]
async fn an_unpaused_tenant_passes_the_pause_check() {
    let f = Fixture::new(false, true).await;
    let tenant = f.tenant_row().await;
    f.allow_the_stub_model(&tenant).await;
    f.set_paused(&tenant, false).await;

    let outcome = f.research(&tenant).await;
    if let Err(err) = &outcome {
        if let AppError::Unavailable(message) = &err.0 {
            assert!(
                !message.to_lowercase().contains("paused"),
                "an unpaused tenant was refused as paused: {message:?}"
            );
        }
    }
    assert!(
        f.calls() > 0,
        "the handler never reached the stub model for an unpaused tenant (outcome: {outcome:?})"
    );

    f.cleanup(&[&tenant]).await;
}

/// The pause is per tenant: a paused neighbour does not stop this tenant.
#[tokio::test]
async fn a_paused_neighbour_does_not_stop_an_unpaused_tenant() {
    let f = Fixture::new(false, true).await;
    let running = f.tenant_row().await;
    let paused = f.tenant_row().await;
    f.allow_the_stub_model(&running).await;
    f.set_paused(&paused, true).await;

    let outcome = f.research(&running).await;
    assert!(
        f.calls() > 0,
        "the unpaused tenant never reached the stub model (outcome: {outcome:?})"
    );

    f.cleanup(&[&running, &paused]).await;
}
