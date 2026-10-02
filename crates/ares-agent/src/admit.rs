use std::any::TypeId;
use std::sync::Arc;

use ares_types::models::{QuotaExceeded, TenantContext};
use ares_types::types::AppError;
use cordis::{Context, CordisError, EventsService};
use serde_json::Value;

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
pub fn quota_exceeded(tenant: &TenantContext, monthly: u64, daily: u64) -> Option<QuotaExceeded> {
    tenant.admit(monthly, daily).err()
}

/// What a paused tenant is told. It names the pause and nothing else about the tenant.
const TENANT_PAUSED_MESSAGE: &str = "Agent runs are paused for this tenant.";

/// Refuse a run for a paused tenant: the per-tenant kill switch, `tenants.paused`
/// (migration 037).
///
/// This is the one pause check. [`admit`] calls it, and so does the research handler, which
/// does not pass through `admit`.
///
/// - One read of `tenants.paused` per call, no cache and no restart: a pause binds the next
///   run.
/// - With no `TenantDb` on the context (a direct library context, or a build without
///   `postgres`) there is nothing to read and the check passes, the same fallback
///   `usage_counts` takes.
/// - A tenant with no `tenants` row reads as not paused (see
///   `ares_store::tenants::tenant_paused`), as `admit` already admits such a tenant.
/// - A failed read is returned as an error, so the run is refused.
///
/// The refusal is `AppError::Unavailable` (HTTP 503).
pub async fn ensure_tenant_not_paused(ctx: &Arc<Context>, tenant_id: &str) -> Result<(), AppError> {
    #[cfg(feature = "postgres")]
    {
        if let Some(db) = ctx.get::<ares_store::TenantDb>() {
            if ares_store::tenants::tenant_paused(db.pool(), tenant_id).await? {
                return Err(AppError::Unavailable(TENANT_PAUSED_MESSAGE.to_string()));
            }
        }
    }
    let _ = (ctx, tenant_id);
    Ok(())
}

/// The tenant a context is scoped to, for a context that carries no `TenantContext`.
///
/// Background runs (the scheduler, the triggers, the pipelines and the workflow engine) call
/// `Execute::run` on `tenant_scope(root, tenant)`. That scopes the context to the tenant but
/// never adds a `TenantContext` (`request_tenant_ctx` is the only place that does), so the only
/// trace of the tenant is the isolate label. What sets it:
///
/// - `TenantRealms::open` labels the realm with the bare tenant id
///   (`ares-store/src/realms.rs`, `isolate_type(self.tools, tenant_id)`);
/// - `tenant_scope` with no `TenantRealms` labels the `Tools` isolate with the bare tenant id
///   (`execution.rs`, `ctx.isolate::<ares_tools::Tools>(tenant_id)`);
/// - `request_user_scope` labels the `Tools` isolate `user:{id}` for a JWT user with no tenant
///   (`execution.rs`): that is a user, never a tenant, and gives `None`;
/// - a legacy `tenant:{id}` label (nothing sets it today) reads as `{id}`.
///
/// The labels are read in `user_id_from_ctx`'s order (the `Execute` isolate first, then the
/// `Tools` one) and the first non-empty one decides, so a context `user_id_from_ctx` reads as a
/// user is not read as a tenant here. A context with no label (a direct library context, the
/// root) names no tenant. A label that names no `tenants` row reads as not paused
/// ([`ensure_tenant_not_paused`]), so a wrong guess costs one read and never a refusal.
fn scoped_tenant_id(ctx: &Arc<Context>) -> Option<String> {
    for tid in [
        TypeId::of::<crate::Execute>(),
        TypeId::of::<ares_tools::Tools>(),
    ] {
        if let Some(label) = ctx.isolate_label(tid) {
            let label: &str = &label;
            if label.starts_with("user:") {
                return None;
            }
            let tenant_id = label.strip_prefix("tenant:").unwrap_or(label);
            if !tenant_id.is_empty() {
                return Some(tenant_id.to_string());
            }
        }
    }
    None
}

/// Shared quota gate used by `Execute::run` and protocol adapters.
///
/// A paused tenant is refused first ([`ensure_tenant_not_paused`]), before any usage read. The
/// tenant is the `TenantContext`'s when the context has one (the request paths). A background
/// run has none (`tenant_scope` isolates and never intercepts), so the tenant is the one the
/// context is scoped to ([`scoped_tenant_id`]): the pause binds a paused tenant's schedules,
/// triggers, pipelines and workflows too. A `user:` scope and an unscoped context name no
/// tenant and are not checked.
///
/// The pause is checked here and not in [`admit_with_details`]: that function's
/// [`AdmissionError`] is matched exhaustively by the API-key middleware, so a pause variant
/// would change it. The quota stays as it was: [`admit_with_details`] still sees only a
/// `TenantContext`, so a background run is paused-checked but not quota-checked.
///
/// The event is authoritative when an `EventsService` is available. The typed
/// `TenantContext::admit` check remains the final fallback, which keeps direct
/// library contexts safe when no event bus has been installed yet.
pub async fn admit(ctx: &Arc<Context>) -> Result<(), AppError> {
    if let Some(tc) = ctx.get::<TenantContext>() {
        ensure_tenant_not_paused(ctx, &tc.tenant_id).await?;
    } else if let Some(tenant_id) = scoped_tenant_id(ctx) {
        ensure_tenant_not_paused(ctx, &tenant_id).await?;
    }
    admit_with_details(ctx).await.map_err(Into::into)
}

/// Shared admission gate with enough detail for protocol-specific error maps.
pub async fn admit_with_details(ctx: &Arc<Context>) -> Result<(), AdmissionError> {
    let Some(tc) = ctx.get::<TenantContext>() else {
        return Ok(());
    };
    let (monthly, daily) = usage_counts(ctx, &tc.tenant_id).await?;
    if let Some(events) = ctx.get::<EventsService>() {
        let payload = cordis::AgentAdmitPayload {
            tenant_id: tc.tenant_id.clone(),
            monthly,
            daily,
            requests_per_month: Some(tc.quota.requests_per_month),
            requests_per_day: Some(tc.quota.requests_per_day),
            tier: tc.tier.as_str().to_string(),
        };
        let result = events
            .dispatch_typed::<cordis::AgentAdmitEvent>(&payload)
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

async fn usage_counts(ctx: &Arc<Context>, tenant_id: &str) -> Result<(u64, u64), AdmissionError> {
    #[cfg(feature = "postgres")]
    {
        if let Some(db) = ctx.get::<ares_store::TenantDb>() {
            let monthly = db.get_monthly_requests(tenant_id).await.map_err(|source| {
                AdmissionError::Usage {
                    period: UsagePeriod::Monthly,
                    source,
                }
            })?;
            let daily =
                db.get_daily_requests(tenant_id)
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
    use serde_json::json;

    fn free_tenant() -> TenantContext {
        TenantContext::new("acme".into(), TenantTier::Free)
    }

    fn ctx_with_deny(deny: &'static str) -> (Arc<Context>, Box<dyn cordis::Disposable>) {
        let root = Context::new_root();
        let events = root.provide(EventsService::new());
        let keep = events.on(
            cordis::events_catalog::ev::AGENT_ADMIT.to_string(),
            move |_payload| async move { Ok(json!({ "deny": deny })) },
        );
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
        assert!(matches!(err, AdmissionError::Quota(QuotaExceeded::Monthly)));
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

    // -- the tenant a context is scoped to (item 2.5a, fix round 1) ----------------------------

    #[test]
    fn a_tenant_scope_names_its_tenant_by_the_bare_label() {
        let root = Context::new_root();
        let scoped = root.isolate::<ares_tools::Tools>("acme");
        assert_eq!(scoped_tenant_id(&scoped).as_deref(), Some("acme"));
    }

    #[test]
    fn a_legacy_tenant_prefix_is_stripped_on_either_label() {
        let root = Context::new_root();
        let on_tools = root.isolate::<ares_tools::Tools>("tenant:acme");
        assert_eq!(scoped_tenant_id(&on_tools).as_deref(), Some("acme"));
        let on_execute = root.isolate::<crate::Execute>("tenant:legacy");
        assert_eq!(scoped_tenant_id(&on_execute).as_deref(), Some("legacy"));
    }

    #[test]
    fn a_user_scope_names_no_tenant() {
        let root = Context::new_root();
        let on_tools = root.isolate::<ares_tools::Tools>("user:u42");
        assert_eq!(scoped_tenant_id(&on_tools), None);
        let on_execute = root.isolate::<crate::Execute>("user:u42");
        assert_eq!(scoped_tenant_id(&on_execute), None);
    }

    #[test]
    fn an_unscoped_or_empty_context_names_no_tenant() {
        let root = Context::new_root();
        assert_eq!(scoped_tenant_id(&root), None);
        let empty = root.isolate::<ares_tools::Tools>("");
        assert_eq!(scoped_tenant_id(&empty), None);
        let empty_after_prefix = root.isolate::<ares_tools::Tools>("tenant:");
        assert_eq!(scoped_tenant_id(&empty_after_prefix), None);
    }

    /// The first label found decides, in `user_id_from_ctx`'s order: the `Execute` label before
    /// the `Tools` one.
    #[test]
    fn the_execute_label_is_read_before_the_tools_label() {
        let root = Context::new_root();
        let user_then_tenant = root
            .isolate::<crate::Execute>("user:u1")
            .isolate::<ares_tools::Tools>("acme");
        assert_eq!(scoped_tenant_id(&user_then_tenant), None);
        let tenant_then_user = root
            .isolate::<crate::Execute>("tenant:legacy")
            .isolate::<ares_tools::Tools>("user:u1");
        assert_eq!(
            scoped_tenant_id(&tenant_then_user).as_deref(),
            Some("legacy")
        );
    }

    /// For every tenant scope the tenant is the scope `user_id_from_ctx` reads, and a `user:`
    /// scope is the one place the two differ: that function returns the user's id, this one
    /// returns no tenant.
    #[test]
    fn it_agrees_with_user_id_from_ctx_except_for_user_labels() {
        let root = Context::new_root();
        for label in ["acme", "tenant:acme", "t-1_2"] {
            let scoped = root.isolate::<ares_tools::Tools>(label);
            assert_eq!(
                scoped_tenant_id(&scoped),
                Some(crate::user_id_from_ctx(&scoped, "")),
                "label {label:?}"
            );
        }
        let user = root.isolate::<ares_tools::Tools>("user:u42");
        assert_eq!(crate::user_id_from_ctx(&user, ""), "u42");
        assert_eq!(scoped_tenant_id(&user), None);
    }

    /// A request context (a scope and a `TenantContext`, as `request_tenant_ctx` builds it) is
    /// admitted as before. With no `TenantDb` there is no pause to read.
    #[tokio::test]
    async fn a_scoped_request_ctx_is_admitted_as_before() {
        let scoped = Context::new_root().isolate::<ares_tools::Tools>("acme");
        let ctx = scoped.with_intercept(free_tenant());
        assert!(ctx.get::<TenantContext>().is_some());
        admit(&ctx)
            .await
            .expect("with no TenantDb there is no pause to read, and the typed quota admits");
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
