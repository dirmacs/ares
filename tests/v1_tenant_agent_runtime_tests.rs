use axum::{
    routing::{get, post},
    Json, Router,
};
use axum_test::TestServer;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use ares::{
    auth::jwt::AuthService,
    db::{
        tenant_agents::{
            create_tenant_agent, update_tenant_agent, CreateTenantAgentRequest,
            UpdateTenantAgentRequest,
        },
        TenantDb,
    },
    models::TenantTier,
    utils::toml_config::{
        AgentConfig, AresConfig, AuthConfig as TomlAuthConfig, BillingConfig,
        DatabaseConfig as TomlDatabaseConfig, DynamicConfigPaths, ModelConfig, ProviderConfig,
        RagConfig, ServerConfig as TomlServerConfig,
    },
    AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory, Context,
    DynamicConfigManager, ProviderRegistry, ToolRegistry,
};

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
        ProviderConfig::OpenAI {
            api_key_env: "TEST_KEY".to_string(),
            api_base: mock_ollama_url,
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
        },
    );

    let ares_config = AresConfig {
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

    let config_manager = Arc::new(AresConfigManager::from_config(ares_config));
    let provider_registry = Arc::new(ProviderRegistry::from_config(&config_manager.config()));
    let llm_factory = Arc::new(ConfigBasedLLMFactory::new(
        provider_registry.clone(),
        "default",
    ));
    let tool_registry = Arc::new(ToolRegistry::with_config(&config_manager.config()));
    let agent_registry = Arc::new(AgentRegistry::from_config(
        &config_manager.config(),
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
    let tenant_db = Arc::new(ares::db::TenantDb::new(db.clone()));
    let auth_service = Arc::new(auth_service);
    let runtime_tool_registry = Arc::new(ares::RuntimeToolRegistry::new(tenant_db.pool().clone()));
    let skill_engine = Arc::new(ares::skill_engine::SkillEngine::new(
        tenant_db.pool().clone(),
        tool_registry.clone(),
        Arc::new(ares::RuntimeToolRegistry::new(tenant_db.pool().clone())),
        llm_factory.clone(),
        config_manager.clone(),
    ));

    let state: AppState = Context::new_root();
    state.provide_arc(config_manager.clone());
    state.provide_arc(dynamic_config);
    state.provide_arc(db.clone());
    state.provide_arc(tenant_db.clone());
    state.provide_arc(llm_factory.clone());
    state.provide_arc(provider_registry.clone());
    state.provide_arc(agent_registry);
    state.provide(ares::context_services::ToolRegistryService(
        tool_registry.clone(),
    ));
    state.provide_arc(auth_service.clone());
    state.provide(ares::api::handlers::deploy::DeployRegistry::default());
    state.provide(ares::api::handlers::loops::LoopRegistry::new());
    state.provide(ares::context_services::EmergencyStop::new(false));
    state.provide(ares::agents::ContextProviderHandle::new(std::sync::Arc::new(ares::agents::context_provider::NoOpContextProvider)));
    state.provide(ares_config::fleet_secrets::FleetSecrets::new());
    state.provide_arc(runtime_tool_registry.clone());
    state.provide(ares::active_runs::ActiveRuns::new());
    state.provide_arc(skill_engine);

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .nest(
            "/api",
            ares::api::routes::create_router(auth_service.clone(), tenant_db.clone()),
        )
        .with_state(state);

    (TestServer::new(app).expect("create test server"), tenant_db)
}

async fn provision_tenant(tenant_db: &Arc<TenantDb>, prefix: &str) -> (String, String) {
    let tenant = tenant_db
        .create_tenant(unique_name(prefix), TenantTier::Enterprise)
        .await
        .expect("create tenant");
    let (_, api_key) = tenant_db
        .create_api_key(&tenant.id, format!("{}-key", prefix))
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
async fn test_v1_chat_uses_tenant_agent_config_over_registry() {
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
    assert_eq!(body["agent"], "product (tenant_db)");
    assert_eq!(body["response"], "SYSTEM_PROMPT=tenant-product-prompt");
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
    assert_eq!(body["agent"], "product (registry)");
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
    assert_eq!(body_a["response"], "SYSTEM_PROMPT=tenant-a-product");
    assert_eq!(body_b["response"], "SYSTEM_PROMPT=tenant-b-product");
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

    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert_eq!(body["agent"], "some-agent (tenant_db)");
    assert_eq!(body["response"], "SYSTEM_PROMPT=tenant-custom-agent-prompt");
}

#[tokio::test]
async fn test_v1_chat_rejects_disabled_tenant_agent_without_fallback() {
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

    assert_eq!(response.status_code(), 404);
    let body: Value = response.json();
    assert!(body["error"].as_str().unwrap().contains("disabled"));
}

#[tokio::test]
async fn test_v1_chat_rejects_invalid_tenant_agent_config_without_fallback() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "invalid-agent").await;

    create_tenant_agent(
        tenant_db.pool(),
        &tenant_id,
        CreateTenantAgentRequest {
            agent_name: "product".to_string(),
            display_name: "invalid product".to_string(),
            description: None,
            config: json!({
                "system_prompt": "broken"
            }),
        },
    )
    .await
    .expect("insert invalid tenant agent");

    let response = server
        .post("/api/v1/chat")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "message": "hello",
            "agent_type": "product"
        }))
        .await;

    assert_eq!(response.status_code(), 500);
    let body: Value = response.json();
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("missing a valid non-empty 'model'"));
}

#[tokio::test]
async fn test_v1_run_agent_executes_tenant_agent_config() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "run-agent").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "run-agent-tenant-prompt").await;

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
        "SYSTEM_PROMPT=run-agent-tenant-prompt"
    );
}
