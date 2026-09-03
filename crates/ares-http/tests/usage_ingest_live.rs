//! Live proof for `POST /v1/usage/events` (external usage ingest).
//!
//! Drives the real axum router (`ares_http::build_router`) in-process against
//! a staging Postgres (`ares_staging`, fully migrated including
//! `028_usage_ingest.sql`). No LLM calls, no NVIDIA key needed.
//!
//! `#[ignore]` — run explicitly:
//! ```sh
//! cd /opt/ares && export RUSTUP_TOOLCHAIN=1.98.0 CARGO_TARGET_DIR=/opt/ares-target
//! ARES_STAGING_DB_URL='postgres://dirmacs@%2Fvar%2Frun%2Fpostgresql/ares_staging' \
//!   cargo test -p ares-http --features postgres --test usage_ingest_live \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Serve mode (for manual `curl` receipts): set `USAGE_INGEST_SERVE=1` and the
//! test binds `127.0.0.1:18081` until killed:
//! ```sh
//! USAGE_INGEST_SERVE=1 ... cargo test -p ares-http --features postgres \
//!   --test usage_ingest_live -- --ignored --nocapture
//! ```

#![cfg(feature = "postgres")]

use std::sync::Arc;
use std::time::Duration;

use ares_http::active_runs::ActiveRuns;
use ares_http::auth::jwt::AuthService;
use ares_http::config::{AuthConfig, ServerConfig};
use ares_http::overlay::{
    AgentConfig, AresConfig, AresConfigManager, BillingConfig, DatabaseConfig, DynamicConfigPaths,
    RagConfig,
};
use ares_store::TenantDb;
use serde_json::json;

const STAGING_DB_ENV: &str = "ARES_STAGING_DB_URL";
const DEFAULT_STAGING_DB_URL: &str = "postgres://dirmacs@%2Fvar%2Frun%2Fpostgresql/ares_staging";
const TENANT_ID: &str = "stg-ingest-0001";
const AGENT_NAME: &str = "ingest-probe";
const TEST_JWT_SECRET: &str = "usage-ingest-live-test-secret-at-least-32-chars!";

fn staging_db_url() -> String {
    std::env::var(STAGING_DB_ENV)
        .map(|v| v.trim().to_string())
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_STAGING_DB_URL.to_string())
}

fn minimal_config() -> AresConfig {
    AresConfig {
        server: ServerConfig::default(),
        auth: AuthConfig {
            jwt_secret_env: "JWT_SECRET".into(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604_800,
            api_key_env: "API_KEY".into(),
        },
        database: DatabaseConfig::default(),
        nvidia: None,
        config: DynamicConfigPaths::default(),
        providers: std::collections::HashMap::new(),
        models: std::collections::HashMap::new(),
        tools: std::collections::HashMap::new(),
        agents: std::collections::HashMap::from([(
            AGENT_NAME.to_string(),
            AgentConfig {
                model: "test-model".to_string(),
                system_prompt: None,
                tools: vec![],
                allowed_tools: None,
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: std::collections::HashMap::new(),
                compaction_enabled: None,
            },
        )]),
        workflows: std::collections::HashMap::new(),
        rag: RagConfig::default(),
        billing: BillingConfig::default(),
        skills: None,
    }
}

async fn seed(pool: &sqlx::PgPool) {
    let now: i64 = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, 'Ingest Staging', 'enterprise', $2, $2) ON CONFLICT (id) DO NOTHING",
    )
    .bind(TENANT_ID)
    .bind(now)
    .execute(pool)
    .await
    .expect("seed tenant");
    sqlx::query(
        "INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at) VALUES ($1, $2, $3, 'Ingest Probe', 'staging mirror', '{}', true, $4, $4) ON CONFLICT (tenant_id, agent_name) DO NOTHING",
    )
    .bind(format!("stg-agent-{}", uuid::Uuid::new_v4()))
    .bind(TENANT_ID)
    .bind(AGENT_NAME)
    .bind(now)
    .execute(pool)
    .await
    .expect("seed agent");
}

async fn boot() -> (String, String) {
    // `PostgresClient::new_local` ignores its argument and resolves through
    // `DATABASE_URL`, so bridge the staging URL into the environment.
    std::env::set_var("DATABASE_URL", staging_db_url());
    let pg = Arc::new(
        ares_store::PostgresClient::new_local("")
            .await
            .expect("staging Postgres reachable"),
    );
    let tenant_db_probe = TenantDb::new(pg.clone());
    // 028 must be applied: fail loudly otherwise.
    let has_cols: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_name = 'usage_events' AND column_name IN ('request_id', 'outcome_class', 'reason_code')",
    )
    .fetch_one(tenant_db_probe.pool())
    .await
    .expect("schema probe");
    assert_eq!(
        has_cols, 3,
        "staging DB is missing 028_usage_ingest.sql columns"
    );

    let ctx = cordis::Context::new_root();
    ctx.provide_arc(Arc::new(AresConfigManager::from_config(minimal_config())));
    let tenant_db = Arc::new(TenantDb::new(pg.clone()));
    ctx.provide_arc(tenant_db.clone());
    ctx.provide_arc(pg.clone());
    seed(tenant_db.pool()).await;
    ctx.provide_arc(Arc::new(AuthService::new(
        TEST_JWT_SECRET.to_string(),
        900,
        604_800,
    )));
    ctx.provide(ares_agent::EmergencyStop::new(false));
    ctx.provide(ActiveRuns::new());
    ctx.provide(ares_agent::execution::Execute::new());

    let (_api_key, raw_key) = tenant_db
        .create_api_key(TENANT_ID, "usage-ingest-live-test".to_string())
        .await
        .expect("mint staging key");

    let router = ares_http::build_router(ctx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18081")
        .await
        .expect("bind 127.0.0.1:18081");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("axum::serve");
    });
    ("http://127.0.0.1:18081".to_string(), raw_key)
}

fn ev(request_id: &str) -> serde_json::Value {
    json!({
        "agent": AGENT_NAME,
        "model": "openai/gpt-oss-20b",
        "input_tokens": 120,
        "output_tokens": 45,
        "outcome_class": "ok",
        "latency_ms": 812,
        "request_id": request_id,
        "occurred_at": chrono::Utc::now().timestamp(),
    })
}

#[tokio::test]
#[ignore = "live staging test: needs ares_staging Postgres; run explicitly"]
async fn usage_ingest_live() {
    let (base, raw_key) = boot().await;
    if std::env::var("USAGE_INGEST_SERVE").as_deref() == Ok("1") {
        eprintln!("usage_ingest_live: SERVING {base} (kill to stop)");
        futures::future::pending::<()>().await;
        return;
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");
    // `build_router` nests the v1 API under `/api` (`/api/v1/usage/events`).
    let url = format!("{base}/api/v1/usage/events");
    // Unique per run: staging rows persist across runs, and redelivery of an
    // old id would (correctly) come back `deduplicated` here.
    let run = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let rid = |n: u32| format!("live-{run}-{n}");

    // 1. First delivery records.
    let first = http
        .post(&url)
        .bearer_auth(&raw_key)
        .json(&vec![ev(rid(1).as_str())])
        .send()
        .await
        .expect("post events");
    assert_eq!(first.status(), 202, "expected 202 Accepted");
    let first_body: serde_json::Value = first.json().await.expect("202 body");
    eprintln!("first delivery: {first_body}");
    assert_eq!(first_body["results"][0]["status"], "recorded");
    assert!(first_body["results"][0]["id"].is_string());

    // 2. Redelivery deduplicates.
    let second = http
        .post(&url)
        .bearer_auth(&raw_key)
        .json(&vec![ev(rid(1).as_str())])
        .send()
        .await
        .expect("repost events");
    let second_status = second.status();
    let second_body: serde_json::Value = second.json().await.expect("202 body");
    eprintln!("redelivery: {second_body}");
    assert_eq!(second_status, 202);
    assert_eq!(second_body["results"][0]["status"], "deduplicated");

    // 3. Batch of two (handback-zero-tokens + reject) records both.
    let batch = http
        .post(&url)
        .bearer_auth(&raw_key)
        .json(&vec![
            json!({"agent": AGENT_NAME, "input_tokens": 0, "output_tokens": 0, "outcome_class": "ok", "reason_code": "handback:off_topic", "latency_ms": 34, "request_id": rid(2).as_str()}),
            json!({"agent": AGENT_NAME, "input_tokens": 88, "output_tokens": 0, "outcome_class": "client_error", "reason_code": "reject:policy", "latency_ms": 51, "request_id": rid(3).as_str()}),
        ])
        .send()
        .await
        .expect("post batch");
    let batch_status = batch.status();
    let batch_body: serde_json::Value = batch.json().await.expect("batch body");
    eprintln!("batch: {batch_body}");
    assert_eq!(batch_status, 202);
    assert_eq!(batch_body["results"][0]["status"], "recorded");
    assert_eq!(batch_body["results"][1]["status"], "recorded");

    // 4. Unknown agent fails closed.
    let bad_agent = http
        .post(&url)
        .bearer_auth(&raw_key)
        .json(&vec![
            json!({"agent": "no-such-agent", "outcome_class": "ok", "request_id": rid(4).as_str()}),
        ])
        .send()
        .await
        .expect("post bad agent");
    assert_eq!(bad_agent.status(), 400, "unknown agent must fail closed");

    // 5. Content smuggling fails closed (axum maps body-shape rejections to
    // 422; semantic violations are 400 — see the unknown-agent case above).
    let smuggle = http
        .post(&url)
        .bearer_auth(&raw_key)
        .json(&vec![json!({"agent": AGENT_NAME, "outcome_class": "ok", "request_id": rid(5).as_str(), "prompt_text": "must never be accepted"})])
        .send()
        .await
        .expect("post smuggled field");
    assert_eq!(smuggle.status(), 422, "unknown fields must be rejected");

    // 6. No credential fails closed.
    let anon = http
        .post(&url)
        .json(&vec![ev(rid(6).as_str())])
        .send()
        .await
        .expect("post anonymous");
    assert_eq!(anon.status(), 401, "anonymous ingest must be rejected");

    eprintln!("usage_ingest_live: ALL ASSERTIONS PASSED");
}
