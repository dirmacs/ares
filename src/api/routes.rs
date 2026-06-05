use crate::auth::jwt::AuthService;
use crate::db::tenants::TenantDb;
use crate::AppState;

use axum::{
    extract::Request,
    middleware::{self, Next},
    routing::{delete, get, post, put},
    Router,
};
use std::sync::Arc;

use crate::api::handlers::deploy;
use crate::api::handlers::loops;

/// Creates the main API router with all routes configured.
///
/// Routes are split into public (no auth), protected (requires JWT), and admin (requires admin secret).
/// `tenant_db` is injected into request extensions so `track_usage` middleware can record billing events.
pub fn create_router(auth_service: Arc<AuthService>, tenant_db: Arc<TenantDb>) -> Router<AppState> {
    // Clone for v1 routes (API key auth)
    let tenant_db_for_v1 = tenant_db.clone();

    let public_routes = Router::new()
        // Public routes (no auth required)
        .route("/auth/register", post(crate::api::handlers::auth::register))
        .route("/auth/login", post(crate::api::handlers::auth::login))
        .route(
            "/auth/refresh",
            post(crate::api::handlers::auth::refresh_token),
        )
        .route("/auth/logout", post(crate::api::handlers::auth::logout))
        .route("/agents", get(crate::api::handlers::agents::list_agents));

    #[allow(unused_mut)]
    let mut protected_routes = Router::new()
        // Protected routes (auth required)
        .route("/chat", post(crate::api::handlers::chat::chat))
        .route(
            "/chat/stream",
            post(crate::api::handlers::chat::chat_stream),
        )
        .route(
            "/research",
            post(crate::api::handlers::research::deep_research),
        )
        .route("/memory", get(crate::api::handlers::chat::get_user_memory))
        // Workflow routes
        .route(
            "/workflows",
            get(crate::api::handlers::workflows::list_workflows),
        )
        .route(
            "/workflows/{workflow_name}",
            post(crate::api::handlers::workflows::execute_workflow),
        )
        // User agent routes
        .route(
            "/user/agents",
            get(crate::api::handlers::user_agents::list_agents)
                .post(crate::api::handlers::user_agents::create_agent),
        )
        .route(
            "/user/agents/import",
            post(crate::api::handlers::user_agents::import_agent_toon),
        )
        .route(
            "/user/agents/{name}",
            get(crate::api::handlers::user_agents::get_agent)
                .put(crate::api::handlers::user_agents::update_agent)
                .delete(crate::api::handlers::user_agents::delete_agent),
        )
        .route(
            "/user/agents/{name}/export",
            get(crate::api::handlers::user_agents::export_agent_toon),
        )
        // Loop-mode agent routes
        .route("/loops/start", post(loops::start_loop))
        .route("/loops", get(loops::list_loops))
        .route("/loops/{id}", delete(loops::stop_loop))
        // Conversation routes
        .route(
            "/conversations",
            get(crate::api::handlers::conversations::list_conversations),
        )
        .route(
            "/conversations/{id}",
            get(crate::api::handlers::conversations::get_conversation)
                .put(crate::api::handlers::conversations::update_conversation)
                .delete(crate::api::handlers::conversations::delete_conversation),
        );

    // Skills routes (requires skills feature)
    #[cfg(feature = "skills")]
    {
        protected_routes = protected_routes
            .route("/skills", get(crate::api::handlers::skills::list_skills))
            .route(
                "/skills/{name}",
                get(crate::api::handlers::skills::get_skill),
            );
    }

    // RAG routes (requires local-embeddings feature for ONNX-based embeddings and ares-vector for vector storage)
    #[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
    {
        protected_routes = protected_routes
            .route("/rag/ingest", post(crate::api::handlers::rag::ingest))
            .route("/rag/search", post(crate::api::handlers::rag::search))
            .route(
                "/rag/collection",
                delete(crate::api::handlers::rag::delete_collection),
            )
            .route(
                "/rag/collections",
                get(crate::api::handlers::rag::list_collections),
            );
    }

    // Layer order: last added = outermost = runs first.
    // Request flow: jwt_auth → inject_tenant_db → track_usage → handler → track_usage (reads response)
    let protected_routes = protected_routes
        // Innermost: wraps handler, reads tenant info from extensions, records token usage from response headers
        .layer(middleware::from_fn(crate::middleware::usage::track_usage))
        // Middle: injects Arc<TenantDb> into extensions so track_usage and api_key_auth can read it
        .layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let db = tenant_db.clone();
            async move {
                req.extensions_mut().insert(db);
                next.run(req).await
            }
        }))
        // Outermost: validates JWT, rejects unauthorized requests early
        .layer(middleware::from_fn(move |req, next| {
            crate::auth::middleware::auth_middleware(auth_service.clone(), req, next)
        }));

    // Admin routes (protected by X-Admin-Secret header)
    let admin_routes = Router::new()
        .route(
            "/admin/tenants",
            post(crate::api::handlers::admin::create_tenant)
                .get(crate::api::handlers::admin::list_tenants),
        )
        .route(
            "/admin/tenants/{tenant_id}",
            get(crate::api::handlers::admin::get_tenant),
        )
        .route(
            "/admin/tenants/{tenant_id}/api-keys",
            post(crate::api::handlers::admin::create_api_key)
                .get(crate::api::handlers::admin::list_api_keys),
        )
        .route(
            "/admin/tenants/{tenant_id}/usage",
            get(crate::api::handlers::admin::get_tenant_usage),
        )
        .route(
            "/admin/tenants/{tenant_id}/quota",
            put(crate::api::handlers::admin::update_tenant_quota),
        )
        // Provisioning
        .route(
            "/admin/provision-client",
            post(crate::api::handlers::admin::provision_client),
        )
        // Tenant agents CRUD
        .route(
            "/admin/tenants/{tenant_id}/agents",
            get(crate::api::handlers::admin::list_tenant_agents_handler)
                .post(crate::api::handlers::admin::create_tenant_agent_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/versions",
            get(crate::api::handlers::admin::list_tenant_agent_versions_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/rollback/{version}",
            post(crate::api::handlers::admin::rollback_tenant_agent_version_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/test",
            post(crate::api::handlers::admin::test_tenant_agent_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}",
            put(crate::api::handlers::admin::update_tenant_agent_handler)
                .delete(crate::api::handlers::admin::delete_tenant_agent_handler),
        )
        // Templates and models
        .route(
            "/admin/agent-templates",
            get(crate::api::handlers::admin::list_agent_templates_handler),
        )
        .route(
            "/admin/models",
            get(crate::api::handlers::admin::list_models_handler),
        )
        // Alerts
        .route(
            "/admin/alerts",
            get(crate::api::handlers::admin::list_alerts),
        )
        .route(
            "/admin/alerts/{alert_id}/resolve",
            post(crate::api::handlers::admin::resolve_alert),
        )
        // Audit log
        .route(
            "/admin/audit-log",
            get(crate::api::handlers::admin::list_audit_log),
        )
        // Daily usage per tenant
        .route(
            "/admin/tenants/{tenant_id}/usage/daily",
            get(crate::api::handlers::admin::get_daily_usage),
        )
        // Agent runs per tenant+agent
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/runs",
            get(crate::api::handlers::admin::list_agent_runs_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/feedback/summary",
            get(crate::api::handlers::admin::get_agent_feedback_summary_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/runs/{run_id}/feedback",
            post(crate::api::handlers::admin::create_agent_run_feedback_handler),
        )
        .route(
            "/admin/tenants/{tenant_id}/agents/{agent_name}/stats",
            get(crate::api::handlers::admin::get_agent_stats_handler),
        )
        // Cross-tenant agent list
        .route(
            "/admin/agents",
            get(crate::api::handlers::admin::list_all_agents_handler),
        )
        // Platform stats
        .route(
            "/admin/stats",
            get(crate::api::handlers::admin::get_platform_stats),
        )
        // Agent versioning (Sprint 12): version history, rollback, kill switch
        .route(
            "/admin/agents/{agent_id}/versions",
            get(crate::api::handlers::admin::list_agent_versions_handler),
        )
        .route(
            "/admin/agents/{agent_id}/rollback/{version}",
            post(crate::api::handlers::admin::rollback_agent_handler),
        )
        .route(
            "/admin/agents/emergency-stop",
            post(crate::api::handlers::admin::emergency_stop_handler),
        )
        // Deployment automation
        .route("/admin/deploy", post(deploy::trigger_deploy))
        .route("/admin/deploy/{deploy_id}", get(deploy::get_deploy_status))
        .route("/admin/deploys", get(deploy::list_deploys))
        .route("/admin/services", get(deploy::get_services_health))
        .route(
            "/admin/services/{service_name}/logs",
            get(deploy::get_service_logs),
        )
        .layer(middleware::from_fn(
            crate::api::handlers::admin::admin_middleware,
        ));

    // External API: authenticated via API key (for client apps, CLI, MCP)
    // Client-specific business logic lives in the client's own portal backend, not here.
    // ARES provides generic agent execution — clients call /v1/chat with their API key.
    #[allow(unused_mut)]
    let v1_metered_routes = Router::new()
        .route("/chat", post(crate::api::handlers::v1::v1_chat))
        .route("/research", post(crate::api::handlers::v1::v1_research))
        .route(
            "/agents/{name}/run",
            post(crate::api::handlers::v1::run_agent),
        )
        .layer(middleware::from_fn(crate::middleware::usage::track_usage));

    let v1_routes = Router::new()
        .merge(v1_metered_routes)
        .route("/agents", get(crate::api::handlers::v1::list_agents))
        .route("/agents/{name}", get(crate::api::handlers::v1::get_agent))
        .route(
            "/agents/{name}/runs",
            get(crate::api::handlers::v1::list_agent_runs),
        )
        .route(
            "/agents/{name}/logs",
            get(crate::api::handlers::v1::list_agent_logs),
        )
        .route("/usage", get(crate::api::handlers::v1::get_usage))
        .route(
            "/api-keys",
            get(crate::api::handlers::v1::list_api_keys)
                .post(crate::api::handlers::v1::create_api_key),
        )
        .route(
            "/api-keys/{id}",
            delete(crate::api::handlers::v1::revoke_api_key),
        )
        .route(
            "/tenant/data",
            delete(crate::api::handlers::v1::delete_tenant_data),
        );

    // Semantic search (requires local-embeddings and ares-vector features)
    #[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
    {
        v1_routes = v1_routes.route(
            "/search/semantic",
            post(crate::api::handlers::v1::semantic_search),
        );
    }

    // Eruka context middleware — only when eruka-context feature is enabled
    #[cfg(feature = "eruka-context")]
    let v1_routes = v1_routes.layer(middleware::from_fn(
        crate::middleware::eruka_context::eruka_context_middleware,
    ));
    let v1_routes = v1_routes
        .layer(middleware::from_fn(
            crate::middleware::api_key_auth::api_key_auth_middleware,
        ))
        .layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let db = tenant_db_for_v1.clone();
            async move {
                req.extensions_mut().insert(db);
                next.run(req).await
            }
        }));

    public_routes
        .merge(protected_routes)
        .merge(admin_routes)
        .nest("/v1", v1_routes)
}



/// Joins a route prefix and suffix into a single path (for nested routers).
pub(crate) fn join_route_paths(prefix: &str, suffix: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let suffix = if suffix.starts_with('/') {
        suffix.to_string()
    } else {
        format!("/{suffix}")
    };
    format!("{prefix}{suffix}")
}

/// Public routes that do not require JWT authentication.
pub(crate) fn public_route_paths() -> &'static [&'static str] {
    &[
        "/auth/register",
        "/auth/login",
        "/auth/refresh",
        "/auth/logout",
        "/agents",
    ]
}

/// Protected routes that require JWT authentication (non-v1).
pub(crate) fn protected_route_paths() -> &'static [&'static str] {
    &[
        "/chat",
        "/chat/stream",
        "/research",
        "/memory",
        "/workflows",
        "/conversations",
        "/loops/start",
    ]
}

#[cfg(test)]
mod route_path_tests {
    use super::*;

    #[test]
    fn join_route_paths_nests_v1_chat() {
        assert_eq!(join_route_paths("/v1", "/chat"), "/v1/chat");
    }

    #[test]
    fn join_route_paths_adds_leading_slash_when_missing() {
        assert_eq!(join_route_paths("/api", "health"), "/api/health");
    }

    #[test]
    fn public_route_paths_include_auth_login() {
        assert!(public_route_paths().contains(&"/auth/login"));
    }

    #[test]
    fn protected_route_paths_include_research() {
        assert!(protected_route_paths().contains(&"/research"));
    }

    #[test]
    fn public_and_protected_paths_do_not_overlap() {
        for path in public_route_paths() {
            assert!(!protected_route_paths().contains(path));
        }
    }

    #[test]
    fn join_route_paths_strips_trailing_slash_on_prefix() {
        assert_eq!(join_route_paths("/v1/", "agents"), "/v1/agents");
    }

    #[test]
    fn join_route_paths_preserves_suffix_with_leading_slash() {
        assert_eq!(join_route_paths("/api", "/v1/chat"), "/api/v1/chat");
    }

    #[test]
    fn protected_route_paths_include_conversations_and_chat() {
        let paths = protected_route_paths();
        assert!(paths.contains(&"/conversations"));
        assert!(paths.contains(&"/chat"));
        assert!(paths.contains(&"/chat/stream"));
    }

    #[test]
    fn public_route_paths_include_refresh_and_logout() {
        let paths = public_route_paths();
        assert!(paths.contains(&"/auth/refresh"));
        assert!(paths.contains(&"/auth/logout"));
    }
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;
    use crate::agents::context_provider::NoOpContextProvider;
    use crate::utils::toml_config::{
        AgentConfig, AresConfig, AuthConfig, BillingConfig, DatabaseConfig,
        DynamicConfigPaths, ModelConfig, ProviderConfig, RagConfig, ServerConfig,
    };
    use crate::{
        AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory,
        DynamicConfigManager, ProviderRegistry, ToolRegistry,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    fn minimal_config() -> AresConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "p".into(),
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".into(),
                api_base: "https://test.example.com/v1".into(),
                default_model: "m".into(),
            },
        );
        let mut models = HashMap::new();
        models.insert(
            "default".into(),
            ModelConfig {
                provider: "p".into(),
                model: "m".into(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );
        let mut agents = HashMap::new();
        agents.insert(
            "a".into(),
            AgentConfig {
                model: "default".into(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig {
                jwt_secret_env: "JWT_SECRET".into(),
                jwt_access_expiry: 900,
                jwt_refresh_expiry: 604800,
                api_key_env: "API_KEY".into(),
            },
            database: DatabaseConfig::default(),
            nvidia: None,
            config: DynamicConfigPaths::default(),
            providers,
            models,
            tools: HashMap::new(),
            agents,
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            #[cfg(feature = "skills")]
            skills: None,
        }
    }

    fn test_app_state() -> AppState {
        let config = minimal_config();
        let config_manager = Arc::new(AresConfigManager::from_config(config));
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config_manager.config()));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config_manager.config(),
            provider_registry.clone(),
            tool_registry.clone(),
        ));
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let base = temp_dir.path();
        for sub in ["agents", "models", "tools", "workflows", "mcps"] {
            std::fs::create_dir_all(base.join(sub)).expect("mkdir");
        }
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
        std::mem::forget(temp_dir);

        let db = Arc::new(crate::db::PostgresClient::new_test());
        AppState {
            config_manager,
            dynamic_config,
            db: db.clone(),
            tenant_db: Arc::new(TenantDb::new(db)),
            llm_factory: Arc::new(ConfigBasedLLMFactory::new(
                provider_registry.clone(),
                "default",
            )),
            provider_registry,
            agent_registry,
            tool_registry,
            auth_service: Arc::new(AuthService::new(
                "test-secret-at-least-32-characters-long".into(),
                900,
                604800,
            )),
            deploy_registry: deploy::new_deploy_registry(),
            loop_registry: loops::LoopRegistry::new(),
            emergency_stop: Arc::new(AtomicBool::new(false)),
            context_provider: Arc::new(NoOpContextProvider),
            #[cfg(feature = "mcp")]
            mcp_registry: None,
        }
    }

    fn test_server(state: AppState) -> axum_test::TestServer {
        let auth = state.auth_service.clone();
        let tenant_db = state.tenant_db.clone();
        let app = create_router(auth, tenant_db).with_state(state);
        axum_test::TestServer::new(app).expect("test server")
    }

    fn public_api_paths() -> &'static [&'static str] {
        &[
            "/auth/register",
            "/auth/login",
            "/auth/refresh",
            "/auth/logout",
            "/agents",
        ]
    }

    #[test]
    fn public_api_paths_include_auth_and_agents() {
        let paths = public_api_paths();
        assert!(paths.contains(&"/auth/register"));
        assert!(paths.contains(&"/auth/login"));
        assert!(paths.contains(&"/agents"));
    }

    #[test]
    fn public_api_paths_exclude_protected_resources() {
        let paths = public_api_paths();
        assert!(!paths.iter().any(|p| p.contains("chat")));
        assert!(!paths.iter().any(|p| p.contains("conversations")));
    }

    #[tokio::test]
    async fn create_router_builds_without_panic() {
        let state = test_app_state();
        let _ = create_router(state.auth_service.clone(), state.tenant_db.clone());
    }

    #[test]
    fn route_contract_does_not_depend_on_env() {
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("JWT_SECRET");
        assert_eq!(public_api_paths().len(), 5);
    }

    #[tokio::test]
    async fn create_router_exposes_public_agents_list() {
        let server = test_server(test_app_state());
        let response = server.get("/agents").await;
        response.assert_status_ok();
    }

    #[tokio::test]
    async fn create_router_protects_chat_without_jwt() {
        let server = test_server(test_app_state());
        let response = server
            .post("/chat")
            .json(&serde_json::json!({"message": "hi"}))
            .await;
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_exposes_loop_routes_behind_jwt() {
        let state = test_app_state();
        let tokens = state
            .auth_service
            .generate_tokens("user-1", "user@example.com")
            .expect("tokens");
        let server = test_server(state);
        let response = server
            .get("/loops")
            .add_header("authorization", format!("Bearer {}", tokens.access_token))
            .await;
        response.assert_status_ok();
    }

    #[tokio::test]
    async fn create_router_admin_deploys_rejects_missing_secret() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        server.get("/admin/deploys").await.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_nests_v1_agents_behind_api_key_auth() {
        let server = test_server(test_app_state());
        let response = server.get("/v1/agents").await;
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_deploy_post_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .post("/admin/deploy")
            .json(&serde_json::json!({"target": "not-valid"}))
            .await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }
}
