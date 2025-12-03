use crate::{AppState, auth::middleware::auth_middleware};
use axum::{
    Router, middleware,
    routing::{get, post},
};

pub fn create_router() -> Router<AppState> {
    Router::new()
        // Public routes (no auth required)
        .route("/auth/register", post(crate::api::handlers::auth::register))
        .route("/auth/login", post(crate::api::handlers::auth::login))
        .route(
            "/auth/refresh",
            post(crate::api::handlers::auth::refresh_token),
        )
        // Protected routes (auth required)
        .route("/chat", post(crate::api::handlers::chat::chat))
        .route(
            "/research",
            post(crate::api::handlers::research::deep_research),
        )
        .route("/agents", get(crate::api::handlers::agents::list_agents))
        .route("/memory", get(crate::api::handlers::chat::get_user_memory))
        .route_layer(middleware::from_fn_with_state(
            std::marker::PhantomData::<AppState>,
            auth_middleware,
        ))
}
