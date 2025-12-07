use crate::auth::jwt::AuthService;
use crate::types::Claims;
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

pub async fn auth_middleware(mut req: Request, next: Next) -> Response {
    // Temporary: Put fake claims for testing
    // TODO: Properly implement auth middleware with state access
    use crate::types::Claims;
    let fake_claims = Claims {
        sub: "test-user".to_string(),
        email: "test@example.com".to_string(),
        exp: 2000000000, // far future
        iat: 1000000000,
    };
    req.extensions_mut().insert(fake_claims);
    next.run(req).await
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
