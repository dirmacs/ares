use crate::db::tenants::TenantDb;
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

pub async fn api_key_auth_middleware(
    req: Request,
    next: Next,
) -> Response {
    let auth_header = match req.headers().get("authorization") {
        Some(h) => h,
        None => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "Missing Authorization header",
            );
        }
    };

    let auth_str = match auth_header.to_str() {
        Ok(s) => s,
        Err(_) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization header",
            );
        }
    };

    let api_key = match auth_str.strip_prefix("Bearer ") {
        Some(k) => k,
        None => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization format. Expected: Bearer ares_...",
            );
        }
    };

    if !api_key.starts_with("ares_") {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid API key format. Must start with ares_",
        );
    }

    let extensions = req.extensions();
    let tenant_db: Arc<TenantDb> = match extensions.get::<Arc<TenantDb>>() {
        Some(db) => db.clone(),
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Tenant database not configured",
            );
        }
    };

    let tenant_ctx = match tenant_db.verify_api_key(api_key).await {
        Ok(Some(ctx)) => ctx,
        Ok(None) => {
            return error_response(StatusCode::UNAUTHORIZED, "Invalid API key");
        }
        Err(e) => {
            tracing::error!("API key verification error: {}", e);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to verify API key",
            );
        }
    };

    let monthly_usage = match tenant_db.get_monthly_requests(&tenant_ctx.tenant_id).await {
        Ok(m) => m,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to check usage",
            );
        }
    };

    let daily_usage = match tenant_db.get_daily_requests(&tenant_ctx.tenant_id).await {
        Ok(d) => d,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to check rate limit",
            );
        }
    };

    if !tenant_ctx.can_make_request(monthly_usage, daily_usage) {
        if monthly_usage >= tenant_ctx.quota.requests_per_month {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Monthly request quota exceeded",
            );
        }
        if daily_usage >= tenant_ctx.quota.requests_per_day {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Daily rate limit exceeded",
            );
        }
    }

    let mut req = req;
    req.extensions_mut().insert(tenant_ctx);

    next.run(req).await
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = Json(serde_json::json!({
        "error": message
    }));
    (status, body).into_response()
}

pub use crate::auth::middleware::AuthUser;

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

    async fn protected_handler() -> &'static str {
        "protected content"
    }

    #[tokio::test]
    async fn test_middleware_no_auth_header() {
        let app = Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(api_key_auth_middleware));

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
    async fn test_middleware_invalid_format() {
        let app = Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(api_key_auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Basic abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_middleware_missing_prefix() {
        let app = Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(api_key_auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer abc123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
