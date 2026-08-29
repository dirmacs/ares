use ares_agent::admit::{admit_with_details, quota_exceeded, AdmissionError, UsagePeriod};
use ares_store::tenants::TenantDb;
use ares_types::models::{QuotaExceeded, TenantContext};
use axum::{
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use cordis::{Context, EventsService};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiKeyAuthError {
    MissingAuthorizationHeader,
    InvalidAuthorizationHeader,
    InvalidAuthorizationFormat,
    InvalidApiKeyFormat,
}

impl ApiKeyAuthError {
    pub(crate) fn status_code(self) -> StatusCode {
        StatusCode::UNAUTHORIZED
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::MissingAuthorizationHeader => "Missing Authorization header",
            Self::InvalidAuthorizationHeader => "Invalid Authorization header",
            Self::InvalidAuthorizationFormat => {
                "Invalid Authorization format. Expected: Bearer ares_..."
            }
            Self::InvalidApiKeyFormat => "Invalid API key format. Must start with ares_",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ErrorResponseBody {
    pub error: String,
}

pub(crate) fn parse_authorization_header(
    header: Option<&HeaderValue>,
) -> Result<&str, ApiKeyAuthError> {
    let auth_header = header.ok_or(ApiKeyAuthError::MissingAuthorizationHeader)?;
    auth_header
        .to_str()
        .map_err(|_| ApiKeyAuthError::InvalidAuthorizationHeader)
}

pub(crate) fn extract_api_key(auth_str: &str) -> Result<&str, ApiKeyAuthError> {
    auth_str
        .strip_prefix("Bearer ")
        .ok_or(ApiKeyAuthError::InvalidAuthorizationFormat)
}

pub(crate) fn validate_api_key_format(api_key: &str) -> Result<(), ApiKeyAuthError> {
    if api_key.starts_with("ares_") {
        Ok(())
    } else {
        Err(ApiKeyAuthError::InvalidApiKeyFormat)
    }
}

pub(crate) fn parse_bearer_api_key(auth_str: &str) -> Result<&str, ApiKeyAuthError> {
    let api_key = extract_api_key(auth_str)?;
    validate_api_key_format(api_key)?;
    Ok(api_key)
}

pub(crate) fn check_quota(
    tenant_ctx: &TenantContext,
    monthly_usage: u64,
    daily_usage: u64,
) -> Option<QuotaExceeded> {
    quota_exceeded(tenant_ctx, monthly_usage, daily_usage)
}

fn auth_error_response(err: ApiKeyAuthError) -> Response {
    error_response(err.status_code(), err.message())
}

fn quota_exceeded_response(exceeded: QuotaExceeded) -> Response {
    match exceeded {
        QuotaExceeded::Monthly => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "Monthly request quota exceeded",
        ),
        QuotaExceeded::Daily => {
            error_response(StatusCode::TOO_MANY_REQUESTS, "Daily rate limit exceeded")
        }
    }
}

pub async fn api_key_auth_middleware(req: Request, next: Next) -> Response {
    let auth_str = match parse_authorization_header(req.headers().get("authorization")) {
        Ok(s) => s,
        Err(e) => return auth_error_response(e),
    };

    let api_key = match parse_bearer_api_key(auth_str) {
        Ok(k) => k,
        Err(e) => return auth_error_response(e),
    };

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

    let base_ctx = req
        .extensions()
        .get::<Arc<Context>>()
        .cloned()
        .unwrap_or_else(Context::new_root);
    let admission_ctx = base_ctx.with_intercept(tenant_ctx.clone());
    if admission_ctx.get::<TenantDb>().is_none() {
        admission_ctx.provide_arc(tenant_db.clone());
    }
    if admission_ctx.get::<EventsService>().is_none() {
        admission_ctx.provide(EventsService::new());
    }

    if let Err(error) = admit_with_details(&admission_ctx).await {
        match error {
            AdmissionError::Quota(exceeded) => return quota_exceeded_response(exceeded),
            AdmissionError::Usage {
                period: UsagePeriod::Monthly,
                ..
            } => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to check usage");
            }
            AdmissionError::Usage {
                period: UsagePeriod::Daily,
                ..
            } => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to check rate limit",
                );
            }
            AdmissionError::Event(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to check usage");
            }
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
    use ares_types::models::{TenantContext, TenantTier};
    use axum::{
        body::Body,
        extract::Extension,
        http::{HeaderValue, Request, StatusCode},
        middleware::Next,
        routing::get,
        Router,
    };
    use std::sync::Arc;
    use tower::ServiceExt;

    static DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn response_error_message(response: axum::response::Response) -> String {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        json["error"].as_str().unwrap().to_string()
    }

    async fn protected_handler(Extension(ctx): Extension<TenantContext>) -> String {
        format!("protected:{}", ctx.tenant_id)
    }

    fn build_app(tenant_db: Arc<TenantDb>) -> Router {
        Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(api_key_auth_middleware))
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

    fn build_app_with_context(tenant_db: Arc<TenantDb>, ctx: Arc<Context>) -> Router {
        Router::new()
            .route("/protected", get(protected_handler))
            .layer(axum::middleware::from_fn(api_key_auth_middleware))
            .layer(axum::middleware::from_fn(
                move |mut req: Request<Body>, next: Next| {
                    let db = tenant_db.clone();
                    let ctx = ctx.clone();
                    async move {
                        req.extensions_mut().insert(db);
                        req.extensions_mut().insert(ctx);
                        next.run(req).await
                    }
                },
            ))
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

    // --- pure helper unit tests (no DB / HTTP) ---

    #[test]
    fn test_parse_authorization_header_missing() {
        assert_eq!(
            parse_authorization_header(None),
            Err(ApiKeyAuthError::MissingAuthorizationHeader)
        );
    }

    #[test]
    fn test_parse_authorization_header_valid() {
        let header = HeaderValue::from_static("Bearer ares_test");
        assert_eq!(
            parse_authorization_header(Some(&header)),
            Ok("Bearer ares_test")
        );
    }

    #[test]
    fn test_parse_authorization_header_invalid_bytes() {
        let header = HeaderValue::from_bytes(b"Bearer \xFF\xFE").unwrap();
        assert_eq!(
            parse_authorization_header(Some(&header)),
            Err(ApiKeyAuthError::InvalidAuthorizationHeader)
        );
    }

    #[test]
    fn test_parse_authorization_header_basic_auth_value() {
        let header = HeaderValue::from_static("Basic dXNlcjpwYXNz");
        assert_eq!(
            parse_authorization_header(Some(&header)),
            Ok("Basic dXNlcjpwYXNz")
        );
    }

    #[test]
    fn test_extract_api_key_strips_bearer_prefix() {
        assert_eq!(extract_api_key("Bearer ares_abc123"), Ok("ares_abc123"));
    }

    #[test]
    fn test_extract_api_key_rejects_basic_auth() {
        assert_eq!(
            extract_api_key("Basic abc123"),
            Err(ApiKeyAuthError::InvalidAuthorizationFormat)
        );
    }

    #[test]
    fn test_extract_api_key_rejects_lowercase_bearer() {
        assert_eq!(
            extract_api_key("bearer ares_abc"),
            Err(ApiKeyAuthError::InvalidAuthorizationFormat)
        );
    }

    #[test]
    fn test_extract_api_key_rejects_missing_space() {
        assert_eq!(
            extract_api_key("Bearerares_abc"),
            Err(ApiKeyAuthError::InvalidAuthorizationFormat)
        );
    }

    #[test]
    fn test_extract_api_key_accepts_empty_token() {
        assert_eq!(extract_api_key("Bearer "), Ok(""));
    }

    #[test]
    fn test_extract_api_key_rejects_token_without_bearer() {
        assert_eq!(
            extract_api_key("ares_abc123"),
            Err(ApiKeyAuthError::InvalidAuthorizationFormat)
        );
    }

    #[test]
    fn test_extract_api_key_preserves_trailing_segments() {
        assert_eq!(
            extract_api_key("Bearer ares_key extra"),
            Ok("ares_key extra")
        );
    }

    #[test]
    fn test_validate_api_key_format_accepts_ares_prefix() {
        assert!(validate_api_key_format("ares_live_abc123").is_ok());
    }

    #[test]
    fn test_validate_api_key_format_accepts_ares_only() {
        assert!(validate_api_key_format("ares_").is_ok());
    }

    #[test]
    fn test_validate_api_key_format_rejects_missing_prefix() {
        assert_eq!(
            validate_api_key_format("abc123"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_validate_api_key_format_rejects_wrong_prefix() {
        assert_eq!(
            validate_api_key_format("openai_sk_test"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_validate_api_key_format_rejects_ares_without_underscore() {
        assert_eq!(
            validate_api_key_format("aresabc"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_validate_api_key_format_rejects_uppercase_ares() {
        assert_eq!(
            validate_api_key_format("ARES_abc"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_validate_api_key_format_rejects_empty() {
        assert_eq!(
            validate_api_key_format(""),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_validate_api_key_format_rejects_leading_whitespace() {
        assert_eq!(
            validate_api_key_format(" ares_abc"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_validate_api_key_format_rejects_embedded_ares_prefix() {
        assert_eq!(
            validate_api_key_format("prefix_ares_abc"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_parse_bearer_api_key_success() {
        assert_eq!(
            parse_bearer_api_key("Bearer ares_valid_key"),
            Ok("ares_valid_key")
        );
    }

    #[test]
    fn test_parse_bearer_api_key_fails_on_bad_format() {
        assert_eq!(
            parse_bearer_api_key("Token ares_valid_key"),
            Err(ApiKeyAuthError::InvalidAuthorizationFormat)
        );
    }

    #[test]
    fn test_parse_bearer_api_key_fails_on_bad_prefix() {
        assert_eq!(
            parse_bearer_api_key("Bearer sk_test_key"),
            Err(ApiKeyAuthError::InvalidApiKeyFormat)
        );
    }

    #[test]
    fn test_check_quota_none_under_limits() {
        let ctx = TenantContext::new("t1".into(), TenantTier::Free);
        assert_eq!(check_quota(&ctx, 0, 0), None);
        assert_eq!(check_quota(&ctx, 999, 49), None);
    }

    #[test]
    fn test_check_quota_monthly_at_boundary() {
        let ctx = TenantContext::new("t1".into(), TenantTier::Free);
        assert_eq!(check_quota(&ctx, 1_000, 0), Some(QuotaExceeded::Monthly));
    }

    #[test]
    fn test_check_quota_daily_at_boundary() {
        let ctx = TenantContext::new("t1".into(), TenantTier::Free);
        assert_eq!(check_quota(&ctx, 0, 50), Some(QuotaExceeded::Daily));
    }

    #[test]
    fn test_check_quota_monthly_takes_precedence() {
        let ctx = TenantContext::new("t1".into(), TenantTier::Free);
        assert_eq!(check_quota(&ctx, 1_000, 50), Some(QuotaExceeded::Monthly));
    }

    #[test]
    fn test_check_quota_dev_tier_daily_boundary() {
        let ctx = TenantContext::new("dev".into(), TenantTier::Dev);
        assert_eq!(check_quota(&ctx, 0, 1_999), None);
        assert_eq!(check_quota(&ctx, 0, 2_000), Some(QuotaExceeded::Daily));
    }

    #[test]
    fn test_check_quota_enterprise_allows_large_usage() {
        let ctx = TenantContext::new("ent".into(), TenantTier::Enterprise);
        assert_eq!(check_quota(&ctx, 1_000_000, 1_000_000), None);
    }

    #[test]
    fn test_api_key_auth_error_messages() {
        assert_eq!(
            ApiKeyAuthError::MissingAuthorizationHeader.message(),
            "Missing Authorization header"
        );
        assert_eq!(
            ApiKeyAuthError::InvalidAuthorizationHeader.message(),
            "Invalid Authorization header"
        );
        assert_eq!(
            ApiKeyAuthError::InvalidAuthorizationFormat.message(),
            "Invalid Authorization format. Expected: Bearer ares_..."
        );
        assert_eq!(
            ApiKeyAuthError::InvalidApiKeyFormat.message(),
            "Invalid API key format. Must start with ares_"
        );
    }

    #[test]
    fn test_api_key_auth_error_status_codes() {
        assert_eq!(
            ApiKeyAuthError::MissingAuthorizationHeader.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            ApiKeyAuthError::InvalidApiKeyFormat.status_code(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn test_error_response_body_serde_roundtrip() {
        let body = ErrorResponseBody {
            error: "quota exceeded".to_string(),
        };
        let json = serde_json::to_string(&body).unwrap();
        let decoded: ErrorResponseBody = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn test_error_response_body_deserialize_from_json() {
        let decoded: ErrorResponseBody =
            serde_json::from_str(r#"{"error":"Invalid API key"}"#).unwrap();
        assert_eq!(decoded.error, "Invalid API key");
    }

    #[test]
    fn test_error_response_body_serialize_shape() {
        let body = ErrorResponseBody {
            error: "test".to_string(),
        };
        let value: serde_json::Value = serde_json::to_value(body).unwrap();
        assert_eq!(value["error"], "test");
    }

    #[tokio::test]
    async fn test_auth_error_response_missing_header() {
        let response = auth_error_response(ApiKeyAuthError::MissingAuthorizationHeader);
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_error_message(response).await,
            "Missing Authorization header"
        );
    }

    #[tokio::test]
    async fn test_quota_exceeded_response_monthly() {
        let response = quota_exceeded_response(QuotaExceeded::Monthly);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response_error_message(response).await,
            "Monthly request quota exceeded"
        );
    }

    #[tokio::test]
    async fn test_quota_exceeded_response_daily() {
        let response = quota_exceeded_response(QuotaExceeded::Daily);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response_error_message(response).await,
            "Daily rate limit exceeded"
        );
    }

    // --- integration middleware tests (existing, extended where noted) ---

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
        assert_eq!(
            response_error_message(response).await,
            "Missing Authorization header"
        );
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
        assert_eq!(
            response_error_message(response).await,
            "Invalid Authorization format. Expected: Bearer ares_..."
        );
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
        assert_eq!(
            response_error_message(response).await,
            "Invalid API key format. Must start with ares_"
        );
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
        assert_eq!(
            response_error_message(response).await,
            "Tenant database not configured"
        );
    }

    #[tokio::test]
    async fn test_middleware_valid_api_key_passes() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
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

        let db = Arc::new(ares_test_support::client().await);
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
        assert_eq!(response_error_message(response).await, "Invalid API key");
    }
    #[tokio::test]
    async fn test_middleware_monthly_quota_exceeded() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
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
        assert_eq!(
            response_error_message(response).await,
            "Monthly request quota exceeded"
        );
    }
    #[tokio::test]
    async fn test_middleware_event_denial_maps_to_quota_response() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
        let tenant_db = Arc::new(TenantDb::new(db));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-event-deny").await;
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());
        events.on("agent.admit".into(), |_payload| async {
            Ok::<_, cordis::CordisError>(serde_json::json!({ "deny": "monthly" }))
        });
        let app = build_app_with_context(tenant_db, ctx);

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
        assert_eq!(
            response_error_message(response).await,
            "Monthly request quota exceeded"
        );
    }

    #[tokio::test]
    async fn test_middleware_daily_quota_exceeded() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
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
        assert_eq!(
            response_error_message(response).await,
            "Daily rate limit exceeded"
        );
    }
    #[tokio::test]
    async fn test_middleware_invalid_auth_header_bytes() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
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
        assert_eq!(
            response_error_message(response).await,
            "Invalid Authorization header"
        );
    }
    #[tokio::test]
    async fn test_middleware_verify_api_key_db_error() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
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
        assert_eq!(
            response_error_message(response).await,
            "Failed to verify API key"
        );
        sqlx::query("ALTER TABLE api_keys_hidden RENAME TO api_keys")
            .execute(&db.pool)
            .await
            .expect("restore api_keys");
    }
    #[tokio::test]
    async fn test_middleware_monthly_usage_db_error() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        let (_tenant_id, api_key) = provision_tenant(&tenant_db, "auth-db-monthly").await;

        sqlx::query("ALTER TABLE monthly_usage_cache RENAME TO monthly_usage_cache_hidden")
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
        assert_eq!(
            response_error_message(response).await,
            "Failed to check usage"
        );
        sqlx::query("ALTER TABLE monthly_usage_cache_hidden RENAME TO monthly_usage_cache")
            .execute(&db.pool)
            .await
            .expect("restore monthly_usage_cache");
    }
    #[tokio::test]
    async fn test_middleware_daily_usage_db_error() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(ares_test_support::client().await);
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
        assert_eq!(
            response_error_message(response).await,
            "Failed to check rate limit"
        );
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

        let decoded: ErrorResponseBody = serde_json::from_slice(&body).unwrap();
        assert_eq!(decoded.error, "test message");
    }

    #[tokio::test]
    async fn test_error_response_internal_server_error_body() {
        let response = error_response(StatusCode::INTERNAL_SERVER_ERROR, "db unavailable");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response_error_message(response).await, "db unavailable");
    }
}
