use crate::auth::jwt::AuthService;
use ares_store::tenants::TenantDb;

use axum::{
    extract::Request,
    middleware::{self, Next},
    routing::{delete, get, post, put},
    Router,
};
use std::sync::Arc;

use crate::api::handlers::deploy;
use crate::api::handlers::loops;
use cordis::Context;

/// Creates the main API router with all routes configured.
///
/// Routes are split into public (no auth), protected (requires JWT), and admin (requires admin secret).
/// `tenant_db` is injected into request extensions so `track_usage` middleware can record billing events.
pub fn create_router(
    auth_service: Arc<AuthService>,
    tenant_db: Arc<TenantDb>,
) -> Router<Arc<Context>> {
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

    // RAG routes (requires ares-vector for vector storage)
    #[cfg(feature = "ares-vector")]
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

    // The admin middleware verifies asymmetric tokens through the same
    // JWKS cache as the AuthService. Capture it before `auth_service`
    // moves into the protected-routes layer below.
    let admin_jwks = auth_service.jwks();

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
            get(crate::api::handlers::admin::get_tenant)
                .delete(crate::api::handlers::admin::delete_tenant),
        )
        .route(
            "/admin/tenants/{tenant_id}/api-keys",
            post(crate::api::handlers::admin::create_api_key)
                .get(crate::api::handlers::admin::list_api_keys),
        )
        .route(
            "/admin/tenants/{tenant_id}/api-keys/{key_id}",
            delete(crate::api::handlers::admin::revoke_api_key),
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
        // Cordis provider replacement (rolling drain-and-shift, zero absence)
        .route(
            "/admin/cordis/services/{name}/replace",
            post(crate::api::handlers::admin::replace_cordis_service),
        )
        .route(
            "/admin/cordis/entries/reload",
            post(crate::api::handlers::admin::reload_cordis_entries),
        )
        // Cordis entries management (list / upsert / delete / toggle)
        .route(
            "/admin/cordis/entries",
            get(crate::api::handlers::admin::list_cordis_entries)
                .put(crate::api::handlers::admin::put_cordis_entry),
        )
        .route(
            "/admin/cordis/entries/{id}",
            delete(crate::api::handlers::admin::delete_cordis_entry)
                .patch(crate::api::handlers::admin::patch_cordis_entry),
        )
        .route(
            "/admin/cordis/entries/{id}/toggle",
            post(crate::api::handlers::admin::toggle_cordis_entry),
        )
        // Cordis entry relocation (subtree rename cascade, fiber identity)
        .route(
            "/admin/cordis/entries/{id}/move",
            post(crate::api::handlers::admin::cordis::move_cordis_entry),
        )
        // Per-event dispatch metrics from EventsService
        .route(
            "/admin/cordis/events",
            get(crate::api::handlers::admin::cordis::cordis_event_metrics),
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
        .layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let jwks = admin_jwks.clone();
            async move {
                req.extensions_mut().insert(jwks);
                crate::api::handlers::admin::admin_middleware(req, next).await
            }
        }));

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
        // External usage ingest for library-embedded callers. Unmetered by
        // design: reporting usage must not itself consume quota.
        .route(
            "/usage/events",
            post(crate::api::handlers::v1::ingest_usage_events),
        )
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
            "/api-keys/{id}/rotate",
            post(crate::api::handlers::v1::rotate_api_key),
        )
        .route(
            "/tenant/data",
            delete(crate::api::handlers::v1::delete_tenant_data),
        );

    // Semantic search (requires ares-vector for vector storage)
    #[cfg(feature = "ares-vector")]
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

    // -----------------------------------------------------------------------
    // Guard: no router of admin or /v1 routes can be built outside
    // `create_router` (ares-admin-routes-unmounted).
    //
    // `create_router` is the one function that builds the live router. It
    // layers `admin_middleware` over every `/admin` route and the API-key
    // middleware over every `/v1` route. Three aggregators (`build_routes`,
    // `admin_routes`, `v1_routes`) and seventeen per-domain `routes()`
    // functions once built routers of those WRITE handlers with no middleware
    // at all. Nothing mounted them, but mounting any one would have exposed
    // the writes unauthenticated. They are deleted. This guard keys on what
    // such code DOES, not on what it is called, so a rename, a raw identifier
    // or a macro does not get past it. It fails when:
    //
    //   (a) a function that returns a `Router` (`Router`, `axum::Router<..>`,
    //       `r#Router`, `Result<Router, _>`, a per-file `use .. as` or `type`
    //       alias of it, or a return type that a macro variable supplies) is
    //       defined outside `#[cfg(test)]` in `handlers/admin/**`,
    //       `handlers/v1/**`, `handlers/admin.rs` or `handlers/v1.rs`;
    //   (b) non-test code in `crates/ares-http/src` or the root `src/` calls a
    //       function named `routes` (`x::routes()`, a bare `routes()`, a raw
    //       `r#routes()`), except through a module declared inline in the
    //       same file (`mod ui { .. }` in `src/main.rs`, the embedded UI);
    //   (c) `api/routes.rs` does not define `pub fn create_router` exactly
    //       once. Only non-test definitions count: a `#[cfg(test)]` helper of
    //       that name is not one. This is also the scan's control: it must
    //       find the one live builder and see it return a `Router`, so an
    //       empty result for (a) is a real absence and not a blind scan.
    //
    // Comments and the contents of string and char literals are removed
    // before anything is matched, so a comment that mentions a deleted
    // function, or a test fixture that holds the text of one, is not a
    // finding. Items under `#[cfg(test)]` (and `cfg(all(test, ..))`) are
    // skipped: the test modules may build routers.
    //
    // WHAT THIS CANNOT SEE, stated so nobody reads more into a green run:
    //   - code a proc macro or an attribute macro generates: the scan reads
    //     source text, it never expands anything, and `include!`d files are
    //     not followed (a `macro_rules!` body is text, so it IS read);
    //   - a `Router` reached through an alias or re-export from ANOTHER file
    //     (aliases are followed inside the file that declares them only), or
    //     one reached through a `cfg` predicate other than `test`,
    //     `all(.., test, ..)` and `any(test, ..)`;
    //   - a router assembled out of handler functions inside a function that
    //     does not return a `Router` (a `let` in `main`, say), or any router
    //     builder in a file outside the paths of (a): only the CALLS of
    //     `routes()` are caught there, by (b);
    //   - a call through a `use ..::routes as other` alias: the import itself
    //     is not a call and is not flagged. (`handlers/admin/mod.rs` and
    //     `handlers/v1/mod.rs` are not compiled, and still carry such
    //     imports.) With (a) holding, no `routes()` exists in compiled code
    //     to be imported;
    //   - other repositories. The wrapper (`ares-dirmacs`) is checked by the
    //     orchestrator's verify with a grep, not here.
    // -----------------------------------------------------------------------

    /// One lexical token of a source file.
    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        /// An identifier or keyword. `raw` marks `r#name`; `name` never keeps
        /// the `r#`.
        Ident {
            name: String,
            raw: bool,
        },
        Punct(char),
        /// A string, char or number literal, or a lifetime. The content is
        /// dropped, which is how literal text is kept out of every match.
        Lit,
    }

    #[derive(Debug, Clone)]
    struct Token {
        tok: Tok,
        line: usize,
    }

    impl Token {
        /// An identifier called `s`, raw (`r#s`) or not.
        fn is_name(&self, s: &str) -> bool {
            matches!(&self.tok, Tok::Ident { name, .. } if name == s)
        }

        /// The keyword `s` (a raw identifier is never a keyword).
        fn is_kw(&self, s: &str) -> bool {
            matches!(&self.tok, Tok::Ident { name, raw: false } if name == s)
        }

        fn is_punct(&self, c: char) -> bool {
            self.tok == Tok::Punct(c)
        }

        fn name(&self) -> Option<&str> {
            match &self.tok {
                Tok::Ident { name, .. } => Some(name),
                _ => None,
            }
        }
    }

    /// Index just past the string literal whose opening quote is at `open`.
    fn end_of_string(c: &[char], open: usize, line: &mut usize) -> usize {
        let mut i = open + 1;
        while i < c.len() {
            match c[i] {
                '\\' => {
                    if c.get(i + 1) == Some(&'\n') {
                        *line += 1;
                    }
                    i += 2;
                }
                '"' => return i + 1,
                '\n' => {
                    *line += 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        c.len()
    }

    /// Index just past the raw string whose opening quote is at `quote`: it
    /// closes at a quote followed by `hashes` hashes.
    fn end_of_raw_string(c: &[char], quote: usize, hashes: usize, line: &mut usize) -> usize {
        let mut i = quote + 1;
        while i < c.len() {
            if c[i] == '\n' {
                *line += 1;
            }
            if c[i] == '"' && (1..=hashes).all(|k| c.get(i + k) == Some(&'#')) {
                return i + 1 + hashes;
            }
            i += 1;
        }
        c.len()
    }

    /// Tokens of `src` with `//` and (nested) `/* .. */` comments, and the
    /// content of string, raw string, byte string, char and lifetime
    /// literals, removed. Doc comments are comments.
    fn lex(src: &str) -> Vec<Token> {
        let c: Vec<char> = src.chars().collect();
        let is_id = |ch: char| ch.is_alphanumeric() || ch == '_';
        let mut out: Vec<Token> = Vec::new();
        let mut line = 1usize;
        let mut i = 0usize;
        while i < c.len() {
            let ch = c[i];
            let at = line;
            if ch == '\n' {
                line += 1;
                i += 1;
            } else if ch.is_whitespace() {
                i += 1;
            } else if ch == '/' && c.get(i + 1) == Some(&'/') {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            } else if ch == '/' && c.get(i + 1) == Some(&'*') {
                let mut depth = 1;
                i += 2;
                while i < c.len() && depth > 0 {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        if c[i] == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                }
            } else if ch == '"' {
                i = end_of_string(&c, i, &mut line);
                out.push(Token {
                    tok: Tok::Lit,
                    line: at,
                });
            } else if ch == '\'' {
                if c.get(i + 1) == Some(&'\\') {
                    // An escaped char literal: '\n', '\'', '\u{..}'.
                    i += 3;
                    while i < c.len() && c[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                } else if c.get(i + 2) == Some(&'\'') {
                    // A one-char literal, including '"'.
                    i += 3;
                } else {
                    // A lifetime or a loop label.
                    i += 1;
                    while i < c.len() && is_id(c[i]) {
                        i += 1;
                    }
                }
                out.push(Token {
                    tok: Tok::Lit,
                    line: at,
                });
            } else if ch.is_alphabetic() || ch == '_' {
                let mut j = i;
                while j < c.len() && is_id(c[j]) {
                    j += 1;
                }
                let word: String = c[i..j].iter().collect();
                if matches!(word.as_str(), "r" | "br" | "cr") {
                    let mut k = j;
                    while c.get(k) == Some(&'#') {
                        k += 1;
                    }
                    let hashes = k - j;
                    if c.get(k) == Some(&'"') {
                        i = end_of_raw_string(&c, k, hashes, &mut line);
                        out.push(Token {
                            tok: Tok::Lit,
                            line: at,
                        });
                        continue;
                    }
                    if word == "r"
                        && hashes == 1
                        && c.get(k).is_some_and(|x| x.is_alphabetic() || *x == '_')
                    {
                        let mut e = k;
                        while e < c.len() && is_id(c[e]) {
                            e += 1;
                        }
                        out.push(Token {
                            tok: Tok::Ident {
                                name: c[k..e].iter().collect(),
                                raw: true,
                            },
                            line: at,
                        });
                        i = e;
                        continue;
                    }
                }
                let byte_or_c_string = matches!(word.as_str(), "b" | "c") && c.get(j) == Some(&'"');
                let byte_char = word == "b" && c.get(j) == Some(&'\'');
                if byte_or_c_string || byte_char {
                    // The prefix of a byte or C string, or of a byte char: drop it
                    // and lex the literal on the next turn.
                    i = j;
                    continue;
                }
                out.push(Token {
                    tok: Tok::Ident {
                        name: word,
                        raw: false,
                    },
                    line: at,
                });
                i = j;
            } else if ch.is_ascii_digit() {
                while i < c.len() && is_id(c[i]) {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Lit,
                    line: at,
                });
            } else {
                out.push(Token {
                    tok: Tok::Punct(ch),
                    line: at,
                });
                i += 1;
            }
        }
        out
    }

    /// Index just past the bracket that closes the `(`, `[` or `{` at `open`.
    fn skip_balanced(t: &[Token], open: usize) -> usize {
        let (o, cl) = match t.get(open).map(|x| &x.tok) {
            Some(Tok::Punct('(')) => ('(', ')'),
            Some(Tok::Punct('[')) => ('[', ']'),
            Some(Tok::Punct('{')) => ('{', '}'),
            _ => return open + 1,
        };
        let mut depth = 0i32;
        for (k, x) in t.iter().enumerate().skip(open) {
            if x.is_punct(o) {
                depth += 1;
            } else if x.is_punct(cl) {
                depth -= 1;
                if depth == 0 {
                    return k + 1;
                }
            }
        }
        t.len()
    }

    /// Index just past the `>` that closes the `<` at `open`. The `>` of an
    /// arrow (`Fn() -> X`) does not close anything.
    fn skip_angles(t: &[Token], open: usize) -> usize {
        let mut depth = 0i32;
        let mut k = open;
        while k < t.len() {
            if t[k].is_punct('-') && t.get(k + 1).is_some_and(|x| x.is_punct('>')) {
                k += 2;
                continue;
            }
            if t[k].is_punct('<') {
                depth += 1;
            } else if t[k].is_punct('>') {
                depth -= 1;
                if depth == 0 {
                    return k + 1;
                }
            }
            k += 1;
        }
        t.len()
    }

    /// Index just past the item that starts at `k`: further attributes are
    /// skipped, then the item ends at the first `;` outside any bracket or at
    /// the end of its first `{ .. }` block, whichever comes first.
    fn item_end(t: &[Token], mut k: usize) -> usize {
        while k + 1 < t.len() && t[k].is_punct('#') {
            let open = k + 1 + usize::from(t[k + 1].is_punct('!'));
            if t.get(open).is_some_and(|x| x.is_punct('[')) {
                k = skip_balanced(t, open);
            } else {
                break;
            }
        }
        let mut depth = 0i32;
        while k < t.len() {
            match &t[k].tok {
                Tok::Punct('(') | Tok::Punct('[') => depth += 1,
                Tok::Punct(')') | Tok::Punct(']') => depth -= 1,
                Tok::Punct('{') if depth <= 0 => return skip_balanced(t, k),
                Tok::Punct('{') => depth += 1,
                Tok::Punct('}') => depth -= 1,
                Tok::Punct(';') if depth <= 0 => return k + 1,
                _ => {}
            }
            k += 1;
        }
        t.len()
    }

    /// `p` split at the commas that are not inside parentheses.
    fn split_commas(p: &[Token]) -> Vec<&[Token]> {
        let mut out = Vec::new();
        let (mut depth, mut from) = (0i32, 0usize);
        for (k, x) in p.iter().enumerate() {
            if x.is_punct('(') {
                depth += 1;
            } else if x.is_punct(')') {
                depth -= 1;
            } else if x.is_punct(',') && depth == 0 {
                out.push(&p[from..k]);
                from = k + 1;
            }
        }
        if from < p.len() {
            out.push(&p[from..]);
        }
        out
    }

    /// True when the `cfg` predicate `p` holds only under `cfg(test)`: `test`,
    /// `all(.., test, ..)`, or an `any(..)` whose every arm is such.
    /// Everything else (`not(test)`, `feature = ".."`, ..) is not test-only.
    fn cfg_is_test_only(p: &[Token]) -> bool {
        if p.len() == 1 && p[0].is_name("test") {
            return true;
        }
        let head = p.first().and_then(|x| x.name());
        if p.len() < 3 || !p[1].is_punct('(') || !p[p.len() - 1].is_punct(')') {
            return false;
        }
        let args = split_commas(&p[2..p.len() - 1]);
        match head {
            Some("all") => args.iter().any(|a| cfg_is_test_only(a)),
            Some("any") => !args.is_empty() && args.iter().all(|a| cfg_is_test_only(a)),
            _ => false,
        }
    }

    /// For each token, whether it sits in an item that only exists under
    /// `cfg(test)`: the item after a test-only `#[cfg(..)]`, attribute
    /// included. A file that opens with a test-only `#![cfg(..)]` is all test.
    fn test_mask(t: &[Token]) -> Vec<bool> {
        let mut mask = vec![false; t.len()];
        let mut leading = true; // only inner attributes so far
        let mut i = 0;
        while i < t.len() {
            if !t[i].is_punct('#') {
                leading = false;
                i += 1;
                continue;
            }
            let inner = t.get(i + 1).is_some_and(|x| x.is_punct('!'));
            let open = i + 1 + usize::from(inner);
            if !t.get(open).is_some_and(|x| x.is_punct('[')) {
                leading = false;
                i += 1;
                continue;
            }
            let attr_end = skip_balanced(t, open);
            let is_cfg = t.get(open + 1).is_some_and(|x| x.is_name("cfg"))
                && t.get(open + 2).is_some_and(|x| x.is_punct('('));
            if is_cfg {
                let pred_end = skip_balanced(t, open + 2);
                let test_only = t
                    .get(open + 3..pred_end.saturating_sub(1))
                    .is_some_and(cfg_is_test_only);
                if test_only && inner && leading {
                    return vec![true; t.len()];
                }
                if test_only && !inner {
                    let end = item_end(t, attr_end);
                    for m in &mut mask[i..end] {
                        *m = true;
                    }
                    i = end;
                    leading = false;
                    continue;
                }
            }
            if !inner {
                leading = false;
            }
            i = attr_end;
        }
        mask
    }

    /// Every spelling of `Router` a file can use for it: the name itself, a
    /// `use .. Router as X` alias, and a `type X = ..Router..;` alias.
    fn router_names(t: &[Token]) -> Vec<String> {
        let mut names = vec!["Router".to_string()];
        for (i, x) in t.iter().enumerate() {
            if x.is_name("Router") && t.get(i + 1).is_some_and(|y| y.is_kw("as")) {
                if let Some(alias) = t.get(i + 2).and_then(|y| y.name()) {
                    names.push(alias.to_string());
                }
            }
            if x.is_kw("type") {
                if let Some(alias) = t.get(i + 1).and_then(|y| y.name()) {
                    let end = item_end(t, i);
                    if t[i..end].iter().any(|y| y.is_name("Router")) {
                        names.push(alias.to_string());
                    }
                }
            }
        }
        names
    }

    /// A function definition found in a token stream.
    #[derive(Debug)]
    struct FnDef {
        /// Index of the `fn` token.
        idx: usize,
        line: usize,
        /// The name; `$name` when a macro variable supplies it.
        name: String,
        /// The return type names `Router` (or one of its aliases).
        returns_router: bool,
        /// The return type is a macro variable (`-> $ret`): it may be a
        /// `Router`, and the invocation that decides is not in this text.
        returns_metavar: bool,
    }

    /// Every `fn name(..)` (and `fn $name(..)`) in `t`, with what it returns.
    /// A `fn(..)` pointer type is not a definition.
    fn fn_defs(t: &[Token]) -> Vec<FnDef> {
        let routers = router_names(t);
        let mut out = Vec::new();
        for i in 0..t.len() {
            if !t[i].is_kw("fn") {
                continue;
            }
            let (name, mut j) = match (t.get(i + 1), t.get(i + 2)) {
                (Some(a), _) if a.name().is_some() => (a.name().unwrap().to_string(), i + 2),
                (Some(a), Some(b)) if a.is_punct('$') && b.name().is_some() => {
                    (format!("${}", b.name().unwrap()), i + 3)
                }
                _ => continue,
            };
            if t.get(j).is_some_and(|x| x.is_punct('<')) {
                j = skip_angles(t, j);
            }
            if !t.get(j).is_some_and(|x| x.is_punct('(')) {
                continue;
            }
            j = skip_balanced(t, j);
            let mut ret: Vec<&Token> = Vec::new();
            if t.get(j).is_some_and(|x| x.is_punct('-'))
                && t.get(j + 1).is_some_and(|x| x.is_punct('>'))
            {
                let mut k = j + 2;
                let mut depth = 0i32;
                while k < t.len() {
                    if t[k].is_punct('-') && t.get(k + 1).is_some_and(|x| x.is_punct('>')) {
                        ret.push(&t[k]);
                        ret.push(&t[k + 1]);
                        k += 2;
                        continue;
                    }
                    if depth == 0
                        && (t[k].is_punct('{') || t[k].is_punct(';') || t[k].is_kw("where"))
                    {
                        break;
                    }
                    if t[k].is_punct('(') || t[k].is_punct('[') {
                        depth += 1;
                    } else if t[k].is_punct(')') || t[k].is_punct(']') {
                        depth -= 1;
                    }
                    ret.push(&t[k]);
                    k += 1;
                }
            }
            out.push(FnDef {
                idx: i,
                line: t[i].line,
                name,
                returns_router: ret
                    .iter()
                    .any(|x| x.name().is_some_and(|n| routers.iter().any(|r| r == n))),
                returns_metavar: ret.iter().any(|x| x.is_punct('$')),
            });
        }
        out
    }

    /// (a): `(line, what)` for every function outside `#[cfg(test)]` in `src`
    /// that returns a `Router`, or whose return type a macro variable supplies.
    fn router_fns_outside_cfg_test(src: &str) -> Vec<(usize, String)> {
        let t = lex(src);
        let mask = test_mask(&t);
        fn_defs(&t)
            .into_iter()
            .filter(|f| !mask[f.idx] && (f.returns_router || f.returns_metavar))
            .map(|f| {
                let what = if f.returns_router {
                    "returns a Router"
                } else {
                    "has a macro variable as its return type (it can be a Router)"
                };
                (f.line, format!("fn {} {what}", f.name))
            })
            .collect()
    }

    /// (b): `(line, path)` for every call of a function named `routes`
    /// outside `#[cfg(test)]`: `a::b::routes()`, a bare `routes()`, a raw
    /// `r#routes()`. A definition (`fn routes`), a method call (`x.routes()`)
    /// and a call through a module declared inline in the same file
    /// (`mod ui { .. }`, which cannot be a `handlers` module: those are files)
    /// are not findings.
    fn routes_calls_outside_cfg_test(src: &str) -> Vec<(usize, String)> {
        let t = lex(src);
        let mask = test_mask(&t);
        let inline_mods: Vec<&str> = t
            .windows(3)
            .filter(|w| w[0].is_kw("mod") && w[2].is_punct('{'))
            .filter_map(|w| w[1].name())
            .collect();
        let mut out = Vec::new();
        for i in 0..t.len() {
            if mask[i] || !t[i].is_name("routes") {
                continue;
            }
            let called = t.get(i + 1).is_some_and(|x| x.is_punct('('))
                && t.get(i + 2).is_some_and(|x| x.is_punct(')'));
            let defined_or_method = i >= 1 && (t[i - 1].is_kw("fn") || t[i - 1].is_punct('.'));
            if !called || defined_or_method {
                continue;
            }
            let mut segs = vec!["routes".to_string()];
            let mut k = i;
            while k >= 3 && t[k - 1].is_punct(':') && t[k - 2].is_punct(':') {
                match t[k - 3].name() {
                    Some(n) => segs.push(n.to_string()),
                    None => break,
                }
                k -= 3;
            }
            if segs.len() > 1 && inline_mods.contains(&segs[1].as_str()) {
                continue;
            }
            segs.reverse();
            out.push((t[i].line, format!("{}()", segs.join("::"))));
        }
        out
    }

    /// (c): the number of `pub fn <name>` definitions outside `#[cfg(test)]`
    /// in `src`, and whether each is seen returning a `Router`.
    fn pub_fns_named_outside_cfg_test(src: &str, name: &str) -> Vec<bool> {
        let t = lex(src);
        let mask = test_mask(&t);
        let is_plain_pub = |idx: usize| {
            let mut k = idx;
            while k > 0 && (t[k - 1].is_name("async") || t[k - 1].is_name("const")) {
                k -= 1;
            }
            k > 0 && t[k - 1].is_kw("pub")
        };
        fn_defs(&t)
            .into_iter()
            .filter(|f| f.name == name && !mask[f.idx] && is_plain_pub(f.idx))
            .map(|f| f.returns_router)
            .collect()
    }

    fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect_rs_files(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }

    /// The scan's own matchers must see every shape of function that returns a
    /// `Router` (any name, any spelling), and none of the shapes that do not.
    /// A matcher that matches nothing would make the guard below pass
    /// vacuously.
    #[test]
    fn router_fn_scan_matches_every_spelling_and_nothing_else() {
        for src in [
            "pub fn a(ctx: &Arc<Context>) -> Router<Arc<Context>> { x }",
            "pub fn a() -> axum::Router<Arc<Context>> { x }",
            "    pub(crate) fn a() -> Router { x }",
            "pub async fn a<S>() -> Router<S> { x }",
            "fn   a ( ) -> :: axum :: Router { x }",
            "fn a() -> Router where S: Send { x }",
            "pub fn a<F: Fn() -> u8>(f: F) -> Router { x }",
            // renamed, raw identifiers (the name and the type)
            "pub fn all_admin_routes() -> axum::Router<S> { x }",
            "pub fn r#admin_routes() -> axum::Router<S> { x }",
            "pub fn a() -> axum::r#Router<S> { x }",
            // a Router inside another type
            "pub fn a() -> Result<Router, E> { x }",
            "pub fn a() -> impl Into<Router> { x }",
            // a Router under another name, in the same file
            "use axum::Router as R; pub fn a() -> R { x }",
            "type Rt = axum::Router<()>; pub fn a() -> Rt { x }",
            // a macro that defines the function
            "macro_rules! m { ($n:ident) => { pub fn $n() -> axum::Router { x } }; }",
            // a macro that supplies the return type: it may be a Router
            "macro_rules! m { ($n:ident, $r:ty) => { pub fn $n() -> $r { x } }; }",
            // a Router fn after a test item is still seen
            "#[cfg(test)] fn t() {} pub fn a() -> Router { x }",
            // `cfg` predicates that are NOT test-only
            "#[cfg(not(test))] pub fn a() -> Router { x }",
            "#[cfg(feature = \"x\")] pub fn a() -> Router { x }",
            "#[cfg(any(test, feature = \"x\"))] pub fn a() -> Router { x }",
            // `#![cfg(..)]` that is not test-only, and not at the top
            "pub fn t() {} #![cfg(test)] pub fn a() -> Router { x }",
        ] {
            assert_eq!(
                router_fns_outside_cfg_test(src).len(),
                1,
                "must be flagged exactly once: {src}"
            );
        }
        for src in [
            "// pub fn a() -> Router { x }",
            "/// pub fn a() -> Router { x }",
            "//! pub fn a() -> Router { x }",
            "/* pub fn a() -> Router { x } */",
            "/* /* nested */ pub fn a() -> Router { x } */",
            "const S: &str = \"pub fn a() -> Router { x }\";",
            "const S: &str = \"a \\\" quote \\\" pub fn a() -> Router { x }\";",
            "const S: &str = r#\"pub fn \"a\"() -> Router { x }\"#;",
            "const S: &[u8] = b\"pub fn a() -> Router { x }\";",
            "fn c() { let q = '\"'; let n = '\\n'; } // pub fn a() -> Router { x }",
            "pub fn a() -> MethodRouter { x }",
            "pub fn a() -> axum::response::Response { x }",
            "pub fn a(r: Router) -> u8 { 1 }",
            "pub fn a<'a>(r: &'a str) -> &'a str { r }",
            "type F = u8; pub fn a() -> F { x }",
            "let f: fn() -> u8 = a;",
            "pub fn Router() {}",
            // test-only items
            "#[cfg(test)] pub fn a() -> Router { x }",
            "#[cfg(test)] mod t { pub fn a() -> Router { x } }",
            "#[cfg(all(test, feature = \"postgres\"))] #[allow(x)] mod t { fn a() -> Router { x } }",
            "#[cfg(any(test, test))] fn a() -> Router { x }",
            "#[cfg(test)] fn a() -> Router where S: Send { x }",
            "#![cfg(test)] pub fn a() -> Router { x }",
            "#![allow(x)] #![cfg(test)] pub fn a() -> Router { x }",
        ] {
            assert!(
                router_fns_outside_cfg_test(src).is_empty(),
                "must not be flagged: {src}"
            );
        }
        // The report names the function and the line it starts on.
        let found = router_fns_outside_cfg_test("\n\npub fn all_admin_routes() -> Router { x }");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 3, "the line of the `fn` token");
        assert!(found[0].1.contains("all_admin_routes"), "{found:?}");
    }

    /// (b)'s matcher: every way to call a per-domain `routes()`, and none of
    /// the shapes that are not such a call.
    #[test]
    fn routes_call_scan_matches_calls_and_nothing_else() {
        for src in [
            "fn f() { crate::api::handlers::admin::cordis::routes() }",
            "fn f() { tenants::routes ( ) }",
            "fn f() { ares_http::api::handlers::v1::chat::routes().merge(x) }",
            "fn f() { routes() }",
            "fn f() { r#routes() }",
            "fn f() { a::b::r#routes() }",
            "fn f() { ::a::routes() }",
            // a module that is a file, not an inline module
            "#[path = \"admin/cordis.rs\"] pub mod cordis; fn f() { cordis::routes() }",
            "#[cfg(not(test))] fn f() { admin::routes() }",
            "#[cfg(test)] fn t() {} fn f() { admin::routes() }",
        ] {
            assert_eq!(
                routes_calls_outside_cfg_test(src).len(),
                1,
                "must be flagged exactly once: {src}"
            );
        }
        for src in [
            "pub fn routes() -> u8 { 1 }",
            "fn f(x: X) { x.routes() }",
            "// admin::routes()",
            "/* admin::routes() */",
            "const S: &str = \"admin::routes()\";",
            "fn f() { let routes = vec![1]; routes.len(); for r in routes {} }",
            "fn f() { Router::new().route(\"/a\", get(h)) }",
            "fn f() { routes!() }",
            "pub use tenants::routes as tenants_routes;",
            "mod ui { pub fn routes() {} } fn f() { ui::routes() }",
            "mod ui { pub fn routes() {} } fn f() { crate::ui::routes() }",
            "#[cfg(test)] fn f() { admin::routes() }",
            "#[cfg(all(test, feature = \"postgres\"))] mod tests { fn f() { admin::routes() } }",
        ] {
            assert!(
                routes_calls_outside_cfg_test(src).is_empty(),
                "must not be flagged: {src}"
            );
        }
        let found = routes_calls_outside_cfg_test("\nfn f() { crate::admin::tenants::routes() }");
        assert_eq!(
            found,
            vec![(2, "crate::admin::tenants::routes()".to_string())]
        );
    }

    /// (c)'s matcher: only a non-test, plain `pub fn create_router` counts.
    #[test]
    fn create_router_definition_scan_counts_definitions_only() {
        let count = |src: &str| pub_fns_named_outside_cfg_test(src, "create_router").len();
        assert_eq!(count("pub fn create_router(a: A) -> Router<S> { x }"), 1);
        assert_eq!(
            count("pub fn create_router() {} pub fn create_router() {}"),
            2
        );
        assert_eq!(count("pub async fn create_router() {}"), 1);
        assert_eq!(count("pub fn r#create_router() {}"), 1);
        assert_eq!(count(""), 0);
        // not definitions of the live builder
        assert_eq!(count("#[cfg(test)] fn create_router() {}"), 0);
        assert_eq!(count("#[cfg(test)] pub fn create_router() {}"), 0);
        assert_eq!(count("mod t { #[cfg(test)] fn create_router() {} }"), 0);
        assert_eq!(count("fn create_router() {}"), 0);
        assert_eq!(count("pub(crate) fn create_router() {}"), 0);
        assert_eq!(count("pub fn create_router_for_tests() {}"), 0);
        assert_eq!(count("// pub fn create_router() {}"), 0);
        assert_eq!(count("const S: &str = \"pub fn create_router() {}\";"), 0);
        assert_eq!(count("fn f() { create_router(a, b) }"), 0);
        // and the control reads the return type
        assert_eq!(
            pub_fns_named_outside_cfg_test(
                "pub fn create_router() -> Router<S> { x }",
                "create_router"
            ),
            vec![true]
        );
        assert_eq!(
            pub_fns_named_outside_cfg_test("pub fn create_router() -> u8 { 1 }", "create_router"),
            vec![false]
        );
    }

    /// The guard over the real tree: see the block comment above for what it
    /// checks, (a), (b) and (c), and for what it cannot see. It reads the
    /// source at run time, so it follows the tree as it is checked out.
    #[test]
    fn no_unmounted_router_aggregator_is_defined() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let http_src = manifest.join("src");
        let root_src = manifest.join("..").join("..").join("src");
        let mut http_files = Vec::new();
        collect_rs_files(&http_src, &mut http_files);
        let mut root_files = Vec::new();
        collect_rs_files(&root_src, &mut root_files);

        // Where (a) looks: the admin and v1 handler trees and their two shims.
        let handlers = http_src.join("api").join("handlers");
        let in_admin_or_v1 = |p: &std::path::Path| {
            p == handlers.join("admin.rs")
                || p == handlers.join("v1.rs")
                || p.starts_with(handlers.join("admin"))
                || p.starts_with(handlers.join("v1"))
        };
        let scoped = http_files
            .iter()
            .filter(|p| in_admin_or_v1(p.as_path()))
            .count();

        // The walks must have found the trees, or an empty result means nothing.
        assert!(
            http_files.len() > 20,
            "the scan walked only {} files under {}; the source walk is broken",
            http_files.len(),
            http_src.display()
        );
        assert!(
            scoped > 20,
            "(a) saw only {scoped} files under handlers/admin and handlers/v1; the scope is broken"
        );
        assert!(
            root_files.iter().any(|p| p.ends_with("main.rs")),
            "(b) did not find the root src/main.rs under {}; the root walk is broken",
            root_src.display()
        );

        let read = |p: &std::path::Path| {
            std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let mut offenders: Vec<String> = Vec::new();
        for path in &http_files {
            let text = read(path.as_path());
            let rel = path.strip_prefix(&http_src).unwrap_or(path).display();
            if in_admin_or_v1(path.as_path()) {
                for (line, what) in router_fns_outside_cfg_test(&text) {
                    offenders.push(format!("(a) crates/ares-http/src/{rel}:{line}: {what}"));
                }
            }
            for (line, call) in routes_calls_outside_cfg_test(&text) {
                offenders.push(format!(
                    "(b) crates/ares-http/src/{rel}:{line}: non-test code calls {call}"
                ));
            }
        }
        for path in &root_files {
            let rel = path.strip_prefix(&root_src).unwrap_or(path).display();
            for (line, call) in routes_calls_outside_cfg_test(&read(path.as_path())) {
                offenders.push(format!("(b) src/{rel}:{line}: non-test code calls {call}"));
            }
        }

        // (c), and the control for (a): `create_router` is defined once, and
        // the scan sees it return a Router.
        let live = pub_fns_named_outside_cfg_test(
            &read(&http_src.join("api").join("routes.rs")),
            "create_router",
        );
        if live.len() != 1 {
            offenders.push(format!(
                "(c) api/routes.rs defines `pub fn create_router` {} times; it must be exactly once",
                live.len()
            ));
        } else if !live[0] {
            offenders.push(
                "(c) the scan did not see `create_router` return a Router, so (a) would be blind"
                    .to_string(),
            );
        }

        assert!(
            offenders.is_empty(),
            "a router of admin or /v1 routes must be built by `create_router`, behind its \
             middleware, and by nothing else. Delete these (or put them under `#[cfg(test)]`): \
             {offenders:#?}"
        );
    }
}

#[cfg(all(test, feature = "postgres"))]
// ADMIN_ENV_LOCK serializes tests that mutate the process-global ADMIN_API_KEY
// env var; the guard must span the awaited requests because handlers read the
// env var mid-await, so scoping it earlier would reintroduce the race.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::api::handlers::admin::shared::lock_admin_env;
    use crate::config::{AuthConfig, ServerConfig};
    use crate::overlay::{
        AgentConfig, AresConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths, ModelConfig,
        ProviderConfig, RagConfig,
    };
    use crate::{AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager};
    use ares_agent::context_provider::NoOpContextProvider;
    use ares_agent::AgentRegistry;
    use ares_llm::ProviderRegistry;
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
                compaction_enabled: None,
                temperature: None,
                max_tokens: None,
                stop: None,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
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

    fn test_app_state() -> Arc<Context> {
        let ctx = cordis::Context::new_root();
        let config = minimal_config();
        let config_manager = Arc::new(AresConfigManager::from_config(config));
        ctx.provide_arc(config_manager.clone());
        let db = Arc::new(ares_store::PostgresClient::new_test());
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        ctx.provide_arc(tenant_db.clone());
        ctx.provide_arc(db.clone());
        let auth_service = Arc::new(AuthService::new(
            "test-secret-at-least-32-characters-long".into(),
            900,
            604800,
        ));
        ctx.provide_arc(auth_service.clone());
        ctx.provide(deploy::new_deploy_registry());
        ctx.provide(loops::LoopRegistry::new());
        ctx
    }

    fn test_server(state: Arc<Context>) -> axum_test::TestServer {
        let auth = state
            .get::<crate::auth::jwt::AuthService>()
            .expect("not provided")
            .clone();
        let tenant_db = state
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .clone();
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
        let _ = create_router(
            state
                .get::<crate::auth::jwt::AuthService>()
                .expect("not provided")
                .clone(),
            state
                .get::<ares_store::TenantDb>()
                .expect("not provided")
                .clone(),
        );
    }

    #[test]
    fn route_contract_does_not_depend_on_env() {
        let _env_guard = lock_admin_env();
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
        // Same secret `test_app_state` installs in the AuthService. The token
        // carries the ARES product claim, which `auth_middleware` requires.
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &serde_json::json!({
                "sub": "user-1",
                "email": "user@example.com",
                "exp": chrono::Utc::now().timestamp() + 3600,
                "iat": chrono::Utc::now().timestamp(),
                "roles": { "ares": [{ "role": "user" }] },
            }),
            &jsonwebtoken::EncodingKey::from_secret(b"test-secret-at-least-32-characters-long"),
        )
        .expect("tokens");
        let server = test_server(state);
        let response = server
            .get("/loops")
            .add_header("authorization", format!("Bearer {token}"))
            .await;
        response.assert_status_ok();
    }

    #[tokio::test]
    async fn create_router_admin_deploys_rejects_missing_secret() {
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/run-history/llm-calls").await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_run_history_budget_route() {
        let _env_guard = lock_admin_env();
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/run-history/budgets/tenant-1").await;
        assert_ne!(response.status_code(), axum::http::StatusCode::NOT_FOUND);
        response.assert_status_unauthorized();
    }

    #[tokio::test]
    async fn create_router_registers_schedule_update_route() {
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/agents/emergency-stop").await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_runtime_tool_capabilities_route() {
        let _env_guard = lock_admin_env();
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server.get("/admin/runtime-tools/capabilities").await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_cordis_service_retire_route() {
        let _env_guard = lock_admin_env();
        // No env manipulation: other admin tests set/unset ADMIN_API_KEY
        // concurrently. Whether the middleware rejects (401) or the handler
        // runs (200), a non-404 proves the route segment reached the layer.
        let server = test_server(test_app_state());
        let response = server
            .post("/admin/cordis/services/events_service/retire")
            .await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_router_registers_cordis_service_provide_route() {
        let server = test_server(test_app_state());
        let response = server
            .post("/admin/cordis/services/events_service/provide")
            .await;
        assert_ne!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cordis_service_lifecycle_end_to_end_over_http() {
        let ctx = Context::new_root();
        ctx.provide(cordis::ReflectService::new());
        ctx.provide(ares_tools::Tools::from_static(Vec::<
            std::sync::Arc<dyn ares_tools::Tool>,
        >::new()));
        ctx.provide(cordis::EventsService::new());

        let app = crate::api::handlers::admin::cordis::routes().with_state(ctx.clone());
        let server = axum_test::TestServer::new(app).expect("test server");

        // Wrapper-backed name → 409 Conflict (not retirably supported today).
        let response = server.post("/cordis/services/tool_registry/retire").await;
        assert_eq!(response.status_code(), StatusCode::CONFLICT);

        // Real retirement removes EventsService by TypeId.
        let response = server.post("/cordis/services/events_service/retire").await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.json::<serde_json::Value>()["retired"],
            serde_json::json!(true)
        );
        assert!(ctx.get::<cordis::EventsService>().is_none());

        // Companion endpoint re-registers it so the cycle repeats.
        let response = server.post("/cordis/services/events_service/provide").await;
        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(
            response.json::<serde_json::Value>()["provided"],
            serde_json::json!(true)
        );
        assert!(ctx.get::<cordis::EventsService>().is_some());
    }

    #[tokio::test]
    async fn create_router_registers_schedule_missed_runs_route() {
        let _env_guard = lock_admin_env();
        std::env::remove_var("ADMIN_API_KEY");
        let server = test_server(test_app_state());
        let response = server
            .get("/admin/tenants/tenant-1/schedules/schedule-1/missed-runs")
            .await;
        assert_ne!(response.status_code(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn create_router_registers_connector_update_route() {
        let _env_guard = lock_admin_env();
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
        let _env = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
        let _env_guard = lock_admin_env();
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
