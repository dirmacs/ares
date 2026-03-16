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

async fn record_usage(
    tenant_id: &str,
    headers: &axum::http::HeaderMap,
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tokens = 0;
    if let Some(t) = headers
        .get("x-input-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i32>().ok())
    {
        tokens += t;
    }
    if let Some(t) = headers
        .get("x-output-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i32>().ok())
    {
        tokens += t;
    }

    // Record usage event
    sqlx::query!(
        "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, created_at) VALUES ($1, $2, 'http', $3, $4, $5)",
        uuid::Uuid::new_v4().to_string(),
        tenant_id,
        1,
        tokens as i64,
        chrono::Utc::now().timestamp()
    )
    .execute(pool)
    .await?;

    // Track first usage for DCRM stage updates
    crate::dsprint::stage_tracker::track_first_usage(tenant_id, pool).await;

    Ok(())
}
