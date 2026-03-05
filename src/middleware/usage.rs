use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use crate::db::tenants::TenantDb;

pub async fn track_usage(
    req: Request,
    next: Next,
) -> Response {
    let tenant_id = req.extensions().get::<crate::models::TenantContext>().map(|c| c.tenant_id.clone());
    let tenant_db = req.extensions().get::<Arc<TenantDb>>().cloned();

    let response = next.run(req).await;

    if let (Some(tid), Some(db)) = (tenant_id, tenant_db) {
        let headers = response.headers().clone();
        tokio::spawn(async move {
            let _ = crate::middleware::usage::record_usage(&tid, &headers, db.as_ref()).await;
        });
    }

    response
}

async fn record_usage(
    tenant_id: &str,
    headers: &axum::http::HeaderMap,
    db: &TenantDb,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tokens = 0;
    if let Some(t) = headers.get("x-input-tokens").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i32>().ok()) { tokens += t; }
    if let Some(t) = headers.get("x-output-tokens").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i32>().ok()) { tokens += t; }
    
    db.record_usage_event(tenant_id, 1, tokens as u64).await?;
    Ok(())
}
