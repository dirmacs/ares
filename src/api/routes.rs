use crate::AppState;
use crate::auth::jwt::AuthService;
use axum::{
    Router, middleware,
    routing::{get, post},
};
use std::sync::Arc;

pub fn create_router(auth_service: Arc<AuthService>) -> Router<AppState> {
    let public_routes = Router::new()
        // Public routes (no auth required)
        .route("/auth/register", post(crate::api::handlers::auth::register))
        .route("/auth/login", post(crate::api::handlers::auth::login))
        .route(
            "/auth/refresh",
            post(crate::api::handlers::auth::refresh_token),
        )
        .route("/agents", get(crate::api::handlers::agents::list_agents));

    let protected_routes = Router::new()
        // Protected routes (auth required)
        .route("/chat", post(crate::api::handlers::chat::chat))
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
        .layer(middleware::from_fn(move |req, next| {
            crate::auth::middleware::auth_middleware(auth_service.clone(), req, next)
        }));

    public_routes.merge(protected_routes)
}
