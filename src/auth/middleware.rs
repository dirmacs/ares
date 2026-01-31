use crate::auth::jwt::AuthService;
use crate::types::Claims;
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use std::sync::Arc;

/// Axum middleware that validates JWT tokens from the Authorization header.
///
/// Expects tokens in the format: `Authorization: Bearer <token>`
/// On success, injects `Claims` into request extensions for downstream handlers.
pub async fn auth_middleware(auth_service: Arc<AuthService>, req: Request, next: Next) -> Response {
    // Extract Authorization header
    if let Some(auth_header) = req.headers().get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                match auth_service.verify_token(token) {
                    Ok(claims) => {
                        let mut req = req;
                        req.extensions_mut().insert(claims);
                        return next.run(req).await;
                    }
                    Err(e) => {
                        tracing::debug!("Token verification failed: {}", e);
                    }
                }
            }
        }
    }

    // No valid token provided - return JSON error for consistency
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .body(r#"{"error":"Unauthorized"}"#.into())
        .unwrap()
}

// Extractor for claims
use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// Extractor for authenticated user claims.
///
/// Use in handler signatures to require authentication:
/// ```ignore
/// async fn handler(AuthUser(claims): AuthUser) -> impl IntoResponse {
///     format!("Hello, {}", claims.sub)
/// }
/// ```
pub struct AuthUser(pub Claims);

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, axum::Json<serde_json::Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Claims>()
            .cloned()
            .map(AuthUser)
            .ok_or_else(|| {
                (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({"error": "Unauthorized"})),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    fn create_test_auth_service() -> Arc<AuthService> {
        Arc::new(AuthService::new(
            "test-secret-key-that-is-at-least-32-chars".to_string(),
            900,
            604800,
        ))
    }

    async fn protected_handler() -> &'static str {
        "protected content"
    }

    fn create_test_app(auth_service: Arc<AuthService>) -> Router {
        Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(move |req, next| {
                let auth = auth_service.clone();
                async move { auth_middleware(auth, req, next).await }
            }))
    }

    #[tokio::test]
    async fn test_middleware_no_auth_header() {
        let auth_service = create_test_auth_service();
        let app = create_test_app(auth_service);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_invalid_token() {
        let auth_service = create_test_auth_service();
        let app = create_test_app(auth_service);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer invalid.token.here")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_valid_token() {
        let auth_service = create_test_auth_service();
        let tokens = auth_service
            .generate_tokens("user-123", "test@example.com")
            .expect("should generate tokens");

        let app = create_test_app(auth_service);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", tokens.access_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_middleware_malformed_auth_header() {
        let auth_service = create_test_auth_service();
        let app = create_test_app(auth_service);

        // Missing "Bearer " prefix
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "some-token-without-bearer")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_empty_bearer_token() {
        let auth_service = create_test_auth_service();
        let app = create_test_app(auth_service);

        // Bearer prefix but empty token
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer ")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_expired_token() {
        // Create auth service with very short expiry (1 second) and zero leeway
        // Zero leeway ensures strict expiration checking for reliable testing
        let auth_service = Arc::new(AuthService::with_leeway(
            "test-secret-key-that-is-at-least-32-chars".to_string(),
            1, // 1 second access token expiry
            1, // 1 second refresh token expiry
            0, // Zero leeway for strict expiration checking
        ));
        let tokens = auth_service
            .generate_tokens("user-123", "test@example.com")
            .expect("should generate tokens");

        // Wait for token to expire
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let app = create_test_app(auth_service);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", tokens.access_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_wrong_secret() {
        // Create token with one secret
        let auth_service_a = Arc::new(AuthService::new(
            "secret-a-that-is-at-least-32-characters".to_string(),
            900,
            604800,
        ));
        let tokens = auth_service_a
            .generate_tokens("user-123", "test@example.com")
            .expect("should generate tokens");

        // Try to verify with different secret
        let auth_service_b = Arc::new(AuthService::new(
            "secret-b-that-is-at-least-32-characters".to_string(),
            900,
            604800,
        ));
        let app = create_test_app(auth_service_b);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", tokens.access_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_lowercase_bearer() {
        let auth_service = create_test_auth_service();
        let tokens = auth_service
            .generate_tokens("user-123", "test@example.com")
            .expect("should generate tokens");

        let app = create_test_app(auth_service);

        // Use lowercase "bearer" instead of "Bearer"
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("bearer {}", tokens.access_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should be unauthorized - we require exact "Bearer " prefix
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
