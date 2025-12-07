use crate::AppState;
use axum::{
    middleware,
    routing::{get, post},
    Router,
};

pub fn create_router() -> Router<AppState> {
    let public_routes = Router::new()
        // Public routes (no auth required)
        .route("/auth/register", post(crate::api::handlers::auth::register))
        .route("/auth/login", post(crate::api::handlers::auth::login))
        .route(
            "/auth/refresh",
            post(crate::api::handlers::auth::refresh_token),
        );

    let protected_routes = Router::new()
        // Protected routes (auth required)
        .route("/chat", post(crate::api::handlers::chat::chat))
        .route(
            "/research",
            post(crate::api::handlers::research::deep_research),
        )
        .route("/agents", get(crate::api::handlers::agents::list_agents))
        .route("/memory", get(crate::api::handlers::chat::get_user_memory))
        .layer(middleware::from_fn(
            crate::auth::middleware::auth_middleware,
        ));

    public_routes.merge(protected_routes)
}
