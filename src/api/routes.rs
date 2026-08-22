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
use ares_cordis_core::Context;

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
        .route("/agents", get(crate::api::handlers::agents::list_agents))
        // Public webhook receiver (outside admin middleware)
        .route(
            "/webhooks/{trigger_id}",
            post(crate::api::handlers::admin::receive_webhook),
        )
        .route(
            "/events/document-upload",
            post(crate::api::handlers::document_upload::handle_document_upload),
        )
        .route(
            "/events/field-change",
            post(crate::api::handlers::field_change::handle_field_change),
        )
        .route(
            "/oauth/authorize",
            get(crate::api::handlers::admin::oauth_authorize),
        )
        .route(
            "/oauth/callback",
            get(crate::api::handlers::admin::oauth_callback),
        );

    #[allow(unused_mut)]
    let mut protected_routes = Router::new()
        // Protected routes (auth required)
        .route("/chat", post(crate::api::handlers::chat::chat))
        .route(
            "/chat/stream",
            post(crate::api::handlers::chat::chat_stream)
                .get(crate::api::handlers::chat::chat_stream_get),
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
    // Phase 6 §21: route registration gated — handler types require feature deps to compile
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
            get(crate::api::handlers::admin::list_agent_templates_handler)
                .post(crate::api::handlers::admin::create_agent_template_handler),
        )
        .route(
            "/admin/agent-templates/{id}",
            delete(crate::api::handlers::admin::delete_agent_template_handler),
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
        // Cross-tenant agent CRUD
        .route(
            "/admin/agents",
            get(crate::api::handlers::admin::list_agents)
                .post(crate::api::handlers::admin::create_agent),
        )
        .route(
            "/admin/agents/{tenant_id}/{agent_name}",
            get(crate::api::handlers::admin::get_agent)
                .put(crate::api::handlers::admin::update_agent)
                .delete(crate::api::handlers::admin::delete_agent),
        )
        .route(
            "/admin/agents/{tenant_id}/{agent_name}/versions",
            get(crate::api::handlers::admin::get_agent_versions),
        )
        .route(
            "/admin/agents/{tenant_id}/{agent_name}/rollback/{version}",
            post(crate::api::handlers::admin::rollback_agent),
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
            get(crate::api::handlers::admin::get_emergency_stop_handler)
                .post(crate::api::handlers::admin::emergency_stop_handler),
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
        // Tenant Model Tiers — per-tenant abstract tier -> concrete provider/model
        .route(
            "/admin/tenants/{tenant_id}/model-tiers",
            get(crate::api::handlers::admin::list_tenant_model_tiers),
        )
        .route(
            "/admin/tenants/{tenant_id}/model-tiers/{tier_name}",
            get(crate::api::handlers::admin::get_tenant_model_tier)
                .put(crate::api::handlers::admin::set_tenant_model_tier)
                .delete(crate::api::handlers::admin::delete_tenant_model_tier),
        )
        // Tenant Allowlists
        .route(
            "/admin/tenants/{tenant_id}/allowed-tools",
            get(crate::api::handlers::admin::list_tenant_allowed_tools)
                .post(crate::api::handlers::admin::add_tenant_allowed_tool),
        )
        .route(
            "/admin/tenants/{tenant_id}/allowed-tools/{tool_name}",
            delete(crate::api::handlers::admin::delete_tenant_allowed_tool),
        )
        .route(
            "/admin/tenants/{tenant_id}/allowed-models",
            get(crate::api::handlers::admin::list_tenant_allowed_models)
                .post(crate::api::handlers::admin::add_tenant_allowed_model),
        )
        .route(
            "/admin/tenants/{tenant_id}/allowed-models/{model_id}",
            delete(crate::api::handlers::admin::delete_tenant_allowed_model),
        )
        .route(
            "/admin/tenants/{tenant_id}/allowed-rag-sources",
            get(crate::api::handlers::admin::list_tenant_allowed_rag_sources)
                .post(crate::api::handlers::admin::add_tenant_allowed_rag_source),
        )
        .route(
            "/admin/tenants/{tenant_id}/allowed-rag-sources/{rag_source}",
            delete(crate::api::handlers::admin::delete_tenant_allowed_rag_source),
        )
        .route(
            "/admin/tenants/{tenant_id}/triggers",
            get(crate::api::handlers::admin::list_tenant_triggers)
                .post(crate::api::handlers::admin::create_tenant_trigger),
        )
        .route(
            "/admin/tenants/{tenant_id}/triggers/{id}",
            put(crate::api::handlers::admin::update_tenant_trigger)
                .delete(crate::api::handlers::admin::delete_tenant_trigger),
        )
        .route(
            "/admin/tenants/{tenant_id}/pipelines",
            get(crate::api::handlers::admin::list_tenant_pipelines)
                .post(crate::api::handlers::admin::create_tenant_pipeline),
        )
        .route(
            "/admin/tenants/{tenant_id}/pipelines/{id}",
            put(crate::api::handlers::admin::update_tenant_pipeline)
                .delete(crate::api::handlers::admin::delete_tenant_pipeline),
        )
        // Fleet Provider Secrets — encrypted at rest, hot-swap in memory
        .route(
            "/admin/fleet-providers",
            get(crate::api::handlers::admin::list_fleet_providers),
        )
        .route(
            "/admin/fleet-providers/capabilities",
            get(crate::api::handlers::admin::fleet_provider_capabilities),
        )
        .route(
            "/admin/fleet-providers/{provider_name}",
            put(crate::api::handlers::admin::upsert_fleet_provider)
                .delete(crate::api::handlers::admin::delete_fleet_provider),
        )
        .route(
            "/admin/fleet-providers/{provider_name}/verify",
            post(crate::api::handlers::admin::verify_fleet_provider),
        )
        // Runtime Tools — CRUD + versions + rollback + test
        .route(
            "/admin/runtime-tools",
            get(crate::api::handlers::admin::list_runtime_tools)
                .post(crate::api::handlers::admin::create_runtime_tool),
        )
        .route(
            "/admin/runtime-tools/capabilities",
            get(crate::api::handlers::admin::runtime_tool_capabilities),
        )
        .route(
            "/admin/runtime-tools/{id}",
            get(crate::api::handlers::admin::get_runtime_tool)
                .put(crate::api::handlers::admin::update_runtime_tool)
                .delete(crate::api::handlers::admin::delete_runtime_tool),
        )
        .route(
            "/admin/runtime-tools/{id}/test",
            post(crate::api::handlers::admin::test_runtime_tool),
        )
        .route(
            "/admin/runtime-tools/{id}/versions",
            get(crate::api::handlers::admin::list_runtime_tool_versions),
        )
        .route(
            "/admin/runtime-tools/{id}/rollback/{version}",
            post(crate::api::handlers::admin::rollback_runtime_tool),
        )
        // Cordis service lifecycle (retire / re-provide)
        .route(
            "/admin/cordis/services/{name}/retire",
            post(crate::api::handlers::admin::retire_cordis_service),
        )
        .route(
            "/admin/cordis/services/{name}/provide",
            post(crate::api::handlers::admin::provide_cordis_service),
        )
        // Runtime Providers
        .route(
            "/admin/runtime_providers",
            get(crate::api::handlers::admin::list_runtime_providers)
                .post(crate::api::handlers::admin::upsert_runtime_provider),
        )
        .route(
            "/admin/runtime_providers/{name}",
            get(crate::api::handlers::admin::get_runtime_provider)
                .delete(crate::api::handlers::admin::delete_runtime_provider),
        )
        // Run History
        .route(
            "/admin/run-history/llm-calls",
            get(crate::api::handlers::admin::list_llm_calls)
                .post(crate::api::handlers::admin::insert_llm_call),
        )
        .route(
            "/admin/run-history/llm-calls/{id}",
            get(crate::api::handlers::admin::get_llm_call),
        )
        .route(
            "/admin/run-history/tool-calls",
            get(crate::api::handlers::admin::list_tool_calls)
                .post(crate::api::handlers::admin::insert_tool_call),
        )
        .route(
            "/admin/run-history/tool-calls/{id}",
            get(crate::api::handlers::admin::get_tool_call),
        )
        .route(
            "/admin/run-history/costs/{run_id}",
            get(crate::api::handlers::admin::get_run_cost),
        )
        .route(
            "/admin/run-history/costs",
            get(crate::api::handlers::admin::list_run_costs),
        )
        .route(
            "/admin/tenants/{tenant_id}/billing/summary",
            get(crate::api::handlers::admin::get_tenant_billing_summary),
        )
        .route(
            "/admin/tenants/{tenant_id}/billing/line-items",
            get(crate::api::handlers::admin::get_tenant_billing_line_items),
        )
        .route(
            "/admin/billing/model-rates",
            get(crate::api::handlers::admin::list_billing_model_rates),
        )
        .route(
            "/admin/billing/unit-rates",
            get(crate::api::handlers::admin::list_billing_unit_rates),
        )
        .route(
            "/admin/run-history/budgets/{tenant_id}",
            get(crate::api::handlers::admin::get_tenant_budget)
                .put(crate::api::handlers::admin::set_tenant_budget)
                .delete(crate::api::handlers::admin::delete_tenant_budget),
        )
        .route(
            "/admin/token-budgets/{tenant_id}",
            get(crate::api::handlers::admin::get_token_budget)
                .put(crate::api::handlers::admin::set_token_budget),
        )
        .route(
            "/admin/token-budgets/{tenant_id}/status",
            get(crate::api::handlers::admin::get_token_budget_status),
        )
        .route(
            "/admin/token-budgets/{tenant_id}/reset",
            post(crate::api::handlers::admin::reset_token_budget_period),
        )
        .route(
            "/admin/token-budgets/{tenant_id}/usage",
            get(crate::api::handlers::admin::list_token_usage),
        )
        .route(
            "/admin/run-history/alerts",
            get(crate::api::handlers::admin::list_budget_alerts),
        )
        .route(
            "/admin/run-history/alerts/{id}/acknowledge",
            post(crate::api::handlers::admin::acknowledge_budget_alert),
        )
        .route(
            "/admin/run-history/health-metrics",
            get(crate::api::handlers::admin::list_health_metrics)
                .post(crate::api::handlers::admin::insert_health_metrics),
        )
        .route(
            "/admin/run-history/model-metrics",
            get(crate::api::handlers::admin::list_model_metrics),
        )
        // Skills & Connectors
        .route(
            "/admin/runs/live",
            get(crate::api::handlers::admin::stream_active_runs),
        )
        .route(
            "/admin/skills",
            get(crate::api::handlers::admin::list_skills)
                .post(crate::api::handlers::admin::create_skill),
        )
        .route(
            "/admin/skills/run",
            post(crate::api::handlers::admin::run_skill),
        )
        .route(
            "/admin/skills/{id}",
            get(crate::api::handlers::admin::get_skill)
                .put(crate::api::handlers::admin::update_skill)
                .delete(crate::api::handlers::admin::delete_skill),
        )
        .route(
            "/admin/connectors",
            get(crate::api::handlers::admin::list_connectors)
                .post(crate::api::handlers::admin::create_connector),
        )
        .route(
            "/admin/connectors/{id}",
            put(crate::api::handlers::admin::update_connector)
                .delete(crate::api::handlers::admin::delete_connector),
        )
        .route(
            "/admin/tenants/{tenant_id}/connectors/{id}",
            delete(crate::api::handlers::admin::delete_tenant_connector),
        )
        .route(
            "/admin/tenants/{tenant_id}/oauth-creds",
            get(crate::api::handlers::admin::list_oauth_credentials)
                .post(crate::api::handlers::admin::create_oauth_credential),
        )
        .route(
            "/admin/tenants/{tenant_id}/oauth-creds/{id}",
            delete(crate::api::handlers::admin::delete_oauth_credential),
        )
        // Schedules, Triggers & Pipelines
        .route(
            "/admin/schedules",
            get(crate::api::handlers::admin::list_schedules)
                .post(crate::api::handlers::admin::create_schedule),
        )
        .route(
            "/admin/schedules/{id}",
            put(crate::api::handlers::admin::update_schedule)
                .delete(crate::api::handlers::admin::delete_schedule),
        )
        .route(
            "/admin/tenants/{tenant_id}/schedules/{id}",
            put(crate::api::handlers::admin::update_tenant_schedule)
                .delete(crate::api::handlers::admin::delete_tenant_schedule),
        )
        .route(
            "/admin/tenants/{tenant_id}/schedules/{id}/missed-runs",
            get(crate::api::handlers::admin::list_schedule_missed_runs),
        )
        .route(
            "/admin/triggers",
            get(crate::api::handlers::admin::list_triggers)
                .post(crate::api::handlers::admin::create_trigger),
        )
        .route(
            "/admin/triggers/{id}",
            delete(crate::api::handlers::admin::delete_trigger),
        )
        .route(
            "/admin/pipelines",
            get(crate::api::handlers::admin::list_pipelines)
                .post(crate::api::handlers::admin::create_pipeline),
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
            "/agents/{name}/sandbox-run",
            post(crate::api::handlers::v1::sandbox_run_agent),
        )
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
    let v1_routes = v1_routes.route(
        "/search/semantic",
        post(crate::api::handlers::v1::semantic_search),
    );

    // Eruka context middleware — only when eruka-context feature is enabled
    // Phase 6 §21: route registration gated — handler types require feature deps to compile
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

// cordis Phase6: Context-driven router that merges RouteSets via Service discovery.
// Keep `create_router` as shim for one commit; new code should call `build_routes`.
/// Build router from a Cordis `Context` by merging `RouteSet`s discovered via `ctx.get::<...>()`.
///
/// Each admin domain (`admin::tenants`, `admin::agents`, …) and each `v1::{chat,stream,agents}`
/// exposes `pub fn routes() -> Router`. In the final cutover this function will
/// `ctx.get::<AdminTenantsService>()` etc. and merge their routers.
///
/// Mirrors `create_router` admin + v1 route sets but via `ctx` (future: `ctx.get::<...>().check()`).
// Phase 6 §21: route registration gated — handler types require feature deps to compile
#[cfg(feature = "postgres")]
pub fn build_routes(ctx: &Arc<Context>) -> Router<AppState> {
    let _ = ctx;
    Router::new()
        .merge(crate::api::handlers::admin::tenants::routes())
        .merge(crate::api::handlers::admin::agents::routes())
        .merge(crate::api::handlers::admin::providers::routes())
        .merge(crate::api::handlers::admin::tools::routes())
        .merge(crate::api::handlers::admin::schedules::routes())
        .merge(crate::api::handlers::admin::triggers::routes())
        .merge(crate::api::handlers::admin::pipelines::routes())
        .merge(crate::api::handlers::admin::billing::routes())
        .merge(crate::api::handlers::admin::mcp::routes())
        .merge(crate::api::handlers::admin::fleet_secrets::routes())
        .merge(crate::api::handlers::admin::connectors::routes())
        .merge(crate::api::handlers::admin::health::routes())
        .merge(crate::api::handlers::admin::audit::routes())
        // Cordis service lifecycle (retire / re-provide)
        .route(
            "/admin/cordis/services/{name}/retire",
            post(crate::api::handlers::admin::retire_cordis_service),
        )
        .route(
            "/admin/cordis/services/{name}/provide",
            post(crate::api::handlers::admin::provide_cordis_service),
        )
        .merge(crate::api::handlers::v1::chat::routes())
        .merge(crate::api::handlers::v1::stream::routes())
        .merge(crate::api::handlers::v1::agents::routes())
}

// Phase 6 §21: route registration gated — handler types require feature deps to compile
#[cfg(not(feature = "postgres"))]
pub fn build_routes(ctx: &Arc<Context>) -> Router<Arc<Context>> {
    let _ = ctx;
    Router::new()
        .merge(crate::api::handlers::admin::tenants::routes())
        .merge(crate::api::handlers::admin::agents::routes())
        .merge(crate::api::handlers::admin::providers::routes())
        .merge(crate::api::handlers::admin::tools::routes())
        .merge(crate::api::handlers::admin::schedules::routes())
        .merge(crate::api::handlers::admin::triggers::routes())
        .merge(crate::api::handlers::admin::pipelines::routes())
        .merge(crate::api::handlers::admin::billing::routes())
        .merge(crate::api::handlers::admin::mcp::routes())
        .merge(crate::api::handlers::admin::fleet_secrets::routes())
        .merge(crate::api::handlers::admin::connectors::routes())
        .merge(crate::api::handlers::admin::health::routes())
        .merge(crate::api::handlers::admin::audit::routes())
        // Cordis service lifecycle (retire / re-provide)
        .route(
            "/admin/cordis/services/{name}/retire",
            post(crate::api::handlers::admin::retire_cordis_service),
        )
        .route(
            "/admin/cordis/services/{name}/provide",
            post(crate::api::handlers::admin::provide_cordis_service),
        )
        .merge(crate::api::handlers::v1::chat::routes())
        .merge(crate::api::handlers::v1::stream::routes())
        .merge(crate::api::handlers::v1::agents::routes())
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
        "/oauth/authorize",
        "/oauth/callback",
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
        AgentConfig, AresConfig, AuthConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths,
        ModelConfig, ProviderConfig, RagConfig, ServerConfig,
    };
    use crate::{
        AgentRegistry, AppState, AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager,
        ProviderRegistry, ToolRegistry,
    };
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

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
                allowed_tools: None,
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
            skills: None,
        }
    }

    fn test_app_state() -> AppState {
        let ctx = ares_cordis_core::Context::new_root();
        let config = minimal_config();
        let config_manager = Arc::new(AresConfigManager::from_config(config));
        ctx.provide(crate::context_services::ConfigManagerService(config_manager.clone()));
        let db = Arc::new(crate::db::PostgresClient::new_test());
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        ctx.provide(crate::context_services::TenantDbService(tenant_db.clone()));
        ctx.provide(crate::context_services::DbService(db.clone() as Arc<dyn crate::db::traits::DatabaseClient>));
        let auth_service = Arc::new(AuthService::new(
            "test-secret-at-least-32-characters-long".into(),
            900,
            604800,
        ));
        ctx.provide(crate::context_services::AuthServiceWrapper(auth_service));
        ctx.provide(crate::context_services::DeployRegistryService(deploy::new_deploy_registry()));
        ctx.provide(crate::context_services::LoopRegistryService(loops::LoopRegistry::new()));
        ctx
    }

    fn test_server(state: AppState) -> axum_test::TestServer {
        let auth = state.get::<crate::context_services::AuthServiceWrapper>().expect("not provided").0.clone();
        let tenant_db = state.get::<crate::context_services::TenantDbService>().expect("not provided").0.clone();
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
            "/oauth/authorize",
            "/oauth/callback",
        ]
    }

    #[test]
    fn public_api_paths_include_auth_agents_and_oauth() {
        let paths = public_api_paths();
        assert!(paths.contains(&"/auth/register"));
        assert!(paths.contains(&"/auth/login"));
        assert!(paths.contains(&"/agents"));
        assert!(paths.contains(&"/oauth/authorize"));
        assert!(paths.contains(&"/oauth/callback"));
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
        let _ = create_router(state.get::<crate::context_services::AuthServiceWrapper>().expect("not provided").0.clone(), state.get::<crate::context_services::TenantDbService>().expect("not provided").0.clone());
    }

    #[test]
    fn route_contract_does_not_depend_on_env() {
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("JWT_SECRET");
        assert_eq!(public_api_paths().len(), 7);
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
        let tokens = state.get::<crate::context_services::AuthServiceWrapper>().expect("not provided").0
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
        server
            .get("/admin/deploys")
            .await
            .assert_status_unauthorized();
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

    #[tokio::test]
    async fn create_router_registers_run_history_llm_calls_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/run-history/llm-calls").await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_run_history_budget_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/run-history/budgets/tenant-1").await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_schedule_update_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .put("/admin/schedules/schedule-1")
            .json(&serde_json::json!({
                "tenant_id": "tenant-1",
                "agent_name": "agent-a",
                "cron_expression": "0 0/5 * * * * *",
                "timezone": "UTC",
                "enabled": true,
                "grace_period_seconds": 120
            }))
            .await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_tenant_pipeline_update_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .put("/admin/tenants/tenant-1/pipelines/pipeline-1")
            .json(&serde_json::json!({
                "tenant_id": "ignored-client-tenant",
                "source_agent": "agent-a",
                "target_agent": "agent-b",
                "condition": null,
                "enabled": true
            }))
            .await;
        assert_ne!(
            response.status_code(),
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn create_router_registers_tenant_trigger_update_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .put("/admin/tenants/tenant-1/triggers/trigger-1")
            .json(&serde_json::json!({
                "tenant_id": "ignored-client-tenant",
                "name": "Webhook",
                "event_type": "webhook",
                "event_config": {},
                "target_agent": "agent-a",
                "enabled": true
            }))
            .await;
        assert_ne!(
            response.status_code(),
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn create_router_registers_tenant_schedule_routes() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let update_response = server
            .put("/admin/tenants/tenant-1/schedules/schedule-1")
            .json(&serde_json::json!({
                "tenant_id": "other-tenant",
                "agent_name": "agent-a",
                "cron_expression": "0 9 * * *",
                "timezone": "UTC",
                "enabled": true,
                "grace_period_seconds": 120
            }))
            .await;
        assert_ne!(
            update_response.status_code(),
            StatusCode::METHOD_NOT_ALLOWED
        );

        let delete_response = server
            .delete("/admin/tenants/tenant-1/schedules/schedule-1")
            .await;
        assert_ne!(
            delete_response.status_code(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn create_router_registers_emergency_stop_status_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/agents/emergency-stop").await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_runtime_tool_capabilities_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/runtime-tools/capabilities").await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_cordis_service_retire_route() {
        // No env manipulation: other admin tests set/unset ADMIN_API_KEY
        // concurrently. Whether the middleware rejects (401) or the handler
        // runs (200), a non-404 proves the route segment reached the layer.
        let server = test_server(test_app_state());
        let response = server.post("/admin/cordis/services/events_service/retire").await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_cordis_service_provide_route() {
        let server = test_server(test_app_state());
        let response = server.post("/admin/cordis/services/events_service/provide").await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cordis_service_lifecycle_end_to_end_over_http() {
        let ctx = Context::new_root();
        ctx.provide(ares_cordis_core::ReflectService::new());
        ctx.provide(crate::context_services::ToolRegistryService(
            std::sync::Arc::new(ares_tools::ToolRegistry::new()),
        ));
        ctx.provide(ares_cordis_core::EventsService::new());

        let app = crate::api::handlers::admin::cordis::routes()
            .with_state(ctx.clone());
        let server = axum_test::TestServer::new(app).expect("test server");

        // Wrapper-backed name → 409 Conflict (not retirably supported today).
        let response = server.post("/cordis/services/tool_registry/retire").await;
        assert_eq!(response.status_code(), StatusCode::CONFLICT);

        // Real retirement removes EventsService by TypeId.
        let response = server.post("/cordis/services/events_service/retire").await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(response.json::<serde_json::Value>()["retired"], serde_json::json!(true));
        assert!(ctx.get::<ares_cordis_core::EventsService>().is_none());

        // Companion endpoint re-registers it so the cycle repeats.
        let response = server.post("/cordis/services/events_service/provide").await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(response.json::<serde_json::Value>()["provided"], serde_json::json!(true));
        assert!(ctx.get::<ares_cordis_core::EventsService>().is_some());
    }

    #[tokio::test]
    async fn create_router_registers_schedule_missed_runs_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .get("/admin/tenants/tenant-1/schedules/schedule-1/missed-runs")
            .await;
        assert_ne!(response.status_code(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn create_router_registers_connector_update_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .put("/admin/connectors/connector-1")
            .json(&serde_json::json!({
                "tenant_id": "tenant-1",
                "name": "github-main",
                "service_type": "github",
                "auth_config": {},
                "endpoints": {},
                "enabled": true
            }))
            .await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_does_not_register_unscoped_pipeline_delete_route() {
        std::env::set_var("ADMIN_API_KEY", "test-admin-secret");
        let server = test_server(test_app_state());
        let response = server
            .delete("/admin/pipelines/pipeline-1")
            .add_header("x-admin-secret", "test-admin-secret")
            .await;
        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_tenant_connector_delete_route() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .delete("/admin/tenants/tenant-1/connectors/connector-1")
            .await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_billing_routes() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        for path in [
            "/admin/tenants/tenant-1/billing/summary?month=2026-06",
            "/admin/tenants/tenant-1/billing/line-items?month=2026-06",
            "/admin/billing/model-rates",
            "/admin/billing/unit-rates",
        ] {
            let response = server.get(path).await;
            assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
            response.assert_status_unauthorized();
        }
    }

    #[tokio::test]
    async fn create_router_registers_token_budget_routes() {
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        for path in [
            "/admin/token-budgets/tenant-1",
            "/admin/token-budgets/tenant-1/status",
            "/admin/token-budgets/tenant-1/usage",
        ] {
            let response = server.get(path).await;
            assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
            response.assert_status_unauthorized();
        }
        let response = server.post("/admin/token-budgets/tenant-1/reset").await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }
}
