use ares_store::tenants::TenantDb;
use axum::{extract::Request, middleware::Next, response::Response};
use cordis::{Context, Service, ServiceInitFuture};
use parking_lot::Mutex;
use std::sync::Arc;

pub async fn track_usage(mut req: Request, next: Next) -> Response {
    let tenant_id = req
        .extensions()
        .get::<ares_types::models::TenantContext>()
        .map(|c| c.tenant_id.clone());
    let tenant_db = req.extensions().get::<Arc<TenantDb>>().cloned();

    let usage = tenant_id.as_ref().map(|tid| UsageContext::new(tid.clone()));
    if let Some(ref usage) = usage {
        req.extensions_mut().insert(usage.clone());
    }

    let response = next.run(req).await;

    if should_record_usage(tenant_id.as_deref(), tenant_db.is_some()) {
        if let Some(snapshot) = usage.as_ref().and_then(|u| u.snapshot()) {
            let tid = tenant_id.expect("checked above");
            let db = tenant_db.expect("checked above");
            let pool = db.pool().clone();
            let tenant_id = usage.as_ref().map(|u| u.tenant_id.clone()).unwrap_or(tid);
            tokio::spawn(async move {
                let _ = record_usage_params(&tenant_id, &snapshot, &pool).await;
            });
        }
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

/// Request-scoped usage metering, interceptable via Cordis `with_intercept`.
#[derive(Debug)]
pub struct UsageContext {
    pub tenant_id: String,
    snapshot: Arc<Mutex<Option<MeteringSnapshot>>>,
}

impl Clone for UsageContext {
    fn clone(&self) -> Self {
        Self {
            tenant_id: self.tenant_id.clone(),
            snapshot: Arc::clone(&self.snapshot),
        }
    }
}

impl UsageContext {
    pub fn new(tenant_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            snapshot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn record(&self, snapshot: MeteringSnapshot) {
        *self.snapshot.lock() = Some(snapshot);
    }

    pub fn snapshot(&self) -> Option<MeteringSnapshot> {
        self.snapshot.lock().clone()
    }
}

impl Service for UsageContext {
    fn name(&self) -> &'static str {
        "usage_context"
    }

    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }

    fn check(&self) -> bool {
        true
    }
}

/// Bind parameters for a usage_events INSERT (no database I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageEventParams {
    pub tenant_id: String,
    pub request_count: i32,
    pub token_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub model_name: Option<String>,
    pub agent_name: Option<String>,
    pub provider_name: Option<String>,
}

/// Returns true when tenant context and DB are both available for usage recording.
pub(crate) fn should_record_usage(tenant_id: Option<&str>, has_tenant_db: bool) -> bool {
    tenant_id.is_some() && has_tenant_db
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
pub(crate) fn parse_metering_headers(headers: &axum::http::HeaderMap) -> Option<MeteringSnapshot> {
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

/// Extracts metering data from response headers (alias for [`parse_metering_headers`]).
pub(crate) fn extract_metering_from_response(
    headers: &axum::http::HeaderMap,
) -> Option<MeteringSnapshot> {
    parse_metering_headers(headers)
}

/// Builds INSERT bind parameters from a tenant id and metering snapshot.
pub(crate) fn usage_event_params(tenant_id: &str, snapshot: &MeteringSnapshot) -> UsageEventParams {
    UsageEventParams {
        tenant_id: tenant_id.to_string(),
        request_count: 1,
        token_count: snapshot.token_count,
        input_tokens: snapshot.input_tokens,
        output_tokens: snapshot.output_tokens,
        model_name: snapshot.model_name.clone(),
        agent_name: snapshot.agent_name.clone(),
        provider_name: snapshot.provider_name.clone(),
    }
}

/// Reads intercepted [`UsageContext`] and maps its snapshot to INSERT params.
pub(crate) fn usage_event_params_from_ctx(ctx: &Arc<Context>) -> Option<UsageEventParams> {
    let usage = ctx.get::<UsageContext>()?;
    let snapshot = usage.snapshot()?;
    Some(usage_event_params(&usage.tenant_id, &snapshot))
}

async fn record_usage_params(
    tenant_id: &str,
    snapshot: &MeteringSnapshot,
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = usage_event_params(tenant_id, snapshot);

    // Record usage event.
    // Use runtime `sqlx::query` (not the `query!` macro) so downstream
    // crates don't need DATABASE_URL at compile time or a `.sqlx` cache.
    // Library crates that ship via crates.io cannot rely on a live DB
    // or bundled cache being available to their consumers.
    sqlx::query(
        "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'http', $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(params.tenant_id)
    .bind(params.request_count)
    .bind(params.token_count)
    .bind(params.input_tokens)
    .bind(params.output_tokens)
    .bind(params.model_name)
    .bind(params.agent_name)
    .bind(params.provider_name)
    .bind(chrono::Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                name.parse::<HeaderName>().expect("header name"),
                value.parse::<HeaderValue>().expect("header value"),
            );
        }
        headers
    }

    #[test]
    fn has_metering_headers_false_when_empty() {
        assert!(!has_metering_headers(&HeaderMap::new()));
    }

    #[test]
    fn has_metering_headers_true_for_input_tokens() {
        assert!(has_metering_headers(&headers_with(&[(
            "x-input-tokens",
            "10"
        )])));
    }

    #[test]
    fn has_metering_headers_true_for_output_tokens() {
        assert!(has_metering_headers(&headers_with(&[(
            "x-output-tokens",
            "5"
        )])));
    }

    #[test]
    fn has_metering_headers_true_for_model_name() {
        assert!(has_metering_headers(&headers_with(&[(
            "x-model-name",
            "gpt-4"
        )])));
    }

    #[test]
    fn has_metering_headers_true_for_agent_name() {
        assert!(has_metering_headers(&headers_with(&[(
            "x-agent-name",
            "router"
        )])));
    }

    #[test]
    fn has_metering_headers_true_for_provider_name() {
        assert!(has_metering_headers(&headers_with(&[(
            "x-provider-name",
            "openai"
        )])));
    }

    #[test]
    fn has_metering_headers_true_when_all_core_fields_present() {
        let headers = headers_with(&[
            ("x-input-tokens", "1"),
            ("x-model-name", "gpt-4"),
            ("x-output-tokens", "2"),
            ("x-provider-name", "openai"),
        ]);
        assert!(has_metering_headers(&headers));
    }

    #[test]
    fn parse_metering_headers_none_without_headers() {
        assert_eq!(parse_metering_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn parse_metering_headers_sums_tokens() {
        let headers = headers_with(&[
            ("x-input-tokens", "12"),
            ("x-output-tokens", "8"),
            ("x-model-name", "gpt-4"),
        ]);

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.input_tokens, 12);
        assert_eq!(snapshot.output_tokens, 8);
        assert_eq!(snapshot.token_count, 20);
        assert_eq!(snapshot.model_name.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn parse_metering_headers_defaults_missing_numeric_fields_to_zero() {
        let headers = headers_with(&[("x-agent-name", "router")]);

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.input_tokens, 0);
        assert_eq!(snapshot.output_tokens, 0);
        assert_eq!(snapshot.token_count, 0);
        assert_eq!(snapshot.agent_name.as_deref(), Some("router"));
    }

    #[test]
    fn parse_metering_headers_invalid_numeric_values_default_to_zero() {
        let headers = headers_with(&[
            ("x-input-tokens", "not-a-number"),
            ("x-output-tokens", "also-bad"),
            ("x-provider-name", "anthropic"),
        ]);

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.input_tokens, 0);
        assert_eq!(snapshot.output_tokens, 0);
        assert_eq!(snapshot.token_count, 0);
        assert_eq!(snapshot.provider_name.as_deref(), Some("anthropic"));
    }

    #[test]
    fn parse_metering_headers_includes_all_optional_fields() {
        let headers = headers_with(&[
            ("x-input-tokens", "1"),
            ("x-output-tokens", "2"),
            ("x-model-name", "m"),
            ("x-agent-name", "a"),
            ("x-provider-name", "p"),
        ]);

        let snapshot = parse_metering_headers(&headers).expect("snapshot");
        assert_eq!(snapshot.model_name.as_deref(), Some("m"));
        assert_eq!(snapshot.agent_name.as_deref(), Some("a"));
        assert_eq!(snapshot.provider_name.as_deref(), Some("p"));
    }

    #[test]
    fn extract_metering_from_response_delegates_to_parser() {
        let headers = headers_with(&[("x-input-tokens", "4"), ("x-output-tokens", "6")]);

        let snapshot = extract_metering_from_response(&headers).expect("snapshot");
        assert_eq!(snapshot.token_count, 10);
    }

    #[test]
    fn extract_metering_from_response_none_without_headers() {
        assert_eq!(extract_metering_from_response(&HeaderMap::new()), None);
    }

    #[test]
    fn should_record_usage_requires_tenant_and_db() {
        assert!(should_record_usage(Some("tenant-1"), true));
        assert!(!should_record_usage(None, true));
        assert!(!should_record_usage(Some("tenant-1"), false));
        assert!(!should_record_usage(None, false));
    }

    #[test]
    fn usage_event_params_maps_snapshot_fields() {
        let snapshot = MeteringSnapshot {
            input_tokens: 3,
            output_tokens: 7,
            token_count: 10,
            model_name: Some("test-model".to_string()),
            agent_name: Some("agent".to_string()),
            provider_name: Some("openai".to_string()),
        };

        let params = usage_event_params("tenant-abc", &snapshot);
        assert_eq!(params.tenant_id, "tenant-abc");
        assert_eq!(params.request_count, 1);
        assert_eq!(params.token_count, 10);
        assert_eq!(params.input_tokens, 3);
        assert_eq!(params.output_tokens, 7);
        assert_eq!(params.model_name.as_deref(), Some("test-model"));
        assert_eq!(params.agent_name.as_deref(), Some("agent"));
        assert_eq!(params.provider_name.as_deref(), Some("openai"));
    }

    #[test]
    fn usage_event_params_with_zero_tokens() {
        let snapshot = MeteringSnapshot {
            input_tokens: 0,
            output_tokens: 0,
            token_count: 0,
            model_name: None,
            agent_name: None,
            provider_name: Some("anthropic".to_string()),
        };

        let params = usage_event_params("t", &snapshot);
        assert_eq!(params.token_count, 0);
        assert_eq!(params.model_name, None);
        assert_eq!(params.provider_name.as_deref(), Some("anthropic"));
    }

    #[test]
    fn record_usage_early_return_when_no_metering() {
        let snapshot = parse_metering_headers(&HeaderMap::new());
        assert_eq!(snapshot, None);
    }

    #[test]
    fn usage_context_readable_via_cordis_intercept() {
        let root = cordis::Context::new_root();
        let child = root.with_intercept(UsageContext::new("acme"));
        let retrieved = child
            .get::<UsageContext>()
            .expect("intercept must make UsageContext readable");
        assert_eq!(retrieved.tenant_id, "acme");
        assert_eq!(retrieved.snapshot(), None);
    }

    #[test]
    fn usage_context_record_then_params_from_ctx() {
        let root = cordis::Context::new_root();
        let child = root.with_intercept(UsageContext::new("acme"));
        let usage = child
            .get::<UsageContext>()
            .expect("intercept must make UsageContext readable");
        usage.record(MeteringSnapshot {
            input_tokens: 3,
            output_tokens: 7,
            token_count: 10,
            model_name: Some("gpt".into()),
            agent_name: Some("bot".into()),
            provider_name: Some("openai".into()),
        });
        let params = usage_event_params_from_ctx(&child).expect("recorded snapshot");
        assert_eq!(params.tenant_id, "acme");
        assert_eq!(params.token_count, 10);
    }

    #[test]
    fn usage_event_params_from_ctx_none_without_intercept() {
        let root = cordis::Context::new_root();
        assert_eq!(usage_event_params_from_ctx(&root), None);
    }

    #[test]
    fn usage_context_clone_shares_snapshot() {
        let original = UsageContext::new("acme");
        let cloned = original.clone();
        original.record(MeteringSnapshot {
            input_tokens: 3,
            output_tokens: 7,
            token_count: 10,
            model_name: Some("gpt".into()),
            agent_name: Some("bot".into()),
            provider_name: Some("openai".into()),
        });
        let snapshot = cloned.snapshot().expect("clone must share snapshot");
        assert_eq!(snapshot.token_count, 10);
        assert_eq!(snapshot.input_tokens, 3);
        assert_eq!(snapshot.output_tokens, 7);
    }

    #[test]
    fn usage_event_params_from_ctx_sees_record_on_clone() {
        let original = UsageContext::new("acme");
        let cloned = original.clone();
        let root = cordis::Context::new_root();
        let child = root.with_intercept(cloned);
        original.record(MeteringSnapshot {
            input_tokens: 4,
            output_tokens: 6,
            token_count: 10,
            model_name: Some("gpt".into()),
            agent_name: Some("bot".into()),
            provider_name: Some("openai".into()),
        });
        let params = usage_event_params_from_ctx(&child).expect("intercepted clone sees record");
        assert_eq!(params.tenant_id, "acme");
        assert_eq!(params.token_count, 10);
    }

    #[test]
    fn track_usage_records_only_from_shared_snapshot() {
        let usage = UsageContext::new("acme");
        assert!(usage.snapshot().is_none());
        usage.record(MeteringSnapshot {
            input_tokens: 1,
            output_tokens: 2,
            token_count: 3,
            model_name: None,
            agent_name: None,
            provider_name: None,
        });
        let clone = usage.clone();
        assert_eq!(clone.snapshot().map(|s| s.token_count), Some(3));
    }
}
