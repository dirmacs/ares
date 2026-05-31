use crate::db::tenants::TenantDb;
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

pub async fn api_key_auth_middleware(req: Request, next: Next) -> Response {
    let auth_header = match req.headers().get("authorization") {
        Some(h) => h,
        None => {
            return error_response(StatusCode::UNAUTHORIZED, "Missing Authorization header");
        }
    };

    let auth_str = match auth_header.to_str() {
        Ok(s) => s,
        Err(_) => {
            return error_response(StatusCode::UNAUTHORIZED, "Invalid Authorization header");
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
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to check usage");
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
            return error_response(StatusCode::TOO_MANY_REQUESTS, "Daily rate limit exceeded");
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
    use crate::db::PostgresClient;
    use crate::models::{TenantContext, TenantTier};
    use axum::{
        body::Body,
        extract::Extension,
        http::{Request, StatusCode},
        middleware::Next,
        routing::get,
        Router,
    };
    use std::sync::{Arc, Once};
    use tower::ServiceExt;

    static LOAD_ENV: Once = Once::new();
    static INIT_SCHEMA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn ensure_env_loaded() {
        LOAD_ENV.call_once(|| {
            let _ = dotenvy::dotenv();
        });
    }

    fn test_db_url() -> String {
        ensure_env_loaded();
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            return url;
        }
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if url.contains("/ares") && !url.contains("ares_test") {
                return url.replace("/ares", "/ares_test");
            }
            return url;
        }
        "postgres://dirmacs@localhost:5432/ares_test".to_string()
    }

    async fn create_test_db() -> PostgresClient {
        let url = test_db_url();
        let db = PostgresClient::new_remote(url, String::new())
            .await
            .expect("Failed to connect to ares_test. Ensure it exists and migrations are applied.");

        ensure_test_schema(&db).await;

        db
    }

    async fn protected_handler(Extension(ctx): Extension<TenantContext>) -> String {
        format!("protected:{}", ctx.tenant_id)
    }

    fn build_app(tenant_db: Arc<TenantDb>) -> Router {
        Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(
                api_key_auth_middleware,
            ))
            .layer(axum::middleware::from_fn(
                move |mut req: Request<Body>, next: Next| {
                    let db = tenant_db.clone();
                    async move {
                        req.extensions_mut().insert(db);
                        next.run(req).await
                    }
                },
            ))
    }


    async fn ensure_test_schema(db: &PostgresClient) {
        let exists: (bool,) = sqlx::query_as(
            "SELECT EXISTS (
                SELECT FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = 'api_keys'
            )",
        )
        .fetch_one(&db.pool)
        .await
        .expect("schema check");

        if !exists.0 {
            sqlx::query("DELETE FROM _sqlx_migrations")
                .execute(&db.pool)
                .await
                .ok();
            sqlx::migrate!("./migrations")
                .run(&db.pool)
                .await
                .expect("rebuild schema");
        }
    }

    async fn restore_test_schema(db: &PostgresClient) {
        sqlx::migrate!("./migrations")
            .run(&db.pool)
            .await
            .expect("restore schema after destructive test");
    }

    async fn provision_tenant(tenant_db: &TenantDb, name: &str) -> (String, String) {
        let tenant = tenant_db
            .create_tenant(name.to_string(), TenantTier::Free)
            .await
            .expect("create tenant");
        let (_, api_key) = tenant_db
            .create_api_key(&tenant.id, format!("{name}-key"))
            .await
            .expect("create api key");
        (tenant.id, api_key)
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

    #[tokio::test]
    async fn test_middleware_missing_tenant_db() {
        let app = Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(api_key_auth_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer ares_test_key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_middleware_valid_api_key_passes() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db));
        let (tenant_id, api_key) = provision_tenant(&tenant_db, "auth-pass").await;
        let app = build_app(tenant_db);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, format!("protected:{tenant_id}"));
    }
    #[tokio::test]
    async fn test_middleware_invalid_api_key_rejected() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db));
        let (_tenant_id, _api_key) = provision_tenant(&tenant_db, "auth-invalid").await;
        let app = build_app(tenant_db);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer ares_invalid_key_value")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn test_middleware_monthly_quota_exceeded() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-monthly").await;

        for _ in 0..1_000 {
            tenant_db
                .record_usage_event(&_tenant_id, 1, 0)
                .await
                .expect("record usage");
        }

        let app = build_app(tenant_db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn test_middleware_daily_quota_exceeded() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-daily").await;

        for _ in 0..50 {
            tenant_db
                .record_usage_event(&_tenant_id, 1, 0)
                .await
                .expect("record usage");
        }

        let app = build_app(tenant_db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    #[tokio::test]
    async fn test_middleware_invalid_auth_header_bytes() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        let app = build_app(tenant_db);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(
                        axum::http::header::AUTHORIZATION,
                        axum::http::HeaderValue::from_bytes(b"Bearer \xFF\xFE").unwrap(),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn test_middleware_verify_api_key_db_error() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-db-verify").await;

        sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_hidden")
            .execute(&db.pool)
            .await
            .expect("hide api_keys");

        let app = build_app(tenant_db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        sqlx::query("ALTER TABLE api_keys_hidden RENAME TO api_keys")
            .execute(&db.pool)
            .await
            .expect("restore api_keys");
    }
    #[tokio::test]
    async fn test_middleware_monthly_usage_db_error() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-db-monthly").await;

        sqlx::query(
            "ALTER TABLE monthly_usage_cache RENAME TO monthly_usage_cache_hidden",
        )
        .execute(&db.pool)
        .await
        .expect("hide monthly_usage_cache");

        let app = build_app(tenant_db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        sqlx::query(
            "ALTER TABLE monthly_usage_cache_hidden RENAME TO monthly_usage_cache",
        )
        .execute(&db.pool)
        .await
        .expect("restore monthly_usage_cache");
    }
    #[tokio::test]
    async fn test_middleware_daily_usage_db_error() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-db-daily").await;

        sqlx::query("ALTER TABLE daily_rate_limits RENAME TO daily_rate_limits_hidden")
            .execute(&db.pool)
            .await
            .expect("hide daily_rate_limits");

        let app = build_app(tenant_db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        sqlx::query("ALTER TABLE daily_rate_limits_hidden RENAME TO daily_rate_limits")
            .execute(&db.pool)
            .await
            .expect("restore daily_rate_limits");
    }
    #[tokio::test]
    async fn test_error_response_json_body() {
        let response = error_response(StatusCode::UNAUTHORIZED, "test message");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "test message");
    }

}
