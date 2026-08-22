//! Tools capability: tenant-aware tool resolution.
//!
//! Precedence: `tenant runtime → fleet runtime → static` (static includes
//! `mcp_bridge` registrations). Callers obtain the service via
//! `ctx.get::<Tools>()` and isolate with `ctx.isolate::<Tools>(tenant_id)`.

use std::any::TypeId;
use std::collections::HashSet;
use std::sync::Arc;

use ares_types::types::ToolDefinition;
use cordis::Service;

use crate::registry::{Tool, ToolRegistry};

#[cfg(any(feature = "postgres", test))]
use crate::runtime_registry::RuntimeToolRegistry;

/// Tenant-aware tool capability.
///
/// Isolate labels on [`Tools`] win over a `TenantContext` intercept.
pub struct Tools {
    static_registry: Arc<ToolRegistry>,
    #[cfg(any(feature = "postgres", test))]
    runtime: Option<Arc<RuntimeToolRegistry>>,
}

impl Tools {
    pub(crate) fn new(static_registry: Arc<ToolRegistry>) -> Self {
        Self {
            static_registry,
            #[cfg(any(feature = "postgres", test))]
            runtime: None,
        }
    }

    #[cfg(any(feature = "postgres", test))]
    pub(crate) fn with_runtime(
        static_registry: Arc<ToolRegistry>,
        runtime: Option<Arc<RuntimeToolRegistry>>,
    ) -> Self {
        Self {
            static_registry,
            runtime,
        }
    }

    /// Build Tools from a static tool set. Runtime is unset.
    pub fn from_static(tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        let mut registry = ToolRegistry::new();
        for tool in tools {
            registry.register(tool);
        }
        Self::new(Arc::new(registry))
    }

    /// Resolve a tool using the tenant derived from `ctx` (isolate, then intercept).
    pub fn resolve(&self, ctx: &Arc<cordis::Context>, name: &str) -> Option<Arc<dyn Tool>> {
        let tenant = tenant_id_from_tool_ctx(ctx);
        self.resolve_named(name, tenant.as_deref())
    }

    /// List tools using the tenant derived from `ctx` (isolate, then intercept).
    pub fn list(&self, ctx: &Arc<cordis::Context>) -> Vec<ToolDefinition> {
        let tenant = tenant_id_from_tool_ctx(ctx);
        self.list_named(tenant.as_deref())
    }

    fn resolve_named(&self, name: &str, tenant: Option<&str>) -> Option<Arc<dyn Tool>> {
        #[cfg(any(feature = "postgres", test))]
        if let Some(rt) = &self.runtime {
            if let Some(tid) = tenant {
                if let Some(tool) = rt.get_for_tenant(name, Some(tid)) {
                    return Some(tool);
                }
            }
            if let Some(tool) = rt.get(name) {
                return Some(tool);
            }
        }
        #[cfg(not(any(feature = "postgres", test)))]
        let _ = tenant;
        self.static_registry.get(name).cloned()
    }

    fn list_named(&self, tenant: Option<&str>) -> Vec<ToolDefinition> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<ToolDefinition> = Vec::new();

        let push_defs =
            |defs: Vec<ToolDefinition>, seen: &mut HashSet<String>, out: &mut Vec<ToolDefinition>| {
                for d in defs {
                    if seen.insert(d.name.clone()) {
                        out.push(d);
                    }
                }
            };

        #[cfg(any(feature = "postgres", test))]
        if let Some(rt) = &self.runtime {
            push_defs(
                rt.get_tool_definitions_for_tenant(tenant),
                &mut seen,
                &mut out,
            );
            if tenant.is_some() {
                let remaining: Vec<ToolDefinition> = rt
                    .get_tool_definitions()
                    .into_iter()
                    .filter(|d| !seen.contains(&d.name))
                    .collect();
                push_defs(remaining, &mut seen, &mut out);
            }
        }
        #[cfg(not(any(feature = "postgres", test)))]
        let _ = tenant;

        push_defs(
            self.static_registry.get_tool_definitions(),
            &mut seen,
            &mut out,
        );
        out
    }
}

impl Service for Tools {
    fn check(&self) -> bool {
        true
    }
}

/// Derive the tenant id for tool resolution from `ctx`.
///
/// Isolate labels on [`Tools`] win. A leading `tenant:` or `user:` prefix is
/// stripped; a non-empty remainder is the tenant. If the isolate label is
/// missing or empty after stripping, fall back to a `TenantContext` intercept.
/// Unlabeled contexts with no intercept yield `None`.
fn tenant_id_from_tool_ctx(ctx: &Arc<cordis::Context>) -> Option<String> {
    if let Some(label) = ctx.isolate_label(TypeId::of::<Tools>()) {
        let trimmed = label
            .strip_prefix("tenant:")
            .or_else(|| label.strip_prefix("user:"))
            .unwrap_or(&label);
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    ctx.get::<ares_types::models::TenantContext>()
        .map(|tc| tc.tenant_id.clone())
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::models::{TenantContext, TenantTier};
    use cordis::Context;

    #[test]
    fn unlabeled_root_yields_no_tenant() {
        let ctx = Context::new_root();
        assert_eq!(tenant_id_from_tool_ctx(&ctx), None);
    }

    #[test]
    fn intercept_tenant_context_yields_acme() {
        let ctx = Context::new_root()
            .with_intercept(TenantContext::new("acme".into(), TenantTier::Pro));
        assert_eq!(tenant_id_from_tool_ctx(&ctx).as_deref(), Some("acme"));
    }

    #[test]
    fn isolate_wins_over_intercept() {
        let ctx = Context::new_root()
            .with_intercept(TenantContext::new("acme".into(), TenantTier::Pro))
            .isolate::<Tools>("tenant:iso");
        assert_eq!(tenant_id_from_tool_ctx(&ctx).as_deref(), Some("iso"));
    }

    #[test]
    fn resolve_missing_tool_is_none() {
        let svc = Tools::new(Arc::new(ToolRegistry::new()));
        let ctx = Context::new_root();
        assert!(svc.resolve(&ctx, "missing").is_none());
        assert!(svc.list(&ctx).is_empty());
    }

    #[test]
    fn list_and_resolve_use_ctx_isolate() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(crate::calculator::Calculator));
        let svc = Tools::with_runtime(Arc::new(registry), None);
        let ctx = Context::new_root().isolate::<Tools>("tenant:acme");
        assert!(svc.resolve(&ctx, "calculator").is_some());
        assert!(svc.list(&ctx).iter().any(|d| d.name == "calculator"));
        assert!(svc.resolve(&ctx, "unknown").is_none());
    }

    #[test]
    fn from_static_resolves_calculator() {
        let svc = Tools::from_static([Arc::new(crate::calculator::Calculator) as Arc<dyn Tool>]);
        let ctx = Context::new_root();
        assert!(svc.resolve(&ctx, "calculator").is_some());
    }
}
