//! End-to-end: a trigger-driven agent run rolls up into `run_costs`.
//!
//! The store integration test `integration_aggregate_run_cost_writes_run_costs_row` proves
//! `RunHistoryStore::aggregate_run_cost` arithmetic, but not that `execute_triggered_agent`
//! calls `spawn_aggregate_run_cost` after a real run with `run_llm_calls` under the same
//! `run_id`. Previously, trigger paths used a no-op stub for aggregation while Execute could
//! mint a different run id and (without an observability sink) write no LLM rows for the
//! trigger parent — so billing looked wired in review but was empty in production.
//!
//! This test drives `execute_triggered_agent` with the same scratch DB + tenant/agent seeding
//! pattern as `dollar_budget.rs`, stub LLM via a loopback provider (as in `run_residency.rs`),
//! then polls `get_run_cost` because aggregation is spawned with `tokio::spawn`.
//!
//! Needs `TEST_DATABASE_URL` when executed (do not run while another harness holds the DB).
#![cfg(all(feature = "postgres", feature = "trigger"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ares_agent::context_provider::{ContextProviderHandle, NoOpContextProvider};
use ares_agent::trigger::execute_triggered_agent;
use ares_agent::{AgentConfig, AgentRegistry, Execute};
use ares_llm::{ModelConfig, ProviderConfig, ProviderRegistry};
use ares_store::run_history::RunHistoryStore;
use ares_store::schedules::EventTrigger;
use ares_store::tenant_agents::{CreateTenantAgentRequest, create_tenant_agent};
use ares_store::tenant_allowlist::TenantAllowlistStore;
use ares_store::{FleetSecrets, PostgresClient, TenantDb};
use ares_tools::{Tool, Tools};
use axum::Router;
use axum::response::IntoResponse;
use cordis::Context;
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::PgPool;

const AGENT: &str = "run-cost-billing-agent";
const PROVIDER: &str = "run-cost-stub-provider";
const MODEL: &str = "run-cost-stub-model";
const MODEL_ALIAS: &str = "run-cost-alias";

/// Ollama chat shape: 3 prompt + 2 completion tokens (see stub JSON below).
const EXPECTED_PROMPT_TOKENS: i64 = 3;
const EXPECTED_COMPLETION_TOKENS: i64 = 2;
const EXPECTED_LLM_CALLS: i32 = 1;

fn expected_cost() -> Decimal {
    Decimal::new((EXPECTED_PROMPT_TOKENS + EXPECTED_COMPLETION_TOKENS) * 2, 6)
}

struct Fixture {
    ctx: Arc<Context>,
    pool: PgPool,
    tenant_id: String,
    stub_calls: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new() -> Self {
        let pool = ares_test_support::pool().await;
        let tenant_id = format!("run-cost-billing-{}", uuid::Uuid::new_v4());
        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) \
             VALUES ($1, $1, 'free', 1, 1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&tenant_id)
        .execute(&pool)
        .await
        .expect("insert the scratch tenant");

        let (stub_base, stub_calls) = spawn_ollama_stub().await;

        let mut providers = ProviderRegistry::new();
        providers.register_provider(
            PROVIDER,
            ProviderConfig::Ollama {
                api_key_env: "ARES_RUN_COST_STUB_UNUSED".to_string(),
                base_url: stub_base,
                default_model: MODEL.to_string(),
            },
        );
        providers.register_model(
            MODEL_ALIAS,
            ModelConfig {
                provider: PROVIDER.to_string(),
                model: MODEL.to_string(),
                temperature: 0.0,
                max_tokens: 64,
            },
        );
        let providers = Arc::new(providers);

        let mut agent_registry = AgentRegistry::new(
            providers.clone(),
            Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
        );
        agent_registry.register(
            AGENT,
            AgentConfig {
                model: MODEL_ALIAS.to_string(),
                system_prompt: Some("run cost billing test".to_string()),
                tools: vec![],
                max_tool_iterations: 3,
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
            },
        );
        let agent_registry = Arc::new(agent_registry);

        let root = Context::new_root();
        root.provide(cordis::EventsService::new());
        root.provide(TenantDb::new(Arc::new(PostgresClient {
            pool: pool.clone(),
        })));
        root.provide(FleetSecrets::new());
        root.provide(ContextProviderHandle::new(Arc::new(NoOpContextProvider)));

        TenantAllowlistStore::new(&pool)
            .allow_model(&tenant_id, MODEL)
            .await
            .expect("allow the stub model");

        let exec = Arc::new(Execute::new().with_agent_registry(agent_registry));
        root.provide_arc(exec);

        create_tenant_agent(
            &pool,
            &tenant_id,
            CreateTenantAgentRequest {
                agent_name: AGENT.to_string(),
                display_name: "run cost billing".to_string(),
                description: None,
                config: json!({
                    "model": MODEL_ALIAS,
                    "system_prompt": "run cost billing test",
                    "tools": [],
                    "max_tool_iterations": 3,
                    "parallel_tools": false
                }),
            },
        )
        .await
        .expect("create the tenant agent row");

        Self {
            ctx: root,
            pool,
            tenant_id,
            stub_calls,
        }
    }

    async fn run_trigger(&self, message: &str) -> Result<(), String> {
        let trigger = EventTrigger {
            id: format!("trigger-run-cost-{}", uuid::Uuid::new_v4()),
            tenant_id: self.tenant_id.clone(),
            name: "run cost billing".to_string(),
            event_type: "webhook".to_string(),
            event_config: json!({}),
            target_agent: AGENT.to_string(),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        execute_triggered_agent(&trigger, message, &self.ctx).await
    }

    async fn latest_run_id(&self) -> String {
        let row: (String,) = sqlx::query_as(
            "SELECT id FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&self.tenant_id)
        .bind(AGENT)
        .fetch_one(&self.pool)
        .await
        .expect("agent_runs row for the trigger run");
        row.0
    }

    async fn poll_run_cost(&self, run_id: &str) -> ares_store::run_history::RunCost {
        let store = RunHistoryStore::new(&self.pool);
        let started = Instant::now();
        loop {
            if let Some(cost) = store
                .get_run_cost(run_id)
                .await
                .expect("get_run_cost query")
            {
                return cost;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no run_costs row for run {run_id} within 10 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn spawn_ollama_stub() -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let app = Router::new().fallback(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({
                "model": "stub",
                "created_at": "2026-10-01T00:00:00Z",
                "message": {"role": "assistant", "content": "stub answer"},
                "done": true,
                "total_duration": 1,
                "load_duration": 1,
                "prompt_eval_count": EXPECTED_PROMPT_TOKENS,
                "prompt_eval_duration": 1,
                "eval_count": EXPECTED_COMPLETION_TOKENS,
                "eval_duration": 1
            }))
            .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the stub provider");
    let addr = listener.local_addr().expect("stub provider address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve the stub provider");
    });
    (format!("http://{addr}"), calls)
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_run_writes_run_costs_row_after_spawned_aggregation() {
    let fx = Fixture::new().await;
    fx.run_trigger("bill this run")
        .await
        .expect("trigger dispatch succeeds");
    assert!(
        fx.stub_calls.load(Ordering::SeqCst) >= 1,
        "stub provider should have answered at least once"
    );

    let run_id = fx.latest_run_id().await;
    let cost = fx.poll_run_cost(&run_id).await;

    assert_eq!(cost.run_id, run_id);
    assert_eq!(cost.tenant_id, fx.tenant_id);
    assert_eq!(cost.agent_name, AGENT);
    assert_eq!(cost.total_llm_calls, EXPECTED_LLM_CALLS);
    assert_eq!(cost.total_prompt_tokens, EXPECTED_PROMPT_TOKENS);
    assert_eq!(cost.total_completion_tokens, EXPECTED_COMPLETION_TOKENS);
    assert_eq!(cost.total_estimated_cost_usd, expected_cost());
    assert!(cost.total_duration_ms >= 0);
}
