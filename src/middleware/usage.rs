use crate::db::tenants::TenantDb;
use crate::models::TenantContext;
use axum::{
    body::Body,
    extract::Request,
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

pub async fn usage_tracking_middleware(
    req: Request,
    next: Next,
) -> Response {
    let extensions = req.extensions();
    let tenant_ctx: Option<&TenantContext> = extensions.get();
    let tenant_db: Option<Arc<TenantDb>> = extensions.get();

    let response = next.run(req).await;

    if let (Some(ctx), Some(db)) = (tenant_ctx, tenant_db) {
        let tenant_id = ctx.tenant_id.clone();
        let response_headers = response.headers().clone();

        tokio::spawn(async move {
            if let Err(e) = record_usage(&tenant_id, &response_headers, db.as_ref().await).await {
                tracing::error!("Failed to record usage: {}", e);
            }
        });
    }

    response
}

async fn record_usage(
    tenant_id: &str,
    headers: &HeaderMap,
    db: &TenantDb,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let input_tokens = extract_token_header(headers, "x-input-tokens")
        .or_else(|| extract_token_header(headers, "x-gpt-input-tokens"))
        .unwrap_or(0);

    let output_tokens = extract_token_header(headers, "x-output-tokens")
        .or_else(|| extract_token_header(headers, "x-gpt-output-tokens"))
        .unwrap_or(0);

    let total_tokens = input_tokens + output_tokens;

    let input_estimate = if total_tokens == 0 {
        estimate_tokens_from_headers(headers)
    } else {
        total_tokens
    };

    if input_estimate > 0 {
        if let Err(e) = db.record_usage_event(tenant_id, 1, input_estimate).await {
            tracing::error!("Failed to record usage event: {}", e);
        }
    }

    Ok(())
}

fn extract_token_header(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
}

fn estimate_tokens_from_headers(headers: &HeaderMap) -> u64 {
    if let Some(content_length) = headers.get("content-length") {
        if let Ok(cl) = content_length.to_str() {
            if let Ok(bytes) = cl.parse::<u64>() {
                return bytes / 4;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_token_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-input-tokens", "100".parse().unwrap());
        headers.insert("x-output-tokens", "50".parse().unwrap());

        assert_eq!(extract_token_header(&headers, "x-input-tokens"), Some(100));
        assert_eq!(extract_token_header(&headers, "x-output-tokens"), Some(50));
        assert_eq!(extract_token_header(&headers, "x-missing"), None);
    }

    #[test]
    fn test_estimate_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "400".parse().unwrap());

        assert_eq!(estimate_tokens_from_headers(&headers), 100);
    }
}
