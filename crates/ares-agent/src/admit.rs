use std::sync::Arc;

use ares_types::models::{QuotaExceeded, TenantContext};
use ares_types::types::AppError;
use cordis::{Context, Dispatch, EventsService, CordisError};
use serde_json::{json, Value};

/// Which usage query failed while preparing the admission payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsagePeriod {
    Monthly,
    Daily,
}

/// Failure from the shared admission gate.
#[derive(Debug)]
pub enum AdmissionError {
    Usage {
        period: UsagePeriod,
        source: AppError,
    },
    Event(CordisError),
    Quota(QuotaExceeded),
}

impl From<AdmissionError> for AppError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Usage { source, .. } => source,
            AdmissionError::Event(error) => {
                AppError::Internal(format!("admission event failed: {error}"))
            }
            AdmissionError::Quota(exceeded) => exceeded.into(),
        }
    }
}

/// Apply the final typed quota policy to a usage snapshot.
pub fn quota_exceeded(
    tenant: &TenantContext,
    monthly: u64,
    daily: u64,
) -> Option<QuotaExceeded> {
    tenant.admit(monthly, daily).err()
}

/// Shared quota gate used by `Execute::run` and protocol adapters.
///
/// The event is authoritative when an `EventsService` is available. The typed
/// `TenantContext::admit` check remains the final fallback, which keeps direct
/// library contexts safe when no event bus has been installed yet.
pub async fn admit(ctx: &Arc<Context>) -> Result<(), AppError> {
    admit_with_details(ctx).await.map_err(Into::into)
}

/// Shared admission gate with enough detail for protocol-specific error maps.
pub async fn admit_with_details(ctx: &Arc<Context>) -> Result<(), AdmissionError> {
    let Some(tc) = ctx.get::<TenantContext>() else {
        return Ok(());
    };
    let (monthly, daily) = usage_counts(ctx, &tc.tenant_id).await?;
    if let Some(events) = ctx.get::<EventsService>() {
        let payload = json!({
            "tenant_id": tc.tenant_id,
            "monthly": monthly,
            "daily": daily,
            "requests_per_month": tc.quota.requests_per_month,
            "requests_per_day": tc.quota.requests_per_day,
            "tier": tc.tier.as_str(),
        });
        let result = events
            .dispatch( cordis::events_catalog::ev::AGENT_ADMIT.to_string(), payload, Dispatch::Bail)
            .await
            .map_err(AdmissionError::Event)?;
        if let Some(err) = deny_from_bail(&result) {
            return Err(AdmissionError::Quota(err));
        }
    }
    quota_exceeded(&tc, monthly, daily)
        .map_or(Ok(()), |exceeded| Err(AdmissionError::Quota(exceeded)))
}

fn deny_from_bail(result: &Value) -> Option<QuotaExceeded> {
    let marker = result
        .get("deny")
        .and_then(|v| v.as_str())
        .or_else(|| result.get("error").and_then(|v| v.as_str()));
    match marker {
        Some("daily") => Some(QuotaExceeded::Daily),
        Some("monthly") | Some(_) => Some(QuotaExceeded::Monthly),
        None => None,
    }
}

async fn usage_counts(
    ctx: &Arc<Context>,
    tenant_id: &str,
) -> Result<(u64, u64), AdmissionError> {
    #[cfg(feature = "postgres")]
    {
        if let Some(db) = ctx.get::<ares_store::TenantDb>() {
            let monthly = db
                .get_monthly_requests(tenant_id)
                .await
                .map_err(|source| AdmissionError::Usage {
                    period: UsagePeriod::Monthly,
                    source,
                })?;
            let daily = db
                .get_daily_requests(tenant_id)
                .await
                .map_err(|source| AdmissionError::Usage {
                    period: UsagePeriod::Daily,
                    source,
                })?;
            return Ok((monthly, daily));
        }
    }
    let _ = (ctx, tenant_id);
    Ok((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::models::TenantTier;

    fn free_tenant() -> TenantContext {
        TenantContext::new("acme".into(), TenantTier::Free)
    }

    fn ctx_with_deny(deny: &'static str) -> (Arc<Context>, Box<dyn cordis::Disposable>) {
        let root = Context::new_root();
        let events = root.provide(EventsService::new());
        let keep = events.on( cordis::events_catalog::ev::AGENT_ADMIT.to_string(), move |_payload| async move {
            Ok(json!({ "deny": deny }))
        });
        let ctx = root.with_intercept(free_tenant());
        (ctx, keep)
    }

    #[tokio::test]
    async fn bail_deny_monthly_overrides_passing_typed_quota() {
        let tenant = free_tenant();
        assert!(
            tenant.admit(0, 0).is_ok(),
            "typed Free quota must pass at zero usage"
        );
        let (ctx, _keep) = ctx_with_deny("monthly");
        let err = admit_with_details(&ctx)
            .await
            .expect_err("event deny must win over typed pass");
        assert!(matches!(
            err,
            AdmissionError::Quota(QuotaExceeded::Monthly)
        ));
    }

    #[tokio::test]
    async fn bail_deny_daily_overrides_passing_typed_quota() {
        let tenant = free_tenant();
        assert!(
            tenant.admit(0, 0).is_ok(),
            "typed Free quota must pass at zero usage"
        );
        let (ctx, _keep) = ctx_with_deny("daily");
        let err = admit_with_details(&ctx)
            .await
            .expect_err("event deny must win over typed pass");
        assert!(matches!(err, AdmissionError::Quota(QuotaExceeded::Daily)));
    }

    #[tokio::test]
    async fn admit_without_events_uses_typed_fallback() {
        let ctx = Context::new_root().with_intercept(free_tenant());
        assert!(
            ctx.get::<EventsService>().is_none(),
            "this path must not install EventsService"
        );
        assert!(quota_exceeded(&free_tenant(), 0, 0).is_none());
        admit_with_details(&ctx)
            .await
            .expect("typed fallback admits Free quota at zero usage");
    }
}
