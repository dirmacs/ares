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
    use axum::http::HeaderMap;

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
}
