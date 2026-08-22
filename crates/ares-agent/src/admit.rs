use std::sync::Arc;

use ares_types::models::{QuotaExceeded, TenantContext};
use ares_types::types::AppError;
use cordis::{Context, Dispatch, EventsService};
use serde_json::{json, Value};

/// Shared quota gate used by `Execute::run` (and HTTP/MCP callers).
///
/// Missing `TenantContext` is a no-op (system / user-only isolate). Handlers on
/// `"agent.admit"` may extra-deny via `{ "deny": "monthly"|"daily" }`; a non-null
/// payload without that marker is not a deny (Bail with no handlers returns the
/// original payload).
pub async fn admit(ctx: &Arc<Context>) -> Result<(), AppError> {
    let Some(tc) = ctx.get::<TenantContext>() else {
        return Ok(());
    };
    let (monthly, daily) = usage_counts(ctx, &tc.tenant_id).await;
    if let Some(events) = ctx.get::<EventsService>() {
        let payload = json!({
            "tenant_id": tc.tenant_id,
            "monthly": monthly,
            "daily": daily,
            "requests_per_month": tc.quota.requests_per_month,
            "requests_per_day": tc.quota.requests_per_day,
            "tier": tc.tier.as_str(),
        });
        if let Ok(result) = events
            .dispatch("agent.admit".into(), payload, Dispatch::Bail)
            .await
        {
            if let Some(err) = deny_from_bail(&result) {
                return Err(err);
            }
        }
    }
    tc.admit(monthly, daily).map_err(AppError::from)
}

fn deny_from_bail(result: &Value) -> Option<AppError> {
    let marker = result
        .get("deny")
        .and_then(|v| v.as_str())
        .or_else(|| result.get("error").and_then(|v| v.as_str()));
    match marker {
        Some("daily") => Some(QuotaExceeded::Daily.into()),
        Some("monthly") | Some(_) => Some(QuotaExceeded::Monthly.into()),
        None => None,
    }
}

async fn usage_counts(ctx: &Arc<Context>, tenant_id: &str) -> (u64, u64) {
    #[cfg(feature = "postgres")]
    {
        if let Some(db) = ctx.get::<ares_store::TenantDb>() {
            let monthly = db.get_monthly_requests(tenant_id).await.unwrap_or(0);
            let daily = db.get_daily_requests(tenant_id).await.unwrap_or(0);
            return (monthly, daily);
        }
    }
    let _ = (ctx, tenant_id);
    (0, 0)
}
