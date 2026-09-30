//! Item 1.13: a failed run on `POST /v1/agents/{name}/run` is nameable.
//!
//! - the failure class (`reason_code`) reaches the run's `usage_events` row;
//! - `agent_runs` and `usage_events` carry the model and provider the run resolved to, or the
//!   marker `unresolved` when it failed before the agent was built;
//! - one `warn` line names the class, never the provider's error text;
//! - a tenant over its budget is refused with its own class, `budget_exceeded`, before any
//!   model call;
//! - a completed run is metered as before, with a NULL `reason_code`.
//!
//! Every test drives the real v1 stack (`create_router`: API-key auth, the `TenantDb`
//! injection, `track_usage`, then `run_agent`) in process with `tower::ServiceExt::oneshot`,
//! against a stub Ollama provider on `127.0.0.1:0` that either answers or fails with HTTP 500.
//! The agent's config names a model alias (`MODEL_ALIAS`), which the registry resolves to the
//! stub's provider and concrete model: the stored names must be the resolved pair, never the
//! alias.
//!
//! Requires a live Postgres named by `TEST_DATABASE_URL` (a scratch database, never
//! `ares_test`). Configured and unreachable, a test panics (naming the variable only);
//! unconfigured, it skips with the crate's convention (`tests/common/mod.rs`).

#![cfg(feature = "postgres")]

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ares_agent::AgentRegistry;
use ares_http::active_runs::ActiveRuns;
use ares_http::auth::jwt::AuthService;
use ares_llm::{ModelConfig, ProviderConfig, ProviderRegistry};
use ares_store::run_history::{LogLlmCallRequest, RunHistoryStore, SetTenantBudgetRequest};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::token_budgets::TokenBudgetStore;
use ares_store::TenantDb;
use ares_tools::Tools;
use ares_types::models::TenantTier;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use cordis::Context;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const AGENT: &str = "metering-probe";
/// The provider name the stub is registered under.
const STUB_PROVIDER: &str = "stub-provider-113";
/// The concrete model id the alias resolves to.
const STUB_MODEL: &str = "stub-model-113";
/// What the agent's config names: an alias the registry resolves, not a model id.
const MODEL_ALIAS: &str = "tier-alias-113";
/// A model no tier, model entry or provider answers to: resolution fails.
const UNKNOWN_MODEL: &str = "no-such-model-113";
/// The failing stub's response body: the provider's own error text.
const STUB_TEXT: &str = "stub-upstream-detail-7f3a: quota for org acme-internal exhausted";
/// The marker for names that were never resolved.
const UNRESOLVED: &str = "unresolved";

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// The stub provider
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Stub {
    /// Answers every chat call (Ollama's native `/api/chat` shape).
    Answer,
    /// Fails every call with HTTP 500 and `STUB_TEXT` as the body.
    Fail,
}

/// Serve the stub on an ephemeral port. Returns its base URL and a counter of every request
/// it received, whatever the path.
async fn spawn_stub(mode: Stub) -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let app = Router::new().fallback(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            match mode {
                Stub::Answer => axum::Json(json!({
                    "model": STUB_MODEL,
                    "created_at": "2026-09-30T00:00:00Z",
                    "message": {"role": "assistant", "content": "stub answer"},
                    "done": true,
                    "total_duration": 1,
                    "load_duration": 1,
                    "prompt_eval_count": 3,
                    "prompt_eval_duration": 1,
                    "eval_count": 2,
                    "eval_duration": 1
                }))
                .into_response(),
                Stub::Fail => (StatusCode::INTERNAL_SERVER_ERROR, STUB_TEXT).into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the stub provider");
    let addr = listener.local_addr().expect("stub provider address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve the stub provider");
    });
    (format!("http://{addr}"), calls)
}

// ---------------------------------------------------------------------------
// The fixture: one tenant, one tenant agent, the real v1 router
// ---------------------------------------------------------------------------

struct Fixture {
    app: Router,
    pool: PgPool,
    tenant_id: String,
    api_key: String,
    stub_calls: Arc<AtomicUsize>,
}

/// What one run returned.
struct RunReply {
    status: StatusCode,
    body: Value,
}

impl RunReply {
    fn run_id(&self) -> String {
        self.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("the run response carries an id: {}", self.body))
            .to_string()
    }
}

/// The run's `agent_runs` row.
#[derive(Debug)]
struct RunRow {
    status: String,
    model_name: Option<String>,
    provider_name: Option<String>,
    error: Option<String>,
}

/// The run's `usage_events` row.
#[derive(Debug)]
struct UsageRow {
    reason_code: Option<String>,
    success: bool,
    model_name: Option<String>,
    provider_name: Option<String>,
}

impl Fixture {
    /// `None` only for the unconfigured skip. The tenant agent's config names `agent_model`.
    async fn new(stub: Stub, agent_model: &str) -> Option<Self> {
        common::live_db_url(&common::current_test_name()).await?;
        // Before any run in the binary, so no callsite is ever cached without the capture.
        global_capture();
        let pg = Arc::new(ares_test_support::client().await);
        let pool = pg.pool.clone();
        let tenant_db = Arc::new(TenantDb::new(pg.clone()));

        let (stub_url, stub_calls) = spawn_stub(stub).await;
        let mut providers = HashMap::new();
        providers.insert(
            STUB_PROVIDER.to_string(),
            ProviderConfig::Ollama {
                api_key_env: "ARES_1_13_UNUSED".to_string(),
                base_url: stub_url,
                default_model: STUB_MODEL.to_string(),
            },
        );
        let mut models = HashMap::new();
        models.insert(
            MODEL_ALIAS.to_string(),
            ModelConfig {
                provider: STUB_PROVIDER.to_string(),
                model: STUB_MODEL.to_string(),
                temperature: 0.0,
                max_tokens: 64,
            },
        );
        let provider_registry = Arc::new(ProviderRegistry::from_config(providers, models, None));
        let tools = Arc::new(Tools::from_static([]));
        let agent_registry = Arc::new(AgentRegistry::from_config(
            HashMap::new(),
            provider_registry.clone(),
            tools.clone(),
        ));

        let ctx = Context::new_root();
        ctx.provide_arc(pg);
        ctx.provide_arc(tenant_db.clone());
        ctx.provide_arc(provider_registry);
        ctx.provide_arc(agent_registry);
        ctx.provide_arc(tools);
        ctx.provide(ares_agent::EmergencyStop::new(false));
        ctx.provide(ares_agent::ContextProviderHandle::new(Arc::new(
            ares_agent::context_provider::NoOpContextProvider,
        )));
        ctx.provide(ares_store::FleetSecrets::new());
        ctx.provide(ActiveRuns::new());
        ctx.provide_arc(Arc::new(ares_agent::execution::Execute::new()));

        let auth = Arc::new(AuthService::new(
            "test-1-13-jwt-secret-not-real".to_string(),
            900,
            604_800,
        ));
        let app = Router::new()
            .nest(
                "/api",
                ares_http::api::routes::create_router(auth, tenant_db.clone()),
            )
            .with_state(ctx);

        let tenant = tenant_db
            .create_tenant(unique("t113"), TenantTier::Enterprise)
            .await
            .expect("create the tenant");
        ares_store::tenant_allowlist::TenantAllowlistStore::new(&pool)
            .allow_model(&tenant.id, STUB_MODEL)
            .await
            .expect("allow the stub model");
        let (_, api_key) = tenant_db
            .create_api_key(&tenant.id, "t113-key".to_string(), None, None)
            .await
            .expect("create the API key");
        create_tenant_agent(
            &pool,
            &tenant.id,
            CreateTenantAgentRequest {
                agent_name: AGENT.to_string(),
                display_name: "Metering probe".to_string(),
                description: None,
                config: json!({
                    "model": agent_model,
                    "system_prompt": "metering probe",
                    "tools": [],
                    "max_tool_iterations": 5,
                    "parallel_tools": false
                }),
            },
        )
        .await
        .expect("create the tenant agent");

        Some(Self {
            app,
            pool,
            tenant_id: tenant.id,
            api_key,
            stub_calls,
        })
    }

    /// One `POST /api/v1/agents/{AGENT}/run`.
    async fn run(&self) -> RunReply {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/agents/{AGENT}/run"))
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .body(Body::from(json!({"message": "hello"}).to_string()))
            .expect("build the run request");
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the run response");
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            panic!(
                "the run response is JSON: {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        RunReply { status, body }
    }

    fn model_calls(&self) -> usize {
        self.stub_calls.load(Ordering::SeqCst)
    }

    /// Turn the tenant's no-retain flag off (it defaults on since migration 035), so the run's
    /// `agent_runs.error` keeps the raw text.
    async fn retain_errors(&self) {
        sqlx::query("UPDATE tenants SET no_retain = FALSE WHERE id = $1")
            .bind(&self.tenant_id)
            .execute(&self.pool)
            .await
            .expect("turn no-retain off for the scratch tenant");
    }

    async fn run_row(&self, run_id: &str) -> RunRow {
        let row: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, model_name, provider_name, error FROM agent_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .expect("the run's agent_runs row");
        RunRow {
            status: row.0,
            model_name: row.1,
            provider_name: row.2,
            error: row.3,
        }
    }

    /// The tenant's one `usage_events` row. `track_usage` writes it in a spawned task after
    /// the response, so poll with a deadline.
    async fn usage_row(&self) -> UsageRow {
        let started = Instant::now();
        loop {
            let rows: Vec<(Option<String>, bool, Option<String>, Option<String>)> =
                sqlx::query_as(
                    "SELECT reason_code, success, model_name, provider_name FROM usage_events \
                     WHERE tenant_id = $1",
                )
                .bind(&self.tenant_id)
                .fetch_all(&self.pool)
                .await
                .expect("query usage_events");
            if let Some(row) = rows.first() {
                assert_eq!(rows.len(), 1, "one run, one usage_events row: {rows:?}");
                return UsageRow {
                    reason_code: row.0.clone(),
                    success: row.1,
                    model_name: row.2.clone(),
                    provider_name: row.3.clone(),
                };
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no usage_events row for tenant {} within 10 s",
                self.tenant_id
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The tenant's dollar budget (2.12's `tenant_budgets`), with `spent` dollars already
    /// recorded this month through the store write the production sink uses.
    async fn over_usd_budget(&self, limit: &str, spent: &str) {
        let store = RunHistoryStore::new(&self.pool);
        store
            .set_tenant_budget(&SetTenantBudgetRequest {
                tenant_id: self.tenant_id.clone(),
                monthly_limit_usd: limit.parse().expect("a decimal limit"),
                daily_limit_usd: None,
                alert_threshold_pct: 80,
                currency: "USD".to_string(),
            })
            .await
            .expect("set the USD budget");
        let now = chrono::Utc::now().timestamp();
        let spend_run = unique("t113-spend");
        sqlx::query(
            "INSERT INTO agent_runs (id, tenant_id, agent_name, status, input_tokens, \
             output_tokens, duration_ms, created_at) VALUES ($1, $2, $3, 'completed', 0, 0, 0, $4)",
        )
        .bind(&spend_run)
        .bind(&self.tenant_id)
        .bind(AGENT)
        .bind(now)
        .execute(&self.pool)
        .await
        .expect("insert the spend's agent_runs parent");
        store
            .insert_llm_call(&LogLlmCallRequest {
                id: uuid::Uuid::new_v4().to_string(),
                run_id: spend_run,
                tenant_id: self.tenant_id.clone(),
                agent_name: AGENT.to_string(),
                step_index: 0,
                provider: STUB_PROVIDER.to_string(),
                model: STUB_MODEL.to_string(),
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: spent.parse().expect("a decimal spend"),
                latency_ms: 1,
                cached_tokens: None,
                total_time_ms: None,
                status: "success".to_string(),
                error_message: None,
                request_payload: None,
                response_payload: None,
                created_at: now,
            })
            .await
            .expect("record the spend");
    }

    /// A monthly token budget that is already used up.
    async fn over_token_budget(&self) {
        let tokens = TokenBudgetStore::new(&self.pool);
        tokens
            .set_budget(&self.tenant_id, 100, "monthly")
            .await
            .expect("set the token budget");
        tokens
            .record_usage(&self.tenant_id, None, AGENT, STUB_MODEL, 60, 40)
            .await
            .expect("use up the token budget");
    }
}

/// The wire contract that does not change: HTTP 200, `status: failed`, the generic error text.
fn assert_failed_reply(reply: &RunReply) {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["status"], "failed", "{}", reply.body);
    assert_eq!(
        reply.body["error"], "The run failed. See reason_code.",
        "{}",
        reply.body
    );
}

// ---------------------------------------------------------------------------
// The six tests
// ---------------------------------------------------------------------------

/// A provider failure: the run's `usage_events` row carries the failure class the body
/// carries, and `success = false`.
#[tokio::test]
async fn failed_run_meters_reason_code() {
    let Some(fx) = Fixture::new(Stub::Fail, MODEL_ALIAS).await else {
        return;
    };
    let reply = fx.run().await;
    assert_failed_reply(&reply);
    assert_eq!(reply.body["reason_code"], "llm_error", "{}", reply.body);
    assert!(fx.model_calls() >= 1, "the provider was called and failed");

    let usage = fx.usage_row().await;
    assert!(!usage.success, "a failed run bills success = false: {usage:?}");
    assert_eq!(
        usage.reason_code.as_deref(),
        Some("llm_error"),
        "the failure class must reach usage_events: {usage:?}"
    );
}

/// A failure after resolution: `agent_runs` and `usage_events` carry the resolved model and
/// provider (the stub's), not `unknown` and not the config's alias.
#[tokio::test]
async fn failed_run_records_resolved_model_and_provider() {
    let Some(fx) = Fixture::new(Stub::Fail, MODEL_ALIAS).await else {
        return;
    };
    let reply = fx.run().await;
    assert_failed_reply(&reply);
    assert!(fx.model_calls() >= 1, "the failure came after resolution");

    let run = fx.run_row(&reply.run_id()).await;
    assert_eq!(run.status, "failed");
    assert_eq!(run.model_name.as_deref(), Some(STUB_MODEL), "{run:?}");
    assert_eq!(run.provider_name.as_deref(), Some(STUB_PROVIDER), "{run:?}");

    let usage = fx.usage_row().await;
    assert_eq!(usage.model_name.as_deref(), Some(STUB_MODEL), "{usage:?}");
    assert_eq!(usage.provider_name.as_deref(), Some(STUB_PROVIDER), "{usage:?}");
}

/// A failure before the agent is built (its model resolves to nothing): no provider is
/// called, and both rows carry the marker `unresolved` for the model and the provider.
#[tokio::test]
async fn failure_before_resolution_is_marked_unresolved() {
    let Some(fx) = Fixture::new(Stub::Fail, UNKNOWN_MODEL).await else {
        return;
    };
    let reply = fx.run().await;
    assert_failed_reply(&reply);
    assert_eq!(fx.model_calls(), 0, "nothing was resolved, so nothing was called");

    let run = fx.run_row(&reply.run_id()).await;
    assert_eq!(run.status, "failed");
    assert_eq!(run.model_name.as_deref(), Some(UNRESOLVED), "{run:?}");
    assert_eq!(run.provider_name.as_deref(), Some(UNRESOLVED), "{run:?}");

    let usage = fx.usage_row().await;
    assert_eq!(usage.model_name.as_deref(), Some(UNRESOLVED), "{usage:?}");
    assert_eq!(usage.provider_name.as_deref(), Some(UNRESOLVED), "{usage:?}");
    assert_eq!(
        usage.reason_code.as_deref(),
        reply.body["reason_code"].as_str(),
        "the metered class is the wire class: {usage:?}"
    );
}

/// Exactly one `warn` event per failed run, carrying the run, the tenant, the agent, the class
/// and the `AppError` variant, and not the provider's error text; no warn-or-above event of the
/// run carries that text.
#[tokio::test]
async fn failed_run_logs_class_not_text() {
    let Some(fx) = Fixture::new(Stub::Fail, MODEL_ALIAS).await else {
        return;
    };
    fx.retain_errors().await;
    let reply = fx.run().await;
    assert_failed_reply(&reply);
    let run_id = reply.run_id();

    // Not vacuous: the provider's text did reach the error. This tenant has no-retain off, so
    // the database copy holds it raw (today's flag-off behaviour, unchanged).
    let run = fx.run_row(&run_id).await;
    assert!(
        run.error.as_deref().is_some_and(|e| e.contains(STUB_TEXT)),
        "precondition: the provider's text reached the run's error: {run:?}"
    );

    // The capture is global (see `global_capture`): every test's events are in it, so the
    // run's own line is found by its `run_id`.
    let events: Vec<CapturedEvent> = global_capture().0.lock().unwrap().clone();
    assert!(
        events.iter().any(|(_, target, _)| target == "sqlx::query"),
        "the capture is live: it saw the database statements ({} events)",
        events.len()
    );
    let run_warns: Vec<&CapturedEvent> = events
        .iter()
        .filter(|(level, _, fields)| {
            *level == tracing::Level::WARN && field(fields, "run_id") == Some(run_id.as_str())
        })
        .collect();
    assert_eq!(
        run_warns.len(),
        1,
        "exactly one warn event for the failed run; captured: {events:?}"
    );
    let (_, _, fields) = run_warns[0];
    assert_eq!(field(fields, "tenant_id"), Some(fx.tenant_id.as_str()), "{fields:?}");
    assert_eq!(field(fields, "agent_name"), Some(AGENT), "{fields:?}");
    assert_eq!(field(fields, "reason_code"), Some("llm_error"), "{fields:?}");
    assert_eq!(field(fields, "error_variant"), Some("LLM"), "{fields:?}");

    for (level, target, fields) in events.iter() {
        if *level > tracing::Level::WARN {
            continue; // INFO, DEBUG, TRACE
        }
        assert!(
            fields.iter().all(|(_, value)| !value.contains(STUB_TEXT)),
            "a {level} event ({target}) carries the provider's error text: {fields:?}"
        );
    }
}

/// A tenant over its USD budget (`tenant_budgets`) is refused with `budget_exceeded`, not
/// `rate_limited`, and the model is never called. The token budget's refusal gets the same class.
#[tokio::test]
async fn budget_refusal_is_budget_exceeded() {
    let Some(usd) = Fixture::new(Stub::Answer, MODEL_ALIAS).await else {
        return;
    };
    usd.over_usd_budget("10.00", "11.000000").await;
    let reply = usd.run().await;
    assert_failed_reply(&reply);
    assert_eq!(reply.body["reason_code"], "budget_exceeded", "{}", reply.body);
    assert_eq!(usd.model_calls(), 0, "a refused run never calls the model");
    let usage = usd.usage_row().await;
    assert_eq!(usage.reason_code.as_deref(), Some("budget_exceeded"), "{usage:?}");
    assert!(!usage.success, "{usage:?}");
    // The refusal comes after resolution: the names are the resolved ones.
    let run = usd.run_row(&reply.run_id()).await;
    assert_eq!(run.model_name.as_deref(), Some(STUB_MODEL), "{run:?}");
    assert_eq!(run.provider_name.as_deref(), Some(STUB_PROVIDER), "{run:?}");

    let Some(tokens) = Fixture::new(Stub::Answer, MODEL_ALIAS).await else {
        return;
    };
    tokens.over_token_budget().await;
    let reply = tokens.run().await;
    assert_failed_reply(&reply);
    assert_eq!(reply.body["reason_code"], "budget_exceeded", "{}", reply.body);
    assert_eq!(tokens.model_calls(), 0, "a refused run never calls the model");
    let usage = tokens.usage_row().await;
    assert_eq!(usage.reason_code.as_deref(), Some("budget_exceeded"), "{usage:?}");
}

/// The success arm is unchanged: completed, metered success = true, `reason_code` NULL.
#[tokio::test]
async fn completed_run_reason_code_stays_null() {
    let Some(fx) = Fixture::new(Stub::Answer, MODEL_ALIAS).await else {
        return;
    };
    let reply = fx.run().await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["status"], "completed", "{}", reply.body);
    assert!(reply.body["reason_code"].is_null(), "{}", reply.body);
    assert_eq!(fx.model_calls(), 1, "one model call");

    let run = fx.run_row(&reply.run_id()).await;
    assert_eq!(run.status, "completed");
    assert_eq!(run.model_name.as_deref(), Some(STUB_MODEL), "{run:?}");
    assert_eq!(run.provider_name.as_deref(), Some(STUB_PROVIDER), "{run:?}");

    let usage = fx.usage_row().await;
    assert!(usage.success, "{usage:?}");
    assert_eq!(usage.reason_code, None, "a completed run meters no reason_code");
    assert_eq!(usage.model_name.as_deref(), Some(STUB_MODEL), "{usage:?}");
}

// ---------------------------------------------------------------------------
// Log capture (the crate's hand-rolled subscriber, as in `audit_writes_live.rs`: `tracing` is
// already a direct dependency and `tracing-subscriber` is not)
// ---------------------------------------------------------------------------

/// The binary's one capture, installed as the global default by every fixture before its run.
///
/// Not a scoped `set_default`: with a single registered dispatcher, tracing-core 0.1.36 caches
/// a callsite's interest from the default of whichever thread hits it first
/// (`callsite::register` with the `JustOne` rebuilder). The other tests here run failed runs on
/// their own threads with no subscriber, so they can hit the new `warn!` first and cache it as
/// never-enabled for every thread. A global default is the same dispatcher on every thread.
fn global_capture() -> &'static CaptureLog {
    static CAPTURE: std::sync::OnceLock<CaptureLog> = std::sync::OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture = CaptureLog::default();
        tracing::subscriber::set_global_default(capture.clone())
            .expect("nothing else sets a global subscriber in this test binary");
        capture
    })
}

#[derive(Clone, Default)]
struct CaptureLog(Arc<Mutex<Vec<CapturedEvent>>>);

/// One captured event: its level, its target and its `(field, value)` pairs.
type CapturedEvent = (tracing::Level, String, Vec<(String, String)>);

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

struct FieldVisitor(Vec<(String, String)>);

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_string(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
}

impl tracing::Subscriber for CaptureLog {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = FieldVisitor(Vec::new());
        event.record(&mut visitor);
        self.0.lock().unwrap().push((
            *event.metadata().level(),
            event.metadata().target().to_string(),
            visitor.0,
        ));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}
