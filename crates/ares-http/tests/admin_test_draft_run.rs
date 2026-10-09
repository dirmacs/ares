//! Issue #53: a Test Draft run (`POST /api/admin/tenants/{tenant}/agents/{agent}/test`)
//! must record its own `agent_runs` row. Every row a run writes —
//! `run_llm_calls`, `run_costs`, `run_tool_calls` — has a foreign key to
//! `agent_runs(id)`, so a draft run without its parent row leaves no trace:
//! each write fails the key and only a WARN is logged.
//!
//! The test drives the admin handler directly as a plain async function with
//! hand-built extractors (`State`, `Path`, `Json`) — the same pattern
//! `audit_writes_live.rs` uses for admin handlers. The handler is the whole
//! HTTP surface here: it takes no `AdminActor`, and a full-router run would
//! have to set the process-wide `ADMIN_API_KEY` for the admin middleware,
//! racing every other test thread in the binary (the middleware itself is
//! unit-tested in `handlers/admin.rs`). The agent runs against a stub Ollama
//! provider on `127.0.0.1:0`, the mock LLM this crate's live tests use
//! (`v1_failed_run_metering_tests.rs`); no real provider is ever called.
//!
//! Requires a live Postgres named by `TEST_DATABASE_URL` (a scratch
//! database). Configured and unreachable, the test panics (naming the
//! variable only); unconfigured, it skips with the crate's convention
//! (`tests/common/mod.rs`).

#![cfg(feature = "postgres")]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use ares_agent::AgentRegistry;
use ares_http::active_runs::ActiveRuns;
use ares_http::api::handlers::admin::shared::TestTenantAgentRequest;
use ares_http::api::handlers::admin::test_tenant_agent_handler;
use ares_llm::{ModelConfig, ProviderConfig, ProviderRegistry};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::TenantDb;
use ares_tools::Tools;
use ares_types::models::TenantTier;
use axum::extract::{Path, State};
use axum::Json;
use cordis::Context;
use serde_json::json;
use sqlx::{PgPool, Row};

const AGENT: &str = "draft-run-probe";
/// The provider name the stub is registered under.
const STUB_PROVIDER: &str = "stub-provider-53";
/// The concrete model id the alias resolves to.
const STUB_MODEL: &str = "stub-model-53";
/// What the draft config names: an alias the registry resolves, not a model id.
const MODEL_ALIAS: &str = "tier-alias-53";

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// Serve the stub on an ephemeral port: every chat call gets Ollama's native
/// `/api/chat` answer shape. Returns its base URL.
async fn spawn_stub() -> String {
    let app = axum::Router::new().fallback(|| async {
        axum::Json(json!({
            "model": STUB_MODEL,
            "created_at": "2026-10-09T00:00:00Z",
            "message": {"role": "assistant", "content": "stub answer"},
            "done": true,
            "total_duration": 1,
            "load_duration": 1,
            "prompt_eval_count": 3,
            "prompt_eval_duration": 1,
            "eval_count": 2,
            "eval_duration": 1
        }))
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
    format!("http://{addr}")
}

/// One tenant with one tenant agent, and a root `Context` carrying everything
/// the handler resolves from it, wired to the stub provider.
struct Fixture {
    ctx: Arc<Context>,
    pool: PgPool,
    tenant_id: String,
}

impl Fixture {
    /// `None` only for the unconfigured skip.
    async fn new() -> Option<Self> {
        common::live_db_url(&common::current_test_name()).await?;
        let pg = Arc::new(ares_test_support::client().await);
        let pool = pg.pool.clone();
        let tenant_db = Arc::new(TenantDb::new(pg.clone()));

        let stub_url = spawn_stub().await;
        let mut providers = HashMap::new();
        providers.insert(
            STUB_PROVIDER.to_string(),
            ProviderConfig::Ollama {
                api_key_env: "ARES_53_UNUSED".to_string(),
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

        let tenant = tenant_db
            .create_tenant(unique("t53"), TenantTier::Enterprise)
            .await
            .expect("create the tenant");
        ares_store::tenant_allowlist::TenantAllowlistStore::new(&pool)
            .allow_model(&tenant.id, STUB_MODEL)
            .await
            .expect("allow the stub model");
        create_tenant_agent(
            &pool,
            &tenant.id,
            CreateTenantAgentRequest {
                agent_name: AGENT.to_string(),
                display_name: "Draft run probe".to_string(),
                description: None,
                config: json!({
                    "model": MODEL_ALIAS,
                    "system_prompt": "draft run probe",
                    "tools": [],
                    "max_tool_iterations": 5,
                    "parallel_tools": false
                }),
            },
        )
        .await
        .expect("create the tenant agent");

        Some(Self {
            ctx,
            pool,
            tenant_id: tenant.id,
        })
    }
}

/// The draft run's `agent_runs` row as the test reads it.
#[derive(Debug)]
struct RunRow {
    id: String,
    status: String,
    agent_config_source: Option<String>,
    agent_config_version: Option<String>,
    request_source: Option<String>,
}

/// A Test Draft run leaves its own `agent_runs` row, marked as a draft admin
/// test and closed out with its final status, and its LLM calls hang off it.
#[tokio::test]
async fn test_draft_run_leaves_agent_runs_row_and_llm_calls() {
    let Some(fx) = Fixture::new().await else {
        return;
    };

    let Json(resp) = test_tenant_agent_handler(
        State(fx.ctx.clone()),
        Path((fx.tenant_id.clone(), AGENT.to_string())),
        Json(TestTenantAgentRequest {
            message: "hello".to_string(),
            config: json!({
                "model": MODEL_ALIAS,
                "system_prompt": "draft run probe",
                "tools": [],
                "max_tool_iterations": 5,
                "parallel_tools": false
            }),
            workspace_id: None,
            use_eruka_context: false,
        }),
    )
    .await
    .expect("the test draft handler answers");
    assert_eq!(resp.status, "completed", "{resp:?}");

    // (a) The run's own `agent_runs` row: exactly one for this tenant agent,
    // marked as a draft admin test, closed out `completed`.
    let rows = sqlx::query(
        "SELECT id, status, agent_config_source, agent_config_version, request_source \
         FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2",
    )
    .bind(&fx.tenant_id)
    .bind(AGENT)
    .fetch_all(&fx.pool)
    .await
    .expect("query agent_runs");
    assert_eq!(
        rows.len(),
        1,
        "the draft run must leave exactly one agent_runs row, got {}",
        rows.len()
    );
    let run = RunRow {
        id: rows[0].get("id"),
        status: rows[0].get("status"),
        agent_config_source: rows[0].get("agent_config_source"),
        agent_config_version: rows[0].get("agent_config_version"),
        request_source: rows[0].get("request_source"),
    };
    assert!(!run.id.is_empty(), "{run:?}");
    assert_eq!(run.status, "completed", "{run:?}");
    assert_eq!(run.agent_config_source.as_deref(), Some("draft"), "{run:?}");
    assert_eq!(
        run.agent_config_version.as_deref(),
        Some("draft"),
        "{run:?}"
    );
    assert_eq!(run.request_source.as_deref(), Some("admin_test"), "{run:?}");

    // (b) At least one `run_llm_calls` row hangs off the run — the FK child
    // the missing parent row used to drop.
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_llm_calls WHERE run_id = $1")
        .bind(&run.id)
        .fetch_one(&fx.pool)
        .await
        .expect("query run_llm_calls");
    assert!(
        calls >= 1,
        "the run's LLM calls must be recorded against {}, got {calls}",
        run.id
    );
}
