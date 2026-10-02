//! Item 2.5a: a paused tenant runs nothing. `tenants.paused` (migration 037) is read on every
//! pass through `ares_agent::admit`, which `Execute::run` and `Execute::run_stream` call before
//! anything else, so a paused tenant is refused with `AppError::Unavailable` before any model
//! call.
//!
//! "No model call" is measured by a stub model that counts every call it receives. The count must
//! be zero on every refusal.
//!
//! What this file does not cover, on purpose:
//! - the global emergency stop: `admit` does not check it, its callers do, so its ordering against
//!   the pause is a caller's matter (item 2.5b, which touches those callers);
//! - the skill-engine paths that bypass `admit` (item 2.5b).
//!
//! Every test needs the scratch database named by `TEST_DATABASE_URL`. Tests set `paused` through
//! the test's own pool, never through a service.
#![cfg(feature = "postgres")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ares_agent::{request_tenant_ctx, AgentRequest, Execute};
use ares_llm::{LLMClient, LLMResponse, Llm, LlmStreamEvent};
use ares_store::{FleetSecrets, PostgresClient, TenantDb};
use ares_types::models::{TenantContext, TenantTier};
use ares_types::types::{AppError, ToolDefinition};
use cordis::Context;
use sqlx::PgPool;

/// What the stub model answers with: proves a run reached the model.
const ANSWER: &str = "the model was called";

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
            content: ANSWER.to_string(),
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
        "counting-llm"
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

// ---------------------------------------------------------------------------
// The fixture: a shared context with a `TenantDb`, the counting model, and no agent registry, so
// an admitted run takes the generic path and calls the stub model.
// ---------------------------------------------------------------------------

struct Fixture {
    exec: Execute,
    root: Arc<Context>,
    pool: PgPool,
    llm_calls: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new() -> Self {
        let pool = ares_test_support::pool().await;
        let llm_calls = Arc::new(AtomicUsize::new(0));
        let root = Context::new_root();
        root.provide(Llm::from_client(Arc::new(CountingLlm {
            calls: Arc::clone(&llm_calls),
        })));
        root.provide(TenantDb::new(Arc::new(PostgresClient {
            pool: pool.clone(),
        })));
        root.provide(FleetSecrets::new());
        Self {
            exec: Execute::new(),
            root,
            pool,
            llm_calls,
        }
    }

    /// A `tenants` row written the way code that predates migration 037 writes it: the insert
    /// does not mention `paused` or `strict`, so both read the column default.
    async fn tenant_row(&self) -> String {
        let tenant = format!("tenant-pause-{}", uuid::Uuid::new_v4());
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

    /// A request context scoped to `tenant`, as the HTTP layer builds it.
    fn ctx_for(&self, tenant: &str) -> Arc<Context> {
        request_tenant_ctx(
            &self.root,
            TenantContext::new(tenant.to_string(), TenantTier::Pro),
        )
    }

    fn calls(&self) -> usize {
        self.llm_calls.load(Ordering::SeqCst)
    }

    /// One run through the public `Execute::run` entry.
    async fn run_once(&self, tenant: &str) -> Result<String, AppError> {
        self.exec
            .run(&request(), &self.ctx_for(tenant))
            .await
            .map(|result| result.response.content)
    }

    /// Best-effort removal of the scratch rows.
    async fn cleanup<S: AsRef<str>>(&self, tenants: &[S]) {
        for tenant in tenants {
            let tenant: &str = tenant.as_ref();
            let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
                .bind(tenant)
                .execute(&self.pool)
                .await;
        }
    }
}

fn request() -> AgentRequest {
    AgentRequest {
        agent_name: "product".to_string(),
        message: "hello".to_string(),
        ..Default::default()
    }
}

/// The refusal is the typed `Unavailable` kind (HTTP 503), names the pause, and carries nothing
/// about the tenant.
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
// A paused tenant is refused, with no model call.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_paused_tenant_is_refused_before_any_model_call() {
    let f = Fixture::new().await;
    let tenant = f.tenant_row().await;
    f.set_paused(&tenant, true).await;

    let err = f
        .run_once(&tenant)
        .await
        .expect_err("a paused tenant must be refused");
    assert_pause_refusal(&err, &tenant);
    assert_eq!(
        f.calls(),
        0,
        "the stub model was called for a paused tenant"
    );

    f.cleanup(&[&tenant]).await;
}

/// The stream path goes through `admit` as well.
#[tokio::test]
async fn a_paused_tenant_is_refused_on_the_stream_path_before_any_model_call() {
    let f = Fixture::new().await;
    let tenant = f.tenant_row().await;
    f.set_paused(&tenant, true).await;

    let err = match f.exec.run_stream(&request(), &f.ctx_for(&tenant)).await {
        Err(err) => err,
        Ok(_) => panic!("a paused tenant must not be handed a stream"),
    };
    assert_pause_refusal(&err, &tenant);
    assert_eq!(
        f.calls(),
        0,
        "the stub model was called for a paused tenant"
    );

    f.cleanup(&[&tenant]).await;
}

/// The smallest public path: `ares_agent::admit` itself.
#[tokio::test]
async fn admit_itself_refuses_a_paused_tenant() {
    let f = Fixture::new().await;
    let tenant = f.tenant_row().await;
    f.set_paused(&tenant, true).await;

    let err = ares_agent::admit(&f.ctx_for(&tenant))
        .await
        .expect_err("admit must refuse a paused tenant");
    assert_pause_refusal(&err, &tenant);
    assert_eq!(f.calls(), 0);

    f.cleanup(&[&tenant]).await;
}

// ---------------------------------------------------------------------------
// An unpaused tenant runs as today.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unpaused_tenant_runs_as_today() {
    let f = Fixture::new().await;
    let tenant = f.tenant_row().await;
    f.set_paused(&tenant, false).await;

    let content = f
        .run_once(&tenant)
        .await
        .expect("an unpaused tenant must run");
    assert_eq!(content, ANSWER);
    assert!(f.calls() > 0, "the run never reached the stub model");

    f.cleanup(&[&tenant]).await;
}

/// A row that predates 037 (its insert never mentions the flags) reads the column default,
/// `false`, and runs unchanged.
#[tokio::test]
async fn a_row_predating_the_flags_reads_the_default_and_runs() {
    let f = Fixture::new().await;
    let tenant = f.tenant_row().await;

    let (paused, strict): (bool, bool) =
        sqlx::query_as("SELECT paused, strict FROM tenants WHERE id = $1")
            .bind(&tenant)
            .fetch_one(&f.pool)
            .await
            .expect("read the flags of a row written without them");
    assert!(!paused, "paused must default to false");
    assert!(!strict, "strict must default to false");

    let content = f.run_once(&tenant).await.expect("a default row must run");
    assert_eq!(content, ANSWER);
    assert!(f.calls() > 0, "the run never reached the stub model");

    f.cleanup(&[&tenant]).await;
}

// ---------------------------------------------------------------------------
// The pause is per tenant, and it takes effect on the next run.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pause_is_per_tenant() {
    let f = Fixture::new().await;
    let paused = f.tenant_row().await;
    let running = f.tenant_row().await;
    f.set_paused(&paused, true).await;

    let err = f
        .run_once(&paused)
        .await
        .expect_err("tenant A is paused and must be refused");
    assert_pause_refusal(&err, &paused);
    assert_eq!(f.calls(), 0, "tenant A's refusal must not call the model");

    let content = f
        .run_once(&running)
        .await
        .expect("tenant B is not paused and must run");
    assert_eq!(content, ANSWER);
    assert!(f.calls() > 0, "tenant B never reached the stub model");

    f.cleanup(&[&paused, &running]).await;
}

/// No cache and no restart: the flag is read on every admit, so a pause binds the next run and
/// lifting it frees the run after that.
#[tokio::test]
async fn a_pause_binds_the_next_run_and_lifts_the_same_way() {
    let f = Fixture::new().await;
    let tenant = f.tenant_row().await;

    f.run_once(&tenant).await.expect("runs before the pause");
    let before = f.calls();
    assert!(before > 0);

    f.set_paused(&tenant, true).await;
    let err = f
        .run_once(&tenant)
        .await
        .expect_err("the very next run after the pause must be refused");
    assert_pause_refusal(&err, &tenant);
    assert_eq!(f.calls(), before, "a paused run must not call the model");

    f.set_paused(&tenant, false).await;
    f.run_once(&tenant)
        .await
        .expect("runs after the pause lifts");
    assert!(
        f.calls() > before,
        "the lifted tenant never reached the model"
    );

    f.cleanup(&[&tenant]).await;
}

// ---------------------------------------------------------------------------
// An unknown tenant.
// ---------------------------------------------------------------------------

/// `admit` already admits a tenant that has no `tenants` row: its usage lookup sums to zero and
/// its quota comes from the `TenantContext`. (The JWT path does the same on purpose: an unknown
/// JWT tenant runs at the free tier.) The pause cannot be set on a row that does not exist, so
/// a missing row reads as not paused and the run behaves as it does today.
#[tokio::test]
async fn an_unknown_tenant_is_admitted_as_today() {
    let f = Fixture::new().await;
    let unknown = format!("tenant-no-row-{}", uuid::Uuid::new_v4());

    let content = f
        .run_once(&unknown)
        .await
        .expect("a tenant without a row runs as it does today");
    assert_eq!(content, ANSWER);
    assert!(f.calls() > 0, "the run never reached the stub model");
}
