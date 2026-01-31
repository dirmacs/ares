use crate::auth::jwt::AuthService;
use crate::AppState;
#[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
use axum::routing::delete;
use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use std::sync::Arc;

/// Creates the main API router with all routes configured.
///
/// Routes are split into public (no auth) and protected (requires JWT).
pub fn create_router(auth_service: Arc<AuthService>) -> Router<AppState> {
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

    let protected_routes = protected_routes.layer(middleware::from_fn(move |req, next| {
        crate::auth::middleware::auth_middleware(auth_service.clone(), req, next)
    }));

    public_routes.merge(protected_routes)
}
