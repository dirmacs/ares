use crate::AppState;
use axum::{
    Router,
    routing::{get, post},
};

pub fn create_router() -> Router<AppState> {
    // Public routes (no auth required)
    let public_routes = Router::new()
        .route("/auth/register", post(crate::api::handlers::auth::register))
        .route("/auth/login", post(crate::api::handlers::auth::login))
        .route(
            "/auth/refresh",
            post(crate::api::handlers::auth::refresh_token),
        );

    // Protected routes (auth required)
    // Note: Auth is handled via the AuthUser extractor in each handler
    let protected_routes = Router::new()
        .route("/chat", post(crate::api::handlers::chat::chat))
        .route(
            "/research",
            post(crate::api::handlers::research::deep_research),
        )
        .route("/agents", get(crate::api::handlers::agents::list_agents))
        .route("/memory", get(crate::api::handlers::chat::get_user_memory));

    // Merge all routes
    Router::new().merge(public_routes).merge(protected_routes)
}
