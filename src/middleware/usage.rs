use crate::db::tenants::TenantDb;
use axum::{extract::Request, middleware::Next, response::Response};
use std::sync::Arc;

pub async fn track_usage(req: Request, next: Next) -> Response {
    let tenant_id = req
        .extensions()
        .get::<crate::models::TenantContext>()
        .map(|c| c.tenant_id.clone());
    let tenant_db = req.extensions().get::<Arc<TenantDb>>().cloned();

    let response = next.run(req).await;

    if let (Some(tid), Some(db)) = (tenant_id, tenant_db) {
        let headers = response.headers().clone();
        let pool = db.pool().clone();
        tokio::spawn(async move {
            let _ = crate::middleware::usage::record_usage(&tid, &headers, &pool).await;
        });
    }

    response
}

/// Parsed metering fields from response headers (no database I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeteringSnapshot {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub token_count: i64,
    pub model_name: Option<String>,
    pub agent_name: Option<String>,
    pub provider_name: Option<String>,
}

/// Returns true when any metering header is present.
pub(crate) fn has_metering_headers(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key("x-input-tokens")
        || headers.contains_key("x-output-tokens")
        || headers.contains_key("x-model-name")
        || headers.contains_key("x-agent-name")
        || headers.contains_key("x-provider-name")
}

/// Parses metering headers into a snapshot, or `None` when no metering headers are set.
pub(crate) fn parse_metering_headers(
    headers: &axum::http::HeaderMap,
) -> Option<MeteringSnapshot> {
    if !has_metering_headers(headers) {
        return None;
    }

    let input_tokens: i64 = headers
        .get("x-input-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let output_tokens: i64 = headers
        .get("x-output-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    Some(MeteringSnapshot {
        input_tokens,
        output_tokens,
        token_count: input_tokens + output_tokens,
        model_name: headers
            .get("x-model-name")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string()),
        agent_name: headers
            .get("x-agent-name")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string()),
        provider_name: headers
            .get("x-provider-name")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string()),
    })
}

async fn record_usage(
    tenant_id: &str,
    headers: &axum::http::HeaderMap,
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(snapshot) = parse_metering_headers(headers) else {
        return Ok(());
    };

    // Record usage event.
    // Use runtime `sqlx::query` (not the `query!` macro) so downstream
    // crates don't need DATABASE_URL at compile time or a `.sqlx` cache.
    // Library crates that ship via crates.io cannot rely on a live DB
    // or bundled cache being available to their consumers.
    sqlx::query(
        "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'http', $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(1_i32)
    .bind(snapshot.token_count)
    .bind(snapshot.input_tokens)
    .bind(snapshot.output_tokens)
    .bind(snapshot.model_name)
    .bind(snapshot.agent_name)
    .bind(snapshot.provider_name)
    .bind(chrono::Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PostgresClient;
    use crate::models::{TenantContext, TenantTier};
    use axum::{
        body::Body,
        http::{HeaderMap, Request, StatusCode},
        middleware::Next,
        response::{IntoResponse, Response},
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

        sqlx::migrate!("./migrations")
            .run(&db.pool)
            .await
            .expect("Failed to run migrations on ares_test");

        db
    }

    async fn restore_test_schema(db: &PostgresClient) {
        sqlx::migrate!("./migrations")
            .run(&db.pool)
            .await
            .expect("restore schema");
    }

    async fn provision_tenant(tenant_db: &TenantDb, name: &str) -> String {
        let tenant = tenant_db
            .create_tenant(name.to_string(), TenantTier::Free)
            .await
            .expect("create tenant");
        tenant.id
    }

    #[test]
    fn has_metering_headers_false_when_empty() {
        assert!(!has_metering_headers(&HeaderMap::new()));
    }

    #[test]
    fn has_metering_headers_true_for_input_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "10".parse().unwrap());
        assert!(has_metering_headers(&headers));
    }

    #[test]
    fn has_metering_headers_true_for_output_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert("x-output-tokens", "5".parse().unwrap());
        assert!(has_metering_headers(&headers));
    }

    #[test]
    fn has_metering_headers_true_for_model_name() {
        let mut headers = HeaderMap::new();
        headers.insert("x-model-name", "gpt-4".parse().unwrap());
        assert!(has_metering_headers(&headers));
    }

    #[test]
    fn has_metering_headers_true_for_provider_name() {
        let mut headers = HeaderMap::new();
        headers.insert("x-provider-name", "openai".parse().unwrap());
        assert!(has_metering_headers(&headers));
    }

    #[test]
    fn parse_metering_headers_none_without_headers() {
        assert_eq!(parse_metering_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn parse_metering_headers_sums_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "12".parse().unwrap());
        headers.insert("x-output-tokens", "8".parse().unwrap());
        headers.insert("x-model-name", "gpt-4".parse().unwrap());

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.input_tokens, 12);
        assert_eq!(snapshot.output_tokens, 8);
        assert_eq!(snapshot.token_count, 20);
        assert_eq!(snapshot.model_name.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn parse_metering_headers_defaults_missing_numeric_fields_to_zero() {
        let mut headers = HeaderMap::new();
        headers.insert("x-agent-name", "router".parse().unwrap());

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.input_tokens, 0);
        assert_eq!(snapshot.output_tokens, 0);
        assert_eq!(snapshot.agent_name.as_deref(), Some("router"));
    }

    #[test]
    fn parse_metering_headers_invalid_numeric_values_default_to_zero() {
        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "not-a-number".parse().unwrap());
        headers.insert("x-output-tokens", "also-bad".parse().unwrap());
        headers.insert("x-provider-name", "anthropic".parse().unwrap());

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.input_tokens, 0);
        assert_eq!(snapshot.output_tokens, 0);
        assert_eq!(snapshot.token_count, 0);
        assert_eq!(snapshot.provider_name.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parse_metering_headers_includes_all_optional_fields() {
        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "1".parse().unwrap());
        headers.insert("x-output-tokens", "2".parse().unwrap());
        headers.insert("x-model-name", "m".parse().unwrap());
        headers.insert("x-agent-name", "a".parse().unwrap());
        headers.insert("x-provider-name", "p".parse().unwrap());

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.model_name.as_deref(), Some("m"));
        assert_eq!(snapshot.agent_name.as_deref(), Some("a"));
        assert_eq!(snapshot.provider_name.as_deref(), Some("p"));
    }

    #[tokio::test]
    async fn record_usage_inserts_event_when_metering_present() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = TenantDb::new(Arc::clone(&db));
        let tenant_id = provision_tenant(&tenant_db, "usage-record").await;

        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "3".parse().unwrap());
        headers.insert("x-output-tokens", "7".parse().unwrap());
        headers.insert("x-model-name", "test-model".parse().unwrap());

        record_usage(&tenant_id, &headers, &db.pool)
            .await
            .expect("record usage");

        let row = sqlx::query(
            "SELECT token_count, input_tokens, output_tokens, model_name FROM usage_events WHERE tenant_id = $1",
        )
        .bind(&tenant_id)
        .fetch_one(&db.pool)
        .await
        .expect("usage row");

        use sqlx::Row;
        let token_count: i64 = row.get(0);
        let input_tokens: i64 = row.get(1);
        let output_tokens: i64 = row.get(2);
        let model_name: Option<String> = row.get(3);

        assert_eq!(token_count, 10);
        assert_eq!(input_tokens, 3);
        assert_eq!(output_tokens, 7);
        assert_eq!(model_name.as_deref(), Some("test-model"));
    }
    #[tokio::test]
    async fn record_usage_noop_without_metering_headers() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = TenantDb::new(Arc::clone(&db));
        let tenant_id = provision_tenant(&tenant_db, "usage-noop").await;

        record_usage(&tenant_id, &HeaderMap::new(), &db.pool)
            .await
            .expect("noop");

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE tenant_id = $1")
                .bind(&tenant_id)
                .fetch_one(&db.pool)
                .await
                .expect("count");

        assert_eq!(count, 0);
    }
    async fn metering_handler() -> Response {
        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "4".parse().unwrap());
        headers.insert("x-output-tokens", "6".parse().unwrap());
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::OK;
        *response.headers_mut() = headers;
        response
    }

    #[tokio::test]
    async fn track_usage_records_metering_from_response_headers() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));
        let tenant_id = provision_tenant(&tenant_db, "usage-track").await;
        let ctx = TenantContext::new(tenant_id.clone(), TenantTier::Free);

        let app = Router::new()
            .route("/metered", get(metering_handler))
            .layer(axum::middleware::from_fn(track_usage))
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
            ));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metered")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE tenant_id = $1")
                .bind(&tenant_id)
                .fetch_one(&db.pool)
                .await
                .expect("count");

        assert_eq!(count, 1);
    }
    #[tokio::test]
    async fn track_usage_skips_when_tenant_context_missing() {
        let _db_guard = DB_TEST_LOCK.lock().await;

        let db = Arc::new(create_test_db().await);
        let tenant_db = Arc::new(TenantDb::new(db.clone()));

        let app = Router::new()
            .route("/metered", get(metering_handler))
            .layer(axum::middleware::from_fn(track_usage))
            .layer(axum::middleware::from_fn(
                move |mut req: Request<Body>, next: Next| {
                    let db = tenant_db.clone();
                    async move {
                        req.extensions_mut().insert(db);
                        next.run(req).await
                    }
                },
            ));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metered")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let sentinel = "00000000-0000-0000-0000-000000000099";
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE tenant_id = $1")
                .bind(sentinel)
                .fetch_one(&db.pool)
                .await
                .expect("count");

        assert_eq!(count, 0);
    }}

