use crate::auth::jwt::AuthService;
use crate::types::Claims;
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use std::sync::Arc;

pub async fn auth_middleware(auth_service: Arc<AuthService>, req: Request, next: Next) -> Response {
    // Extract Authorization header
    if let Some(auth_header) = req.headers().get("authorization")
        && let Ok(auth_str) = auth_header.to_str()
        && let Some(token) = auth_str.strip_prefix("Bearer ")
    {
        match auth_service.verify_token(token) {
            Ok(claims) => {
                let mut req = req;
                req.extensions_mut().insert(claims);
                return next.run(req).await;
            }
            Err(_) => {
                // Invalid token
            }
        }
    }

    // No valid token provided
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body("Unauthorized".into())
        .unwrap()
}

// Extractor for claims
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

pub struct AuthUser(pub Claims);

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Claims>()
            .cloned()
            .map(AuthUser)
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}
