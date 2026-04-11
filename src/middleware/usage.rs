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
	let token_count = input_tokens + output_tokens;

	let model_name: Option<String> = headers
		.get("x-model-name")
		.and_then(|v| v.to_str().ok())
		.map(|v| v.to_string());
	let agent_name: Option<String> = headers
		.get("x-agent-name")
		.and_then(|v| v.to_str().ok())
		.map(|v| v.to_string());
	let provider_name: Option<String> = headers
		.get("x-provider-name")
		.and_then(|v| v.to_str().ok())
		.map(|v| v.to_string());

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
	.bind(token_count)
	.bind(input_tokens)
	.bind(output_tokens)
	.bind(model_name)
	.bind(agent_name)
	.bind(provider_name)
	.bind(chrono::Utc::now().timestamp())
	.execute(pool)
	.await?;
	Ok(())
}
