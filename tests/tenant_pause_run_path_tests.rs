//! Run-path proof for the per-tenant pause switch.
//!
//! `tenant_pause_tests.rs` covers the flag itself. This file covers the part
//! that decides whether the feature is real: a paused tenant's run must be
//! refused **without spending a model call**.
//!
//! Two properties this file is built around:
//!
//! 1. **It asserts at the provider boundary.** The mock provider increments a
//!    counter on every `/api/chat` request. A paused run that still reached the
//!    provider would bump it. Asserting only "zero rows in `run_llm_calls`"
//!    would be weaker than it looks: that row may be written after the response
//!    returns, so it could read zero whether or not the model was called.
//!
//! 2. **It proves the 503 is the pause and not a broken server.** v1 run
//!    handlers delegate to `Execute`; without that provided, the handler returns
//!    503 of its own. So the same server also runs an **unpaused** tenant, which
//!    must succeed. Without that, a 503 from the paused tenant would prove
//!    nothing at all.
//!
//! The server wiring is copied from `v1_tenant_agent_runtime_tests.rs` because
//! that file's mock does not count. Everything else — config shape, state
//! provides, tenant provisioning — is copied rather than re-invented, so the
//! two suites cannot drift apart silently.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::{
    routing::{get, post},
    Json, Router,
};
use axum_test::TestServer;
use serde_json::{json, Value};
use uuid::Uuid;

use cordis::Context;

use ares_agent::AgentRegistry;
use ares_http::{
    auth::jwt::AuthService,
    config::{AuthConfig as TomlAuthConfig, ServerConfig as TomlServerConfig},
    overlay::{
        AgentConfig, AresConfig, BillingConfig, DatabaseConfig as TomlDatabaseConfig,
        DynamicConfigPaths, ModelConfig, ProviderConfig, RagConfig,
    },
    AresConfigManager, DynamicConfigManager,
};
use ares_llm::{ConfigBasedLLMFactory, ProviderRegistry};
use ares_store::tenant_agents::{create_tenant_agent, CreateTenantAgentRequest};
use ares_store::TenantDb;
use ares_tools::Tools;
use ares_types::models::TenantTier;

/// Provider-boundary evidence, per mock server rather than process-global.
///
/// libtest runs `#[tokio::test]`s on parallel threads in one process. A
/// process-global counter would be reset by one test while another is asserting
/// on it, so a green run would be luck rather than evidence. Each server owns
/// its own counter and the test holds the `Arc` it was handed.
#[derive(Clone, Default)]
struct ProviderCalls(Arc<AtomicUsize>);

impl ProviderCalls {
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.0.store(0, Ordering::SeqCst);
    }
}

async fn counting_ollama_chat(
    axum::extract::State(calls): axum::extract::State<ProviderCalls>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    calls.0.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "model": payload["model"].as_str().unwrap_or("test-model"),
        "created_at": "2026-05-21T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": "SYSTEM_PROMPT=mock"
        },
        "done": true,
        "total_duration": 1,
        "load_duration": 1,
        "prompt_eval_count": 1,
        "prompt_eval_duration": 1,
        "eval_count": 1,
        "eval_duration": 1
    }))
}

async fn spawn_counting_mock_provider() -> (String, ProviderCalls) {
    let calls = ProviderCalls::default();
    let app = Router::new()
        .route("/api/chat", post(counting_ollama_chat))
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock provider");
    let addr = listener.local_addr().expect("mock provider addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock provider");
    });
    (format!("http://{}", addr), calls)
}

fn unique_name(prefix: &str) -> String {
    format!("{}-{}", prefix, Uuid::new_v4())
}

async fn create_counting_test_server() -> (TestServer, Arc<TenantDb>, ProviderCalls) {
    let db = common::test_db::create_test_db().await;
    let auth_service = AuthService::new(
        "test_jwt_secret_key_for_testing_only".to_string(),
        900,
        604800,
    );

    let (provider_url, provider_calls) = spawn_counting_mock_provider().await;

    let mut providers = HashMap::new();
    providers.insert(
        "ollama-local".to_string(),
        ProviderConfig::Ollama {
            api_key_env: "TEST_KEY".to_string(),
            base_url: provider_url,
            default_model: "mock-model".to_string(),
        },
    );
    let mut models = HashMap::new();
    models.insert(
        "default".to_string(),
        ModelConfig {
            provider: "ollama-local".to_string(),
            model: "mock-model".to_string(),
            temperature: 0.0,
            max_tokens: 512,
        },
    );
    let mut agents = HashMap::new();
    agents.insert(
        "product".to_string(),
        AgentConfig {
            model: "default".to_string(),
            system_prompt: Some("registry-product-prompt".to_string()),
            tools: vec![],
            allowed_tools: None,
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
            compaction_enabled: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
        },
    );

    let overlay_config = AresConfig {
        server: TomlServerConfig {
            host: "127.0.0.1".to_string(),
            port: 3000,
            log_level: "debug".to_string(),
            cors_origins: vec!["*".to_string()],
            rate_limit_per_second: 0,
            rate_limit_burst: 0,
        },
        auth: TomlAuthConfig {
            jwt_secret_env: "TEST_JWT_SECRET".to_string(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604800,
            api_key_env: "TEST_API_KEY".to_string(),
        },
        database: TomlDatabaseConfig {
            url: "postgres://postgres:postgres@localhost:5432/ares_test".to_string(),
            max_connections: None,
            qdrant: None,
        },
        nvidia: None,
        config: DynamicConfigPaths::default(),
        providers,
        models,
        tools: HashMap::new(),
        agents,
        workflows: HashMap::new(),
        rag: RagConfig::default(),
        billing: BillingConfig {
            model_pricing: HashMap::new(),
        },
        skills: None,
    };

    let config_manager = Arc::new(AresConfigManager::from_config(overlay_config));
    let provider_registry = Arc::new(ProviderRegistry::from_config(
        config_manager.config().providers.clone(),
        config_manager.config().models.clone(),
        config_manager.config().nvidia.as_ref(),
    ));
    let llm_factory = Arc::new(ConfigBasedLLMFactory::new(
        provider_registry.clone(),
        "default",
    ));
    let tool_registry = Arc::new(Tools::from_static([]));
    let agent_registry = Arc::new(AgentRegistry::from_config(
        config_manager.config().agents.clone(),
        provider_registry.clone(),
        tool_registry.clone(),
    ));

    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let base = temp_dir.path();
    std::fs::create_dir_all(base.join("agents")).unwrap();
    std::fs::create_dir_all(base.join("models")).unwrap();
    std::fs::create_dir_all(base.join("tools")).unwrap();
    std::fs::create_dir_all(base.join("workflows")).unwrap();
    std::fs::create_dir_all(base.join("mcps")).unwrap();

    let dynamic_config = Arc::new(
        DynamicConfigManager::new(
            base.join("agents"),
            base.join("models"),
            base.join("tools"),
            base.join("workflows"),
            base.join("mcps"),
            false,
        )
        .expect("dynamic config"),
    );

    let db = Arc::new(db);
    let tenant_db = Arc::new(TenantDb::new(db.clone()));
    let auth_service = Arc::new(auth_service);
    let llm = Arc::new(
        ares_llm::Llm::new(
            provider_registry.clone(),
            Arc::new(ares_llm::ClientPool::with_defaults()),
            None,
        )
        .with_factory(llm_factory.clone()),
    );
    let skill_engine = Arc::new(ares_agent::skills::SkillEngine::new(
        db.pool.clone(),
        tool_registry.clone(),
        llm,
    ));

    let state: Arc<Context> = Context::new_root();
    // The run handler reads `TenantDb` out of the context, exactly as the
    // sibling `get_agent` handler does. In the running server that service is
    // installed by a cordis plugin; this builder wires it by hand, so it has to
    // provide it explicitly or the handler's own `.expect("not provided")`
    // fires.
    state.provide_arc(tenant_db.clone());
    state.provide_arc(dynamic_config);
    state.provide_arc(llm_factory.clone());
    state.provide_arc(provider_registry.clone());
    state.provide_arc(agent_registry);
    state.provide_arc(tool_registry.clone());
    state.provide_arc(auth_service.clone());
    state.provide(ares_http::api::handlers::deploy::DeployRegistry::default());
    state.provide(ares_http::api::handlers::loops::LoopRegistry::new());
    state.provide(ares_agent::EmergencyStop::new(false));
    state.provide(ares_agent::ContextProviderHandle::new(std::sync::Arc::new(
        ares_agent::context_provider::NoOpContextProvider,
    )));
    state.provide(ares_store::FleetSecrets::new());
    state.provide(ares_http::active_runs::ActiveRuns::new());
    state.provide_arc(skill_engine);
    // v1 chat/run delegate to Execute (capability cutover); without it handlers 503.
    state.provide_arc(Arc::new(ares_agent::execution::Execute::new()));

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .nest(
            "/api",
            ares_http::api::routes::create_router(auth_service.clone(), tenant_db.clone()),
        )
        .with_state(state);

    (
        TestServer::new(app).expect("create test server"),
        tenant_db,
        provider_calls,
    )
}

async fn provision_tenant(tenant_db: &Arc<TenantDb>, prefix: &str) -> (String, String) {
    let tenant = tenant_db
        .create_tenant(unique_name(prefix), TenantTier::Enterprise)
        .await
        .expect("create tenant");
    ares_store::tenant_allowlist::TenantAllowlistStore::new(tenant_db.pool())
        .allow_model(&tenant.id, "mock-model")
        .await
        .expect("allow mock-model");
    let (_, api_key) = tenant_db
        .create_api_key(&tenant.id, format!("{}-key", prefix), None, None)
        .await
        .expect("create api key");
    (tenant.id, api_key)
}

async fn insert_tenant_agent(
    tenant_db: &Arc<TenantDb>,
    tenant_id: &str,
    agent_name: &str,
    system_prompt: &str,
) {
    create_tenant_agent(
        tenant_db.pool(),
        tenant_id,
        CreateTenantAgentRequest {
            agent_name: agent_name.to_string(),
            display_name: format!("{} display", agent_name),
            description: Some(format!("{} description", agent_name)),
            config: json!({
                "model": "default",
                "system_prompt": system_prompt,
                "tools": [],
                "max_tool_iterations": 5,
                "parallel_tools": false
            }),
        },
    )
    .await
    .expect("insert tenant agent");
}

async fn run_as(server: &TestServer, api_key: &str) -> axum_test::TestResponse {
    server
        .post("/api/v1/agents/product/run")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({ "message": "hello" }))
        .await
}

#[tokio::test]
async fn paused_tenant_is_refused_without_spending_a_model_call() {
    let (server, tenant_db, provider_calls) = create_counting_test_server().await;

    let (paused_id, paused_key) = provision_tenant(&tenant_db, "pause-paused").await;
    let (other_id, other_key) = provision_tenant(&tenant_db, "pause-other").await;
    insert_tenant_agent(&tenant_db, &paused_id, "product", "paused-prompt").await;
    insert_tenant_agent(&tenant_db, &other_id, "product", "other-prompt").await;

    tenant_db
        .set_tenant_paused(&paused_id, true, "operator@example.com")
        .await
        .expect("pause should succeed");

    // --- the control: an unpaused tenant runs on this same server ---
    provider_calls.reset();
    let other_response = run_as(&server, &other_key).await;
    assert_eq!(
        other_response.status_code(),
        200,
        "an unpaused tenant must still run — without this, a 503 from the paused \
         tenant would be indistinguishable from a misconfigured server"
    );
    assert!(
        provider_calls.get() > 0,
        "control: the unpaused run must actually reach the provider, otherwise the \
         paused assertion below proves nothing"
    );

    // --- the subject: the paused tenant ---
    provider_calls.reset();
    let paused_response = run_as(&server, &paused_key).await;
    assert_eq!(
        paused_response.status_code(),
        503,
        "a paused tenant must be refused"
    );
    assert_eq!(
        provider_calls.get(),
        0,
        "a paused run must not reach the model provider at all — a pause that still \
         spends a model call is not a pause"
    );
}

#[tokio::test]
async fn unpausing_restores_the_run() {
    let (server, tenant_db, provider_calls) = create_counting_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "pause-restore").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "restore-prompt").await;

    tenant_db
        .set_tenant_paused(&tenant_id, true, "operator@example.com")
        .await
        .expect("pause should succeed");
    assert_eq!(run_as(&server, &api_key).await.status_code(), 503);

    tenant_db
        .set_tenant_paused(&tenant_id, false, "operator@example.com")
        .await
        .expect("unpause should succeed");

    provider_calls.reset();
    assert_eq!(
        run_as(&server, &api_key).await.status_code(),
        200,
        "clearing the pause must restore the run"
    );
    assert!(
        provider_calls.get() > 0,
        "the restored run must reach the provider"
    );
}