//! Item 2.12, round A: a run over the tenant's dollar budget is refused before the model call,
//! on the token budget's own seam (`ConfigurableAgent::preflight_budget_check`).
//!
//! The dollar budget is `tenant_budgets` (`monthly_limit_usd`, optional `daily_limit_usd`,
//! migration 016). Spend is the sum of the recorded per-call cost, `run_llm_calls.estimated_cost_usd`,
//! for the tenant in the period (UTC calendar month, UTC calendar day). The tests seed that spend
//! through `RunHistoryStore::insert_llm_call`, the same write the production sink uses, and run the
//! agent through the public `Agent::execute` entry on both of its model paths (no tools, and the
//! tool loop).
//!
//! Every refusal must be typed (`AppError::RateLimited`, the error the token budget already uses)
//! and must happen before the model is called: the stub model counts its calls and the count is
//! asserted to be zero on every refusal.
//!
//! Every test needs the scratch database named by `TEST_DATABASE_URL`.
#![cfg(feature = "postgres")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ares_agent::{Agent, AgentConfig, AgentResponse, ConfigurableAgent};
use ares_llm::{LLMClient, LLMResponse, LlmStreamEvent};
use ares_store::run_history::{LogLlmCallRequest, RunHistoryStore, SetTenantBudgetRequest};
use ares_store::token_budgets::TokenBudgetStore;
use ares_store::{PostgresClient, TenantDb};
use ares_tools::{Tool, Tools};
use ares_types::types::{AgentContext, AppError, ToolDefinition};
use chrono::{Datelike, NaiveTime, Utc};
use cordis::Context;
use rust_decimal::Decimal;
use sqlx::PgPool;

const AGENT: &str = "dollar-budget-agent";
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
// The fixture: one tenant, one agent bound to a context that carries `TenantDb`.
// ---------------------------------------------------------------------------

/// Which of the agent's two model paths the run takes.
#[derive(Clone, Copy, Debug)]
enum Path {
    /// No tools on the context: the simple `execute` path.
    NoTools,
    /// A `Tools` service on the context: the `execute_with_tools` loop.
    ToolLoop,
}

struct Fixture {
    pool: PgPool,
    tenant: String,
    calls: Arc<AtomicUsize>,
    agent: ConfigurableAgent,
    context: AgentContext,
}

impl Fixture {
    async fn new(path: Path) -> Self {
        let pool = ares_test_support::pool().await;
        let tenant = format!("dollar-budget-{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) \
             VALUES ($1, $1, 'free', 1, 1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&tenant)
        .execute(&pool)
        .await
        .expect("insert the scratch tenant");

        let root = Context::new_root();
        root.provide(TenantDb::new(Arc::new(PostgresClient {
            pool: pool.clone(),
        })));
        if let Path::ToolLoop = path {
            root.provide(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let config = AgentConfig {
            model: "default".to_string(),
            system_prompt: Some("dollar budget test".to_string()),
            tools: vec![],
            max_tool_iterations: 5,
            parallel_tools: false,
            compaction_enabled: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            extra: HashMap::new(),
            allowed_tools: None,
        };
        let agent = ConfigurableAgent::new_from_context(
            &root,
            AGENT,
            &config,
            Box::new(CountingLlm {
                calls: Arc::clone(&calls),
            }),
        );
        let context = AgentContext {
            user_id: tenant.clone(),
            session_id: format!("session-{}", uuid::Uuid::new_v4()),
            conversation_history: vec![],
            user_memory: None,
        };
        Self {
            pool,
            tenant,
            calls,
            agent,
            context,
        }
    }

    /// Set the tenant's dollar budget (`tenant_budgets`).
    async fn set_usd_budget(&self, monthly: &str, daily: Option<&str>) {
        RunHistoryStore::new(&self.pool)
            .set_tenant_budget(&SetTenantBudgetRequest {
                tenant_id: self.tenant.clone(),
                monthly_limit_usd: usd(monthly),
                daily_limit_usd: daily.map(usd),
                alert_threshold_pct: 80,
                currency: "USD".to_string(),
            })
            .await
            .expect("set the tenant USD budget");
    }

    /// Record one LLM call costing `cost` dollars at `created_at` (unix seconds), through the
    /// same store write the production sink uses.
    async fn record_spend(&self, cost: &str, created_at: i64) {
        let run_id = format!("run-{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO agent_runs (id, tenant_id, agent_name, status, input_tokens, \
             output_tokens, duration_ms, created_at) \
             VALUES ($1, $2, $3, 'completed', 0, 0, 0, $4)",
        )
        .bind(&run_id)
        .bind(&self.tenant)
        .bind(AGENT)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .expect("insert the agent_runs parent");
        RunHistoryStore::new(&self.pool)
            .insert_llm_call(&LogLlmCallRequest {
                id: uuid::Uuid::new_v4().to_string(),
                run_id,
                tenant_id: self.tenant.clone(),
                agent_name: AGENT.to_string(),
                step_index: 0,
                provider: "test".to_string(),
                model: "counting-llm".to_string(),
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: usd(cost),
                latency_ms: 1,
                cached_tokens: None,
                total_time_ms: None,
                status: "success".to_string(),
                error_message: None,
                request_payload: None,
                response_payload: None,
                created_at,
            })
            .await
            .expect("record the LLM call cost");
    }

    /// Run the agent once. Returns the result and how many times the model was called.
    async fn run(&self) -> (Result<AgentResponse, AppError>, usize) {
        let result = self.agent.execute("hello", &self.context).await;
        (result, self.calls.load(Ordering::SeqCst))
    }

    /// Best-effort removal of everything the fixture wrote.
    async fn cleanup(&self) {
        for sql in [
            "DELETE FROM run_llm_calls WHERE tenant_id = $1",
            "DELETE FROM agent_runs WHERE tenant_id = $1",
            "DELETE FROM tenant_budgets WHERE tenant_id = $1",
            "DELETE FROM tenant_token_budgets WHERE tenant_id = $1",
            "DELETE FROM token_usage_log WHERE tenant_id = $1",
            "DELETE FROM tenants WHERE id = $1",
        ] {
            let _ = sqlx::query(sql)
                .bind(&self.tenant)
                .execute(&self.pool)
                .await;
        }
    }
}

fn usd(s: &str) -> Decimal {
    s.parse().expect("a decimal literal")
}

/// Unix seconds at UTC midnight of the first day of the current month.
fn month_start() -> i64 {
    let today = Utc::now().date_naive();
    today
        .with_day(1)
        .unwrap_or(today)
        .and_time(NaiveTime::MIN)
        .and_utc()
        .timestamp()
}

/// Unix seconds at UTC midnight today.
fn day_start() -> i64 {
    Utc::now()
        .date_naive()
        .and_time(NaiveTime::MIN)
        .and_utc()
        .timestamp()
}

/// The run must have been refused with the typed budget error, and the model never called.
/// Returns the error text.
fn assert_refused_before_the_model(outcome: (Result<AgentResponse, AppError>, usize)) -> String {
    let (result, calls) = outcome;
    match result {
        Err(AppError::RateLimited(msg)) => {
            assert_eq!(
                calls, 0,
                "the run was refused but the model was still called {calls} time(s): {msg}"
            );
            msg
        }
        Err(other) => panic!("expected the typed budget refusal (RateLimited), got {other:?}"),
        Ok(response) => panic!(
            "expected the run to be refused before the model call, but it ran: the model was \
             called {calls} time(s) and answered {:?}",
            response.content
        ),
    }
}

/// The run must have gone ahead: the model answered exactly once.
fn assert_ran(outcome: (Result<AgentResponse, AppError>, usize), why: &str) {
    let (result, calls) = outcome;
    match result {
        Ok(response) => {
            assert_eq!(response.content, ANSWER, "{why}");
            assert_eq!(calls, 1, "{why}: the model must be called exactly once");
        }
        Err(e) => panic!("{why}: the run must not be refused, got {e:?} (model calls: {calls})"),
    }
}

// ---------------------------------------------------------------------------
// The dollar budget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_over_monthly_usd_budget_is_refused_before_the_model_call() {
    let fx = Fixture::new(Path::NoTools).await;
    fx.set_usd_budget("10.00", None).await;
    // $11.00 this month, in two calls: over the $10.00 limit.
    fx.record_spend("6.000000", Utc::now().timestamp()).await;
    fx.record_spend("5.000000", Utc::now().timestamp()).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    let msg = assert_refused_before_the_model(outcome);
    assert!(
        msg.to_lowercase().contains("monthly"),
        "the refusal must name the monthly limit: {msg}"
    );
}

#[tokio::test]
async fn monthly_usd_budget_refusal_holds_on_the_tool_loop_path() {
    let fx = Fixture::new(Path::ToolLoop).await;
    fx.set_usd_budget("10.00", None).await;
    fx.record_spend("12.500000", Utc::now().timestamp()).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_refused_before_the_model(outcome);
}

#[tokio::test]
async fn run_at_the_monthly_usd_limit_is_refused() {
    let fx = Fixture::new(Path::NoTools).await;
    fx.set_usd_budget("10.00", None).await;
    // Exactly the limit: "at or over" refuses.
    fx.record_spend("10.000000", Utc::now().timestamp()).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_refused_before_the_model(outcome);
}

#[tokio::test]
async fn run_over_daily_usd_budget_is_refused() {
    let fx = Fixture::new(Path::NoTools).await;
    // A large monthly limit that is not reached; the $5.00 daily limit is.
    fx.set_usd_budget("1000.00", Some("5.00")).await;
    fx.record_spend("5.500000", Utc::now().timestamp()).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    let msg = assert_refused_before_the_model(outcome);
    assert!(
        msg.to_lowercase().contains("daily"),
        "the refusal must name the daily limit: {msg}"
    );
}

#[tokio::test]
async fn daily_usd_budget_ignores_spend_from_before_today() {
    let fx = Fixture::new(Path::NoTools).await;
    fx.set_usd_budget("1000.00", Some("5.00")).await;
    // $100 an hour before UTC midnight: not today's spend, and far under the monthly limit
    // whichever month that hour falls in.
    fx.record_spend("100.000000", day_start() - 3600).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_ran(
        outcome,
        "yesterday's spend must not count against the daily limit",
    );
}

#[tokio::test]
async fn monthly_usd_budget_ignores_spend_from_before_this_month() {
    let fx = Fixture::new(Path::NoTools).await;
    fx.set_usd_budget("10.00", None).await;
    // $500 one second before the month began: last month's spend.
    fx.record_spend("500.000000", month_start() - 1).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_ran(
        outcome,
        "last month's spend must not count against this month",
    );
}

#[tokio::test]
async fn another_tenants_spend_does_not_count() {
    let fx = Fixture::new(Path::NoTools).await;
    let other = Fixture::new(Path::NoTools).await;
    fx.set_usd_budget("10.00", Some("5.00")).await;
    // The other tenant spent far over this tenant's limits; this tenant spent nothing.
    other
        .record_spend("999.000000", Utc::now().timestamp())
        .await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    other.cleanup().await;
    assert_ran(outcome, "only the tenant's own recorded spend counts");
}

// ---------------------------------------------------------------------------
// Unchanged behaviour (the ruling's condition 2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tenant_without_usd_budget_is_unchanged() {
    let fx = Fixture::new(Path::NoTools).await;
    // No `tenant_budgets` row at all, and a very large recorded spend (the column is
    // NUMERIC(12,6), so this is close to its ceiling): nothing to enforce.
    fx.record_spend("900000.000000", Utc::now().timestamp())
        .await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_ran(
        outcome,
        "a tenant with no USD budget row runs exactly as before",
    );
}

#[tokio::test]
async fn null_daily_limit_is_not_enforced() {
    let fx = Fixture::new(Path::NoTools).await;
    // A monthly limit with no daily limit: a large day's spend under the monthly limit runs.
    fx.set_usd_budget("1000.00", None).await;
    fx.record_spend("500.000000", Utc::now().timestamp()).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_ran(outcome, "a NULL daily limit is no daily limit");
}

#[tokio::test]
async fn run_under_usd_budget_proceeds() {
    let fx = Fixture::new(Path::NoTools).await;
    fx.set_usd_budget("10.00", Some("5.00")).await;
    // Just under both limits.
    fx.record_spend("4.990000", Utc::now().timestamp()).await;

    let outcome = fx.run().await;
    fx.cleanup().await;
    assert_ran(outcome, "a run under both USD limits proceeds");
}

#[tokio::test]
async fn token_budget_refusal_unchanged() {
    let fx = Fixture::new(Path::NoTools).await;
    // A token budget already used up: the existing check refuses, with its existing text.
    let tokens = TokenBudgetStore::new(&fx.pool);
    tokens
        .set_budget(&fx.tenant, 100, "monthly")
        .await
        .expect("set the token budget");
    tokens
        .record_usage(&fx.tenant, None, AGENT, "counting-llm", 60, 40)
        .await
        .expect("use up the token budget");

    let outcome = fx.run().await;
    let tenant = fx.tenant.clone();
    fx.cleanup().await;
    let msg = assert_refused_before_the_model(outcome);
    assert_eq!(
        msg,
        format!("Tenant {tenant} token budget exceeded (100 / 100)"),
        "the token budget refusal text must not change"
    );
}
