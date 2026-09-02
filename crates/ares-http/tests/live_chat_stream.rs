//! Live end-to-end test: `POST /api/chat/stream` with a multimodal image
//! attachment, driving the real axum router (`ares_http::build_router`)
//! in-process against NVIDIA NIM and a real Postgres-backed `Context`.
//!
//! `#[ignore]` — run explicitly:
//! ```sh
//! cd /opt/ares && CARGO_TARGET_DIR=/tmp/ares-0114-target \
//!   cargo test -p ares-http --features postgres --test live_chat_stream -- --ignored --nocapture
//! ```
//!
//! Skips cleanly (prints `SKIPPED ...` and returns without failing) when
//! `NVIDIA_API_KEY` is unset/empty or the test database is unreachable.
//!
//! ## Context wiring
//!
//! Services provided directly on a fresh `cordis::Context::new_root()`
//! (mirrors `ares-http`'s own `routes.rs` `test_app_state()` and
//! `ares-agent`'s `live_tenant_db()` test helpers rather than the
//! TOML-loader/`PluginRegistry` path, which needs `config/cordis-entries.toml`
//! and a running-binary-shaped boot sequence that does not fit an
//! in-process integration test):
//!
//! - `AresConfigManager` (static config with one agent entry, "product")
//! - `ares_store::PostgresClient` + `ares_store::TenantDb` (real Postgres via
//!   `ares_test_support`, migrated + truncated once per test binary)
//! - `ares_http::auth::jwt::AuthService` (JWT issuance/validation)
//! - `ares_agent::EmergencyStop`, `ares_http::active_runs::ActiveRuns` (both
//!   `.expect("not provided")` by the chat/stream handler)
//! - `ares_agent::execution::Execute` (plain `Execute::new()`, no
//!   `AgentRegistry` attached — see below)
//! - `ares_llm::Llm` wrapping a `ProviderRegistry` with exactly one
//!   registered model pointing at a real NVIDIA NIM vision model, built the
//!   same way `ares-llm/tests/live_nvidia.rs::live_stack()` does.
//!
//! Deliberately NOT provided: `ares_tools::Tools`, `ares_agent::registry::
//! AgentRegistry`, `ares_store::FleetSecrets`. Their absence makes
//! `Execute::run_stream` skip the Postgres-resolved-agent path
//! (`prepare_resolved_agent` bails out because no `AgentRegistry` is on
//! `ctx`) and take the LLM+tools fallback path (`execute_stream_fallback`),
//! which calls `Llm::get_client_boxed` + `LLMClient::stream_with_tools_and_history`
//! directly — the same real NVIDIA call `live_vision_parts` makes, just
//! reached through the full HTTP stack instead of a hand-built client.

#![cfg(feature = "postgres")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ares_http::active_runs::ActiveRuns;
use ares_http::auth::jwt::AuthService;
use ares_http::config::{AuthConfig, ServerConfig};
use ares_http::overlay::{
    AgentConfig, AresConfig, AresConfigManager, BillingConfig, DatabaseConfig, DynamicConfigPaths,
    RagConfig,
};
use ares_llm::{ClientPool, Llm, ModelConfig, NvidiaConfig, ProviderRegistry};
use ares_store::TenantDb;
use serde::Deserialize;

const NVIDIA_KEY_ENV: &str = "NVIDIA_API_KEY";
const AGENT_NAME: &str = "product";
const MODEL_KEY: &str = "live-vision";
const TEST_JWT_SECRET: &str = "live-chat-stream-test-secret-at-least-32-characters-long";
const OVERALL_TIMEOUT: Duration = Duration::from_secs(120);

/// 1x1 red PNG, base64-encoded — copied from `ares-llm/tests/live_nvidia.rs`
/// (`TINY_PNG_BASE64`).
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// Mirrors `api::handlers::chat::StreamEvent`'s wire shape (private to the
/// crate, so the test decodes SSE payloads against its own copy).
#[derive(Debug, Deserialize)]
struct StreamEvent {
    event: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    agent: Option<String>,
    #[serde(default)]
    context_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn nvidia_key_present() -> bool {
    std::env::var(NVIDIA_KEY_ENV)
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}

/// Honor `NVIDIA_API_BASE` (e.g. a local sink/proxy), mirroring
/// `ares-llm/tests/live_nvidia.rs::apply_base_override`.
fn apply_base_override(nvidia: &mut NvidiaConfig) {
    if let Ok(base) = std::env::var("NVIDIA_API_BASE") {
        let base = base.trim().trim_end_matches('/').to_string();
        if !base.is_empty() {
            nvidia.models_url = format!("{base}/models");
            nvidia.api_base = base;
        }
    }
}

/// Cheap reachability probe so an unreachable DB skips cleanly instead of
/// panicking inside `ares_test_support::client()`.
async fn db_reachable(url: &str) -> bool {
    match tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(url),
    )
    .await
    {
        Ok(Ok(pool)) => {
            pool.close().await;
            true
        }
        _ => false,
    }
}

/// Static `AresConfig` carrying just enough for `resolve_agent`'s
/// TOML-fallback tier: one agent entry named `AGENT_NAME` (matches
/// `AgentType::Product.as_str()`). The `model` value is metadata only here
/// (billing/active-runs telemetry) — actual model resolution goes through
/// the real `Llm` service registered separately below.
fn minimal_config() -> AresConfig {
    let mut agents = HashMap::new();
    agents.insert(
        AGENT_NAME.to_string(),
        AgentConfig {
            model: MODEL_KEY.to_string(),
            system_prompt: None,
            tools: vec![],
            allowed_tools: None,
            max_tool_iterations: 1,
            parallel_tools: false,
            extra: HashMap::new(),
            compaction_enabled: None,
        },
    );
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
        providers: HashMap::new(),
        models: HashMap::new(),
        tools: HashMap::new(),
        agents,
        workflows: HashMap::new(),
        rag: RagConfig::default(),
        billing: BillingConfig::default(),
        skills: None,
    }
}

async fn run() {
    dotenvy::dotenv().ok();

    if !nvidia_key_present() {
        eprintln!("SKIPPED live_chat_stream_vision_e2e: {NVIDIA_KEY_ENV} is unset or empty");
        return;
    }

    let db_url = ares_test_support::test_db_url();
    if !db_reachable(&db_url).await {
        eprintln!(
            "SKIPPED live_chat_stream_vision_e2e: test database unreachable \
             (checked TEST_DATABASE_URL / DATABASE_URL / unix-socket fallback)"
        );
        return;
    }

    // ---- Build the Cordis Context (see module doc for the exact plugin set). ----
    let ctx = cordis::Context::new_root();

    let config_manager = Arc::new(AresConfigManager::from_config(minimal_config()));
    ctx.provide_arc(config_manager);

    let pg = Arc::new(ares_test_support::client().await);
    let tenant_db = Arc::new(TenantDb::new(pg.clone()));
    ctx.provide_arc(tenant_db);
    ctx.provide_arc(pg);

    let auth_service = Arc::new(AuthService::new(TEST_JWT_SECRET.to_string(), 900, 604_800));
    ctx.provide_arc(auth_service);

    ctx.provide(ares_agent::EmergencyStop::new(false));
    ctx.provide(ActiveRuns::new());
    ctx.provide(ares_agent::execution::Execute::new());

    let mut nvidia = NvidiaConfig::default();
    apply_base_override(&mut nvidia);
    let vision_model = std::env::var("NVIDIA_VISION_MODEL")
        .unwrap_or_else(|_| "meta/llama-3.2-90b-vision-instruct".to_string());
    eprintln!("live_chat_stream_vision_e2e: nvidia vision model = {vision_model}");
    let mut registry = ProviderRegistry::from_config(HashMap::new(), HashMap::new(), Some(&nvidia));
    registry.register_model(
        MODEL_KEY,
        ModelConfig {
            provider: "nvidia".to_string(),
            model: vision_model,
            temperature: 0.2,
            max_tokens: 256,
        },
    );
    registry.set_default_model(MODEL_KEY);
    let llm = Llm::new(
        Arc::new(registry),
        Arc::new(ClientPool::with_defaults()),
        None,
    );
    ctx.provide(llm);
    // `ProviderRegistry::from_config` always synthesizes `bedrock`/`azure`
    // default-provider entries alongside ours; `Llm::get_client`'s
    // capability-based `find_best_model` scoring can rank one of those
    // (unconfigured, will 400) above our single real NVIDIA model. Pin the
    // model explicitly so `get_client_inner`'s fast override path is used
    // instead of capability scoring — the same mechanism `chat.rs` uses for
    // per-request router model pinning.
    ctx.provide(ares_llm::ModelOverride {
        model: MODEL_KEY.to_string(),
    });

    let router = ares_http::build_router(ctx);

    // ---- Bind an ephemeral port and drive the real router in-process. ----
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("axum::serve");
    });

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(100))
        .build()
        .expect("reqwest client");
    let base = format!("http://{addr}");

    // ---- Register a fresh user, get a bearer token. ----
    let email = format!("live-chat-stream-{}@example.test", uuid::Uuid::new_v4());
    let register_resp = http
        .post(format!("{base}/api/auth/register"))
        .json(&serde_json::json!({
            "email": email,
            "password": "Sup3r-Secret-Passw0rd!",
            "name": "Live Chat Stream Test",
        }))
        .send()
        .await
        .expect("register request");
    let register_status = register_resp.status();
    let register_body: serde_json::Value = register_resp
        .json()
        .await
        .unwrap_or_else(|e| panic!("register response body: {e}"));
    assert!(
        register_status.is_success(),
        "register failed: status={register_status} body={register_body}"
    );
    let access_token = register_body["access_token"]
        .as_str()
        .expect("access_token in register response")
        .to_string();

    // ---- POST /api/chat/stream with a multimodal image part. ----
    let chat_body = serde_json::json!({
        "message": "Reply with exactly one word: the color of the image.",
        "agent_type": "product",
        "parts": [
            {"type": "image_base64", "mime": "image/png", "data": TINY_PNG_BASE64}
        ]
    });
    let stream_resp = http
        .post(format!("{base}/api/chat/stream"))
        .bearer_auth(&access_token)
        .json(&chat_body)
        .send()
        .await
        .expect("chat/stream request");
    let stream_status = stream_resp.status();
    let body_text = stream_resp.text().await.expect("chat/stream body text");
    assert!(
        stream_status.is_success(),
        "chat/stream failed: status={stream_status} body={body_text}"
    );

    let events: Vec<StreamEvent> = body_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<StreamEvent>(data).ok())
        .collect();

    let start_count = events.iter().filter(|e| e.event == "start").count();
    let token_events: Vec<&StreamEvent> = events.iter().filter(|e| e.event == "token").collect();
    let done_events: Vec<&StreamEvent> = events.iter().filter(|e| e.event == "done").collect();
    let error_events: Vec<&StreamEvent> = events.iter().filter(|e| e.event == "error").collect();

    eprintln!(
        "live_chat_stream_vision_e2e: sse events total={} start={} token={} done={} error={}",
        events.len(),
        start_count,
        token_events.len(),
        done_events.len(),
        error_events.len()
    );
    if let Some(err) = error_events.first() {
        panic!(
            "chat/stream produced an error event: {:?} (full body: {body_text})",
            err.error
        );
    }
    assert_eq!(
        start_count, 1,
        "expected exactly one start event; body={body_text}"
    );
    assert!(
        !token_events.is_empty(),
        "expected at least one token event; body={body_text}"
    );
    assert!(
        token_events
            .iter()
            .any(|e| e.content.as_deref().is_some_and(|c| !c.is_empty())),
        "expected at least one token event with non-empty content; body={body_text}"
    );
    assert_eq!(
        done_events.len(),
        1,
        "expected exactly one done event; body={body_text}"
    );

    let context_id = events
        .iter()
        .find_map(|e| e.context_id.clone())
        .expect("a start/done event carrying context_id");

    // ---- Fetch the persisted conversation; assert the multimodal shape. ----
    let convo_resp = http
        .get(format!("{base}/api/conversations/{context_id}"))
        .bearer_auth(&access_token)
        .send()
        .await
        .expect("get conversation request");
    let convo_status = convo_resp.status();
    let convo_body: serde_json::Value = convo_resp
        .json()
        .await
        .unwrap_or_else(|e| panic!("conversation response body: {e}"));
    assert!(
        convo_status.is_success(),
        "GET conversation failed: status={convo_status} body={convo_body}"
    );

    let messages = convo_body["messages"]
        .as_array()
        .expect("messages array in conversation response");

    let user_message = messages
        .iter()
        .find(|m| m["role"] == "user")
        .expect("a user message in the persisted conversation");
    let user_parts = user_message["parts"]
        .as_array()
        .expect("user message parts array");
    eprintln!(
        "live_chat_stream_vision_e2e: persisted user message parts = {}",
        serde_json::to_string(user_parts).unwrap_or_default()
    );
    assert_eq!(
        user_parts.len(),
        1,
        "expected exactly one persisted part on the user message; parts={user_parts:?}"
    );
    assert_eq!(user_parts[0]["type"], "image_base64");
    assert_eq!(user_parts[0]["mime"], "image/png");
    assert_eq!(
        user_parts[0]["data"].as_str(),
        Some(TINY_PNG_BASE64),
        "persisted image_base64 data should round-trip byte-for-byte"
    );

    let assistant_message = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("an assistant message in the persisted conversation");
    let assistant_content = assistant_message["content"]
        .as_str()
        .expect("assistant message content string");
    eprintln!(
        "live_chat_stream_vision_e2e: assistant content chars={}",
        assistant_content.len()
    );
    assert!(
        !assistant_content.trim().is_empty(),
        "expected non-empty assistant content; conversation={convo_body}"
    );

    server.abort();
}

#[tokio::test]
#[ignore]
async fn live_chat_stream_vision_e2e() {
    match tokio::time::timeout(OVERALL_TIMEOUT, run()).await {
        Ok(()) => {}
        Err(_) => {
            panic!("live_chat_stream_vision_e2e: exceeded overall {OVERALL_TIMEOUT:?} budget")
        }
    }
}
