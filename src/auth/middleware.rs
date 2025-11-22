use crate::auth::jwt::AuthService;
use crate::types::Claims;
use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

pub async fn auth_middleware(
    State(auth_service): State<Arc<AuthService>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let claims = auth_service
        .verify_token(token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

// Extractor for claims
use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

pub struct AuthUser(pub Claims);

#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Claims>()
            .cloned()
            .map(AuthUser)
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}
