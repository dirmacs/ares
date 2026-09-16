#![cfg(feature = "http")]

//! API-key scopes + TTL + rotation + attribution (KeyScopes).
//!
//! Harness mirrors `tests/v1_tenant_agent_runtime_tests.rs`: in-process axum
//! router via `create_router` nested under `/api`, live Postgres via
//! `common::test_db`, mock Ollama for agent runs. Covers:
//! expired 401, TTL persist/surface, unknown scopes default full,
//! ingest-scope 403 on run but 202 on ingest (with `api_key_id` attribution),
//! rotate old-401/new-200, admin revoke 404 plus audit.

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
    tenant_agents::{create_tenant_agent, CreateTenantAgentRequest},
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

fn agent_config_with_prompt(prompt: &str) -> AgentConfig {
    AgentConfig {
        model: "default".to_string(),
        system_prompt: Some(prompt.to_string()),
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
    }
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
        agent_config_with_prompt("registry-product-prompt"),
    );
    agents.insert(
        "orchestrator".to_string(),
        agent_config_with_prompt("registry-orchestrator-prompt"),
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
async fn expired_key_returns_401() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, _) = provision_tenant(&tenant_db, "expire").await;
    let (key_id, raw) = tenant_db
        .create_api_key(
            &tenant_id,
            "short-lived".into(),
            Some("full".into()),
            Some(1),
        )
        .await
        .expect("create expiring key");
    // Force expiry in the past.
    sqlx::query("UPDATE api_keys SET expires_at = $1 WHERE id = $2")
        .bind(chrono::Utc::now().timestamp() - 10)
        .bind(&key_id.id)
        .execute(tenant_db.pool())
        .await
        .expect("expire key");

    let response = server
        .get("/api/v1/agents")
        .add_header("Authorization", format!("Bearer {}", raw))
        .await;
    assert_eq!(response.status_code(), 401);
}

#[tokio::test]
async fn ttl_persists_and_surfaces() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (_, api_key) = provision_tenant(&tenant_db, "ttl").await;

    let response = server
        .post("/api/v1/api-keys")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({"name": "ttl-key", "expires_in_days": 30}))
        .await;
    assert_eq!(response.status_code(), 200);
    let body: Value = response.json();
    assert!(
        body["key"]["expires_at"].is_string(),
        "expires_at must surface"
    );

    let key_id = body["key"]["id"].as_str().expect("key id").to_string();
    let row: Option<i64> = sqlx::query_scalar("SELECT expires_at FROM api_keys WHERE id = $1")
        .bind(&key_id)
        .fetch_optional(tenant_db.pool())
        .await
        .expect("select expiry");
    let expires_at = row.expect("TTL must persist");
    let now = chrono::Utc::now().timestamp();
    assert!(expires_at > now + 29 * 86_400 && expires_at <= now + 31 * 86_400);

    // Invalid TTLs are 400.
    for bad in [0, 3651] {
        let bad_resp = server
            .post("/api/v1/api-keys")
            .add_header("Authorization", format!("Bearer {}", api_key))
            .json(&json!({"name": "bad-ttl", "expires_in_days": bad}))
            .await;
        assert_eq!(bad_resp.status_code(), 400);
    }
}

#[tokio::test]
async fn unknown_scopes_default_full() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, _) = provision_tenant(&tenant_db, "unknown-scope").await;
    let (_, raw) = tenant_db
        .create_api_key(&tenant_id, "weird".into(), Some("weird".into()), None)
        .await
        .expect("create weird-scope key");
    let ctx = tenant_db
        .verify_api_key(&raw)
        .await
        .expect("verify")
        .expect("weird scope must verify");
    assert_eq!(ctx.scopes, "full");
    assert!(ctx.is_full_scope());

    // HTTP: unknown-scope key behaves as full (list succeeds, not 403).
    let resp = server
        .get("/api/v1/agents")
        .add_header("Authorization", format!("Bearer {}", raw))
        .await;
    assert_eq!(resp.status_code(), 200);
}

#[tokio::test]
async fn ingest_scope_403_on_run_but_202_on_ingest() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, _) = provision_tenant(&tenant_db, "least-priv").await;
    insert_tenant_agent(&tenant_db, &tenant_id, "product", "tenant-product-prompt").await;
    let (_, ingest_key) = tenant_db
        .create_api_key(
            &tenant_id,
            "ingest-only".into(),
            Some("ingest".into()),
            None,
        )
        .await
        .expect("create ingest key");

    // Least-privilege: agent run is forbidden.
    let run_resp = server
        .post("/api/v1/agents/product/run")
        .add_header("Authorization", format!("Bearer {}", ingest_key))
        .json(&json!({"message": "hello"}))
        .await;
    assert_eq!(run_resp.status_code(), 403);
    let err: Value = run_resp.json();
    assert_eq!(err["error"], "insufficient_scope");

    // Ingest path is allowed.
    let request_id = format!("req-{}", Uuid::new_v4());
    let ingest_resp = server
        .post("/api/v1/usage/events")
        .add_header("Authorization", format!("Bearer {}", ingest_key))
        .json(&json!([{
            "agent": "product",
            "input_tokens": 10,
            "output_tokens": 5,
            "outcome_class": "ok",
            "request_id": request_id,
        }]))
        .await;
    assert_eq!(ingest_resp.status_code(), 202);

    // Attribution flows to the usage row.
    let key_id: Option<String> = sqlx::query_scalar(
        "SELECT api_key_id FROM usage_events WHERE tenant_id = $1 AND request_id = $2",
    )
    .bind(&tenant_id)
    .bind(&request_id)
    .fetch_optional(tenant_db.pool())
    .await
    .expect("select attribution");
    let key_id = key_id.expect("api_key_id must be attributed");
    let stored: String =
        sqlx::query_scalar("SELECT id FROM api_keys WHERE tenant_id = $1 AND scopes = 'ingest' ORDER BY created_at DESC LIMIT 1")
            .bind(&tenant_id)
            .fetch_one(tenant_db.pool())
            .await
            .expect("select key id");
    assert_eq!(key_id, stored);
}

#[tokio::test]
async fn rotate_old_401_new_200() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, api_key) = provision_tenant(&tenant_db, "rotate").await;
    let keys = tenant_db
        .list_api_keys(&tenant_id)
        .await
        .expect("list keys");
    // Self-rotation: the only active key is both the auth credential and the
    // rotation target, so mint-first then revoke proves old-401/new-200.
    let old_id = keys
        .into_iter()
        .find(|k| k.is_active)
        .expect("active key")
        .id;

    let resp = server
        .post(&format!("/api/v1/api-keys/{}/rotate", old_id))
        .add_header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    let new_secret = body["secret"]
        .as_str()
        .expect("once-only secret")
        .to_string();
    assert!(!new_secret.is_empty());
    assert_ne!(new_secret, api_key);

    // Old secret is revoked: 401.
    let old_resp = server
        .get("/api/v1/agents")
        .add_header("Authorization", format!("Bearer {}", api_key))
        .await;
    assert_eq!(old_resp.status_code(), 401);

    // New secret works: 200.
    let new_resp = server
        .get("/api/v1/agents")
        .add_header("Authorization", format!("Bearer {}", new_secret))
        .await;
    assert_eq!(new_resp.status_code(), 200);
}

#[tokio::test]
async fn admin_revoke_404_plus_audit() {
    let (server, tenant_db) = create_v1_test_server().await;
    let (tenant_id, _) = provision_tenant(&tenant_db, "admin-revoke").await;
    let (key_id, _) = tenant_db
        .create_api_key(&tenant_id, "to-revoke".into(), None, None)
        .await
        .expect("create key");

    std::env::set_var("ADMIN_API_KEY", "test-admin-secret-scopes");
    let admin_header = ("x-admin-secret", "test-admin-secret-scopes");

    // Unknown id is 404.
    let not_found = server
        .delete(&format!(
            "/api/admin/tenants/{}/api-keys/no-such-key",
            tenant_id
        ))
        .add_header(admin_header.0, admin_header.1)
        .await;
    assert_eq!(not_found.status_code(), 404);

    // Real revoke succeeds and audits.
    let ok = server
        .delete(&format!(
            "/api/admin/tenants/{}/api-keys/{}",
            tenant_id, key_id.id
        ))
        .add_header(admin_header.0, admin_header.1)
        .await;
    assert_eq!(ok.status_code(), 200);

    // Audit row exists (poll briefly; handler spawns the insert).
    let mut found = false;
    for _ in 0..20 {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM admin_audit_log WHERE action = 'revoke_api_key' AND resource_id = $1",
        )
        .bind(&key_id.id)
        .fetch_one(tenant_db.pool())
        .await
        .expect("audit select");
        if count >= 1 {
            found = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(found, "revoke must write an audit row");
}
