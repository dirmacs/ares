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
        .layer(middleware::from_fn(
            crate::api::handlers::admin::admin_middleware,
        ));

    // External API: authenticated via API key (for client apps, CLI, MCP)
    // Client-specific business logic lives in the client's own portal backend, not here.
    // ARES provides generic agent execution — clients call /v1/chat with their API key.
    let v1_routes = Router::new()
        .route("/chat", post(crate::api::handlers::chat::chat))
        .route("/chat/stream", post(crate::api::handlers::chat::chat_stream))
        .route("/agents", get(crate::api::handlers::v1::list_agents))
        .route("/agents/{name}", get(crate::api::handlers::v1::get_agent))
        .route("/agents/{name}/run", post(crate::api::handlers::v1::run_agent))
        .route("/agents/{name}/runs", get(crate::api::handlers::v1::list_agent_runs))
        .route("/agents/{name}/logs", get(crate::api::handlers::v1::list_agent_logs))
        .route("/usage", get(crate::api::handlers::v1::get_usage))
        .route("/api-keys", get(crate::api::handlers::v1::list_api_keys).post(crate::api::handlers::v1::create_api_key))
        .route("/api-keys/{id}", delete(crate::api::handlers::v1::revoke_api_key))
        .layer(middleware::from_fn(crate::middleware::api_key_auth::api_key_auth_middleware))
        .layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let db = tenant_db_for_v1.clone();
            async move {
                req.extensions_mut().insert(db);
                next.run(req).await
            }
        }));

    public_routes.merge(protected_routes).merge(admin_routes).nest("/v1", v1_routes)
}
