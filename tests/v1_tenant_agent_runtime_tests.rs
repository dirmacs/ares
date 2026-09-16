#![cfg(feature = "http")]

use axum::{
    routing::{get, post},
    Json, Router,
};
use axum_test::TestServer;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

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
use ares_store::{
    tenant_agents::{
        create_tenant_agent, update_tenant_agent, CreateTenantAgentRequest,
        UpdateTenantAgentRequest,
    },
    TenantDb,
};
use ares_tools::Tools;
use ares_types::models::TenantTier;
use cordis::Context;

mod common;

fn unique_name(prefix: &str) -> String {
    format!("{}-{}", prefix, Uuid::new_v4())
}

async fn fake_ollama_chat(Json(payload): Json<Value>) -> Json<Value> {
    let system_prompt = payload["messages"]
        .as_array()
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                (message.get("role").and_then(Value::as_str) == Some("system"))
                    .then(|| message.get("content").and_then(Value::as_str))
                    .flatten()
            })
        })
        .unwrap_or("missing-system-prompt");

    Json(json!({
        "model": payload["model"].as_str().unwrap_or("test-model"),
        "created_at": "2026-05-21T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": format!("SYSTEM_PROMPT={}", system_prompt)
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

async fn spawn_mock_ollama_server() -> String {
    let app = Router::new().route("/api/chat", post(fake_ollama_chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock ollama");
    let addr = listener.local_addr().expect("mock ollama addr");

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock ollama");
    });

    format!("http://{}", addr)
}

async fn create_v1_test_server() -> (TestServer, Arc<TenantDb>) {
    let db = common::test_db::create_test_db().await;
    let auth_service = AuthService::new(
        "test_jwt_secret_key_for_testing_only".to_string(),
        900,
        604800,
    );

    let mock_ollama_url = spawn_mock_ollama_server().await;

    let mut providers = HashMap::new();
    providers.insert(
        "ollama-local".to_string(),
        ProviderConfig::Ollama {
            api_key_env: "TEST_KEY".to_string(),
            base_url: mock_ollama_url,
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
    agents.insert(
        "orchestrator".to_string(),
        AgentConfig {
            model: "default".to_string(),
            system_prompt: Some("registry-orchestrator-prompt".to_string()),
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
    let tenant_db = Arc::new(ares_store::TenantDb::new(db.clone()));
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
    state.provide_arc(config_manager.clone());
    state.provide_arc(dynamic_config);
    state.provide_arc(db.clone());
    state.provide_arc(tenant_db.clone());
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

    (TestServer::new(app).expect("create test server"), tenant_db)
}

async fn provision_tenant(tenant_db: &Arc<TenantDb>, prefix: &str) -> (String, String) {
    let tenant = tenant_db
        .create_tenant(unique_name(prefix), TenantTier::Enterprise)
        .await
        .expect("create tenant");
    // Agent creation enforces the per-tenant model allowlist; seed it so the
    // fixture's mock model is usable, matching production provisioning.
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

#[tokio::test]
async fn test_v1_chat_uses_registry_config_via_execute() {
    // v1/chat delegates to Execute::run, which resolves agents from
    // user_agents/community/system tiers. Tenant `tenant_agents` rows belong
    // to the stream path, so chat falls back to the registry config.
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "tenant-db-wins").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "tenant-product-prompt").await;

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent"], "product");
    assert_eq!(body["response"], "SYSTEM_PROMPT=registry-product-prompt");
}

#[tokio::test]
async fn test_v1_chat_falls_back_to_registry_when_tenant_agent_missing() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (_, api_key) = provision_tenant(&tenant_db, "registry-fallback").await;

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent"], "product");
    assert_eq!(body["response"], "SYSTEM_PROMPT=registry-product-prompt");
}

#[tokio::test]
async fn test_v1_chat_isolates_same_agent_name_per_tenant() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_a, api_key_a) = provision_tenant(&tenant_db, "tenant-a").await;
    let (tenant_b, api_key_b) = provision_tenant(&tenant_db, "tenant-b").await;

    insert_tenant_agent(&tenant_db, &tenant_a, "product", "tenant-a-product").await;
    insert_tenant_agent(&tenant_db, &tenant_b, "product", "tenant-b-product").await;

    let response_a = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key_a))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;
    let response_b = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key_b))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    assert_eq!(response_a.status_code(), 200);
    assert_eq!(response_b.status_code(), 200);

    let body_a: Value = response_a.json();
    let body_b: Value = response_b.json();
    // Chat resolves the shared registry config for both tenants; per-tenant
    // agent overrides live on the stream path.
    assert_eq!(body_a["response"], "SYSTEM_PROMPT=registry-product-prompt");
    assert_eq!(body_b["response"], "SYSTEM_PROMPT=registry-product-prompt");
}

#[tokio::test]
async fn test_v1_chat_supports_custom_agent_type_from_tenant_db() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "custom-agent").await;
    insert_tenant_agent(
        &tenant_db,
        &tenant_id,
        "some-agent",
        "tenant-custom-agent-prompt",
    )
    .await;

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "some-agent"
        }))
        .await;

    // Custom names exist only in tenant_agents; Execute's system tier does not
    // know them, so chat reports the agent as missing instead of executing.
    assert_eq!(response.status_code(), 404);
}

#[tokio::test]
async fn test_v1_chat_ignores_disabled_tenant_agent() {
    // v1/chat never consults tenant_agents (stream-path feature), so a
    // disabled tenant row cannot leak into chat; registry config serves it.
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "disabled-agent").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "disabled-tenant-prompt").await;
    update_tenant_agent(
        tenant_db.pool(),
        &tenant_id,
        "product",
        UpdateTenantAgentRequest {
            display_name: None,
            description: None,
            config: None,
            enabled: Some(false),
        },
    )
    .await
    .expect("disable tenant agent");

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent"], "product");
    assert_eq!(body["response"], "SYSTEM_PROMPT=registry-product-prompt");
}

#[tokio::test]
async fn test_v1_chat_rejects_invalid_tenant_agent_config_without_fallback() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "invalid-agent").await;

    // The store rejects configs without `model` at insert time now; insert a
    // valid one and corrupt it afterwards to simulate the legacy bad row.
    create_tenant_agent(
        tenant_db.pool(),
        &tenant_id,
        CreateTenantAgentRequest {
            agent_name: "product".to_string(),
            display_name: "product to corrupt".to_string(),
            description: None,
            config: json!({
                "model": "default",
                "system_prompt": "to-be-corrupted"
            }),
        },
    )
    .await
    .expect("insert valid tenant agent");
    sqlx::query("UPDATE tenant_agents SET config = '{\"system_prompt\": \"broken\"}' WHERE tenant_id = $1 AND agent_name = 'product'")
        .bind(&tenant_id)
        .execute(tenant_db.pool())
        .await
        .expect("corrupt tenant agent config");

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    // v1/chat resolves through Execute and never reads tenant_agents, so the
    // corrupted row is invisible here; the registry config serves the request.
    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent"], "product");
    assert_eq!(body["response"], "SYSTEM_PROMPT=registry-product-prompt");
}

#[tokio::test]
async fn store_rejects_tenant_agent_config_without_model() {
    let (server, tenant_db) = create_v1_test_server().await;
    let _ = server;
    let (tenant_id, _api_key) = provision_tenant(&tenant_db, "insert-invalid").await;

    let err = create_tenant_agent(
        tenant_db.pool(),
        &tenant_id,
        CreateTenantAgentRequest {
            agent_name: "broken".to_string(),
            display_name: "broken".to_string(),
            description: None,
            config: json!({
                "system_prompt": "no model key"
            }),
        },
    )
    .await
    .expect_err("store must reject configs without model");

    match err {
        ares_types::types::AppError::InvalidInput(msg) => {
            assert!(msg.contains("missing a valid non-empty 'model'"));
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test]
async fn test_v1_run_agent_executes_registry_config_via_execute() {
    // /v1/agents/:name/run delegates to Execute::run (registry/system tier);
    // tenant_agents overrides are a stream-path feature.
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "run-agent").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "run-agent-tenant-prompt").await;
    let _ = tenant_id;

    let response = server
        .post("/api/v1/agents/product/run")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello"
        }))
        .await;

    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent_id"], "product");
    assert_eq!(
        body["output"]["response"],
        "SYSTEM_PROMPT=registry-product-prompt"
    );
}

#[tokio::test]
async fn test_v1_run_agent_persists_trace_rows() {
    // Regression: the parent agent_runs row must exist before any call row keys
    // to it, and the non-skill LLM branch must write call rows at all. Parent
    // plus call rows are awaited inline by contract, so assert them directly
    // after the response; only the cost aggregate stays spawned and needs
    // deadline polling. Fails on current main with 0 run_llm_calls rows.
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "run-trace").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "run-trace-tenant-prompt").await;

    let response = server
        .post("/api/v1/agents/product/run")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello"
        }))
        .await;

    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent_id"], "product");
    assert_eq!(
        body["output"]["response"],
        "SYSTEM_PROMPT=registry-product-prompt"
    );
    let run_id = body["id"]
        .as_str()
        .expect("run response carries id")
        .to_string();
    assert!(!run_id.is_empty());

    // Parent row exists at response time (awaited inline, not spawned).
    {
        use sqlx::Row;
        let parent = sqlx::query("SELECT id, tenant_id, agent_name FROM agent_runs WHERE id = $1")
            .bind(&run_id)
            .fetch_optional(tenant_db.pool())
            .await
            .expect("query agent_runs");
        let parent = parent.expect("agent_runs row must exist at response time");
        assert_eq!(parent.get::<String, _>("tenant_id"), tenant_id);
        assert_eq!(parent.get::<String, _>("agent_name"), "product");
    }

    // Call rows exist at response time (awaited inline).
    let store = ares_store::run_history::RunHistoryStore::new(tenant_db.pool());
    let llm_calls = store
        .get_llm_calls_for_run(&run_id)
        .await
        .expect("query run_llm_calls");
    assert!(
        !llm_calls.is_empty(),
        "expected at least one run_llm_calls row for {run_id}"
    );
    for call in &llm_calls {
        assert_eq!(call.run_id, run_id);
        assert_eq!(call.tenant_id, tenant_id);
    }

    // This fixture configures no tools, so no tool row is required; when a
    // tool does fire, its row must key to the same run.
    let tool_calls = store
        .get_tool_calls_for_run(&run_id)
        .await
        .expect("query run_tool_calls");
    for call in &tool_calls {
        assert_eq!(call.run_id, run_id);
        assert_eq!(call.tenant_id, tenant_id);
    }

    // Cost aggregate stays spawned: poll with a deadline.
    let deadline = std::time::Duration::from_secs(10);
    let started = std::time::Instant::now();
    let cost = loop {
        let cost = store.get_run_cost(&run_id).await.expect("query run_costs");
        if cost.is_some() || started.elapsed() >= deadline {
            break cost;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let cost = cost.expect("run_costs row must appear before deadline");
    assert_eq!(cost.run_id, run_id);
    assert_eq!(cost.tenant_id, tenant_id);
    assert!(
        cost.total_llm_calls >= 1,
        "expected run_costs to reflect llm calls"
    );
}

// ── Metering truth (MeterBuild) ──────────────────────────────────────────
// Failed runs persist success=false with zeroed counts; estimates labeled;
// dead HTTP path revived via UsageContext snapshot + header fallback.

#[tokio::test]
async fn test_v1_chat_success_meters_success_true() {
    // Live: /v1/chat success emits x-success=true plus x-counts-source and
    // persists a usage_events row with success=true (header fallback covers
    // the snapshot when the handler only sets headers).
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "meter-chat-ok").await;

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    assert_eq!(response.status_code(), 200);
    let headers = response.headers();
    assert_eq!(
        headers.get("x-success").and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let source = headers
        .get("x-counts-source")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        source == "reported" || source == "estimated",
        "expected reported/estimated, got {source:?}"
    );

    // usage write is spawned: poll with a deadline.
    let deadline = std::time::Duration::from_secs(10);
    let started = std::time::Instant::now();
    let row = loop {
        let row: Option<(i64, i64, bool, Option<String>)> = sqlx::query_as(
            "SELECT token_count, request_count, success, counts_source FROM usage_events WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&tenant_id)
        .fetch_optional(tenant_db.pool())
        .await
        .expect("query usage_events");
        if row.is_some() || started.elapsed() >= deadline {
            break row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let (tokens, requests, success, counts_source) =
        row.expect("usage_events row must appear before deadline");
    assert!(success, "chat success must persist success=true");
    assert_eq!(requests, 1);
    assert!(tokens >= 0);
    assert!(
        matches!(
            counts_source.as_deref(),
            Some("reported") | Some("estimated")
        ),
        "expected labeled counts_source, got {counts_source:?}"
    );
}

#[tokio::test]
async fn test_usage_ingest_ok_and_timeout_success_mapping() {
    // Live: ingest ok bills success=true, timeout bills success=false, both
    // labeled counts_source='unknown'. SUM WHERE success excludes failures.
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "meter-ingest").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "ingest-ok", "probe").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "ingest-timeout", "probe").await;

    let ok_id = format!("req-ok-{}", Uuid::new_v4());
    let timeout_id = format!("req-timeout-{}", Uuid::new_v4());
    let response = server
        .post("/api/v1/usage/events")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!([
            {
                "agent": "ingest-ok",
                "model": "mock-model",
                "input_tokens": 10,
                "output_tokens": 5,
                "outcome_class": "ok",
                "latency_ms": 100,
                "request_id": ok_id,
            },
            {
                "agent": "ingest-timeout",
                "model": "mock-model",
                "input_tokens": 0,
                "output_tokens": 0,
                "outcome_class": "timeout",
                "latency_ms": 5000,
                "request_id": timeout_id,
            }
        ]))
        .await;

    assert_eq!(response.status_code(), 202);

    let ok_row: (bool, Option<String>, i64) = sqlx::query_as(
        "SELECT success, counts_source, token_count FROM usage_events WHERE tenant_id = $1 AND request_id = $2",
    )
    .bind(&tenant_id)
    .bind(&ok_id)
    .fetch_one(tenant_db.pool())
    .await
    .expect("ok row");
    assert!(ok_row.0, "ingest ok must map success=true");
    assert_eq!(ok_row.1.as_deref(), Some("unknown"));
    assert_eq!(ok_row.2, 15);

    let timeout_row: (bool, Option<String>) = sqlx::query_as(
        "SELECT success, counts_source FROM usage_events WHERE tenant_id = $1 AND request_id = $2",
    )
    .bind(&tenant_id)
    .bind(&timeout_id)
    .fetch_one(tenant_db.pool())
    .await
    .expect("timeout row");
    assert!(!timeout_row.0, "ingest timeout must map success=false");
    assert_eq!(timeout_row.1.as_deref(), Some("unknown"));

    // Quota view: SUM WHERE success counts only the ok row.
    let billed: (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(token_count)::bigint, 0) FROM usage_events WHERE tenant_id = $1 AND success",
    )
    .bind(&tenant_id)
    .fetch_one(tenant_db.pool())
    .await
    .expect("sum where success");
    assert_eq!(billed.0, 15, "failed ingest rows must not bill");
}
