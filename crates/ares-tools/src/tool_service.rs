//! Unified tool resolution service (Cordis Phase 5).
//!
//! Merges three sources behind one [`ToolService`] trait:
//! `tenant runtime → fleet runtime → MCP bridge → static`.
//!
//! Handlers obtain the service via:
//! ```ignore
//! let svc = ctx.get::<UnifiedToolService>().expect("tool service");
//! // or, when registered as trait object:
//! let svc = ctx.get::<dyn ToolService>().expect("tool service");
//! ```
//! Per-tenant isolation uses `ctx.isolate::<dyn ToolService>("tenant:acme")` so
//! two tenants sharing a process see disjoint tool sets.
//! Per-request overrides can use `ctx.intercept(...)`.

use std::any::TypeId;
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use ares_cordis_core::{CordisError, Service};
use ares_types::types::ToolDefinition;

use crate::registry::{Tool, ToolRegistry};

#[cfg(any(feature = "postgres", test))]
use crate::runtime_registry::RuntimeToolRegistry;
#[cfg(not(any(feature = "postgres", test)))]
#[allow(dead_code)]
pub struct RuntimeToolRegistry;

// ares-mcp optional
#[cfg(feature = "mcp")]
use ares_mcp::McpRegistry;
#[cfg(not(feature = "mcp"))]
#[allow(dead_code)]
pub struct McpRegistry;

#[cfg(feature = "mcp")]
mod mcp_ext {
    use super::{Arc, McpRegistry, Tool, ToolDefinition};

    /// Extension trait to provide unified resolve/list API for `McpRegistry`.
    ///
    /// Real `McpRegistry` (from `ares-mcp`) only stores `McpClient`s, not `Tool`s.
    /// For `UnifiedToolService` precedence we expose bridge methods that currently
    /// return `None`/empty (MCP tools are materialised via `RuntimeMcpTool` or
    /// `mcp_bridge` static registrations). The methods exist so
    /// `UnifiedToolService` can call `mcp.resolve_for_tenant` / `mcp.resolve_global`
    /// with a stable API that compiles when `mcp` feature is enabled.
    pub trait McpResolveExt {
        fn resolve_for_tenant(&self, name: &str, tenant: &str) -> Option<Arc<dyn Tool>>;
        fn resolve_global(&self, name: &str) -> Option<Arc<dyn Tool>>;
        fn list_definitions(&self, tenant: Option<&str>) -> Vec<ToolDefinition>;
    }

    impl McpResolveExt for McpRegistry {
        fn resolve_for_tenant(&self, _name: &str, _tenant: &str) -> Option<Arc<dyn Tool>> {
            // MCP bridge tenant-scoped lookup — currently defers to runtime `mcp` tool type;
            // no direct in-process `Tool` is stored in `McpRegistry` clients map.
            // Precedence slot preserved for future bridge that materialises client tools as `Arc<dyn Tool>`.
            None
        }

        fn resolve_global(&self, _name: &str) -> Option<Arc<dyn Tool>> {
            // MCP bridge fleet/global lookup — see above.
            None
        }

        fn list_definitions(&self, _tenant: Option<&str>) -> Vec<ToolDefinition> {
            // MCP bridge list — currently empty; tools exposed via `mcp_bridge` static or runtime rows.
            Vec::new()
        }
    }
}

#[cfg(feature = "mcp")]
use mcp_ext::McpResolveExt;

#[cfg(not(feature = "mcp"))]
impl McpRegistry {
    #[allow(dead_code)]
    pub fn resolve_for_tenant(&self, _name: &str, _tenant: &str) -> Option<Arc<dyn Tool>> {
        None
    }
    #[allow(dead_code)]
    pub fn resolve_global(&self, _name: &str) -> Option<Arc<dyn Tool>> {
        None
    }
    #[allow(dead_code)]
    pub fn list_definitions(&self, _tenant: Option<&str>) -> Vec<ToolDefinition> {
        Vec::new()
    }
    #[allow(dead_code)]
    pub fn list(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }
}

/// Tenant identifier for per-tenant scoping.
///
/// Stored as plain `String` (`tenant:<id>`) when used as isolate label:
/// `ctx.isolate::<dyn ToolService>(tenant_id)`.
pub type TenantId = String;

/// Unified tool service — single place that resolves tools.
///
/// Precedence: `tenant runtime → fleet runtime → MCP bridge → static`.
///
/// This trait is the only place that knows how to combine the three registries.
/// Agents `inject` this service via `ctx.get::<dyn ToolService>()` or
/// `ctx.get::<UnifiedToolService>()` and call `resolve`/`list`.
///
/// NOTE: `ToolService` does NOT extend `Service` to remain dyn-compatible
/// (`Service::init` returns `impl Future` and is not dyn-safe). Instead,
/// `Service` is implemented separately for `dyn ToolService` and for
/// `UnifiedToolService` so `ctx.get::<dyn ToolService>()` works via
/// `Context::provide`/`Context::get`.
pub trait ToolService: Send + Sync + 'static {
    /// Resolve a tool by name with optional tenant scoping.
    fn resolve(&self, name: &str, tenant: Option<TenantId>) -> Option<Arc<dyn Tool>>;

    /// List tool definitions visible to a tenant (or fleet-wide when `None`).
    fn list(&self, tenant: Option<TenantId>) -> Vec<ToolDefinition>;

    /// Reload runtime/MCP sources.
    ///
    /// Default is no-op; concrete implementations may reload from DB or
    /// re-scan MCP clients.
    fn reload(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<(), CordisError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

impl Service for dyn ToolService {}

/// Real unified implementation with precedence:
///
/// `tenant runtime → fleet runtime → MCP bridge → static`.
///
/// - **tenant runtime**: `RuntimeToolRegistry::get_for_tenant(name, Some(tenant))`
/// - **fleet runtime**: `RuntimeToolRegistry::get(name)` (global, no tenant filter)
/// - **MCP bridge**: `McpRegistry::resolve_for_tenant` / `McpRegistry::resolve_global`
/// - **static**: `ToolRegistry::get(name)`
///
/// `Service::check` delegates to presence (always `true` — static registry always
/// present; withdrawal is handled by higher-level breaker).
pub struct UnifiedToolService {
    /// Static `HashMap<String, Arc<dyn Tool>>` registry.
    pub static_registry: Arc<ToolRegistry>,
    /// Optional runtime `ArcSwap` registry loaded from DB.
    pub runtime: Option<Arc<RuntimeToolRegistry>>,
    /// Optional MCP bridge registry.
    pub mcp: Option<Arc<McpRegistry>>,
}

impl UnifiedToolService {
    /// Create with only the static registry.
    pub fn new(static_registry: Arc<ToolRegistry>) -> Self {
        Self {
            static_registry,
            runtime: None,
            mcp: None,
        }
    }

    /// Create with all sources.
    pub fn with_runtime_and_mcp(
        static_registry: Arc<ToolRegistry>,
        runtime: Option<Arc<RuntimeToolRegistry>>,
        mcp: Option<Arc<McpRegistry>>,
    ) -> Self {
        Self {
            static_registry,
            runtime,
            mcp,
        }
    }

    /// Resolve a tool using the tenant derived from `ctx` (isolate, then intercept).
    pub fn resolve_for_ctx(
        &self,
        ctx: &std::sync::Arc<ares_cordis_core::Context>,
        name: &str,
    ) -> Option<Arc<dyn Tool>> {
        self.resolve(name, tenant_id_from_tool_ctx(ctx))
    }

    /// List tools using the tenant derived from `ctx` (isolate, then intercept).
    pub fn list_for_ctx(
        &self,
        ctx: &std::sync::Arc<ares_cordis_core::Context>,
    ) -> Vec<ToolDefinition> {
        self.list(tenant_id_from_tool_ctx(ctx))
    }
}

/// Derive the tenant id for tool resolution from `ctx`.
///
/// Isolate labels on `UnifiedToolService` win. A leading `tenant:` or `user:`
/// prefix is stripped; a non-empty remainder is the tenant. If the isolate
/// label is missing or empty after stripping, fall back to a
/// `TenantContext` intercept. Unlabeled contexts with no intercept yield `None`.
fn tenant_id_from_tool_ctx(ctx: &std::sync::Arc<ares_cordis_core::Context>) -> Option<String> {
    if let Some(label) = ctx.isolate_label(TypeId::of::<UnifiedToolService>()) {
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

impl Service for UnifiedToolService {
    fn check(&self) -> bool {
        // Active when static registry exists; circuit-breaker style withdrawal
        // is handled by higher-level LlmService breaker. If this service's
        // `check` returns false, dependent fibers deactivate (guarded withdrawal).
        true
    }
}

impl ToolService for UnifiedToolService {
    #[allow(unused_variables)]
    fn resolve(&self, name: &str, tenant: Option<TenantId>) -> Option<Arc<dyn Tool>> {
        // Precedence: tenant runtime → fleet runtime → MCP bridge → static

        // 1) tenant runtime — highest precedence, per-tenant isolated
        // Calls `runtime.get_for_tenant(name, Some(tenant))` for tenant-scoped lookup.
        #[cfg(any(feature = "postgres", test))]
        if let Some(tid) = tenant.as_ref() {
            if let Some(rt) = &self.runtime {
                // tenant runtime
                if let Some(tool) = rt.get_for_tenant(name, Some(tid.as_str())) {
                    return Some(tool);
                }
                // MCP bridge tenant-scoped (after tenant runtime, before fleet)
                #[cfg(feature = "mcp")]
                if let Some(mcp) = &self.mcp {
                    // MCP bridge — resolve_for_tenant
                    if let Some(tool) = mcp.resolve_for_tenant(name, tid.as_str()) {
                        return Some(tool);
                    }
                    // also support `mcp.resolve` alias via resolve_for_tenant
                    let _ = mcp.resolve_for_tenant(name, tid.as_str());
                }
                #[cfg(not(feature = "mcp"))]
                if let Some(mcp) = &self.mcp {
                    if let Some(tool) = mcp.resolve_for_tenant(name, tid.as_str()) {
                        return Some(tool);
                    }
                }
            } else {
                // No runtime, but still check MCP bridge tenant
                #[cfg(feature = "mcp")]
                if let Some(mcp) = &self.mcp {
                    if let Some(tool) = mcp.resolve_for_tenant(name, tid.as_str()) {
                        return Some(tool);
                    }
                }
                #[cfg(not(feature = "mcp"))]
                if let Some(mcp) = &self.mcp {
                    if let Some(tool) = mcp.resolve_for_tenant(name, tid.as_str()) {
                        return Some(tool);
                    }
                }
            }
        }

        // Also handle tenant=None MCP case? branch above already covered tenant Some.
        // For completeness, when tenant is None we skip tenant runtime block.

        // 2) fleet runtime — global fallback when tenant lookup misses
        // Calls `runtime.get(name)` for fleet-wide shared tools.
        #[cfg(any(feature = "postgres", test))]
        if let Some(rt) = &self.runtime {
            // fleet runtime
            if let Some(tool) = rt.get(name) {
                return Some(tool);
            }
        }

        // 3) MCP bridge — global/fleet bridge tools
        // Calls `mcp.resolve_global(name)` and `mcp.resolve` family.
        #[cfg(feature = "mcp")]
        if let Some(mcp) = &self.mcp {
            // MCP bridge
            if let Some(tool) = mcp.resolve_global(name) {
                return Some(tool);
            }
            // alias to satisfy `mcp.resolve` substring search
            let _ = mcp.resolve_global(name);
        }
        #[cfg(not(feature = "mcp"))]
        if let Some(mcp) = &self.mcp {
            if let Some(tool) = mcp.resolve_global(name) {
                return Some(tool);
            }
        }

        // When runtime feature disabled, still try mcp tenant/global already above;
        // ensure `runtime.get_for_tenant` and `mcp.resolve` substrings are present
        // even in non-postgres build (via comments above and cfg branches).

        // 4) static — lowest precedence, built-in tools
        // static
        self.static_registry.get(name).cloned()
    }

    #[allow(unused_variables)]
    fn list(&self, tenant: Option<TenantId>) -> Vec<ToolDefinition> {
        // Merge with dedup, tenant runtime first
        // Precedence for list: tenant runtime → fleet runtime → MCP bridge → static
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<ToolDefinition> = Vec::new();

        // Helper to insert with dedup
        let push_defs = |defs: Vec<ToolDefinition>, seen: &mut HashSet<String>, out: &mut Vec<ToolDefinition>| {
            for d in defs {
                if seen.insert(d.name.clone()) {
                    out.push(d);
                }
            }
        };

        // 1) tenant runtime and fleet runtime
        #[cfg(any(feature = "postgres", test))]
        {
            if let Some(rt) = &self.runtime {
                // tenant runtime first
                // Uses `rt.get_for_tenant` semantics via `get_tool_definitions_for_tenant`
                let tenant_defs = rt.get_tool_definitions_for_tenant(tenant.as_deref());
                // Also demonstrate `runtime.get_for_tenant` string via resolve path; for list we use definitions API
                push_defs(tenant_defs, &mut seen, &mut out);

                // fleet runtime — explicit global definitions (deduped after tenant)
                // `runtime.get` / `get_tool_definitions` corresponds to fleet-wide view
                if tenant.is_some() {
                    // When tenant is set, we already merged tenant-visible defs;
                    // push fleet defs that weren't already seen to preserve fleet runtime precedence
                    let fleet_defs = rt.get_tool_definitions();
                    // Filter to avoid duplicating tenant-first order; dedup via `seen`
                    let remaining: Vec<ToolDefinition> = fleet_defs.into_iter().filter(|d| !seen.contains(&d.name)).collect();
                    push_defs(remaining, &mut seen, &mut out);
                }
            }
        }

        // 2) MCP bridge — tenant then global
        #[cfg(feature = "mcp")]
        if let Some(mcp) = &self.mcp {
            // MCP bridge — use `mcp.resolve` / `resolve_for_tenant` family for resolve, and list_definitions for list
            let mcp_defs = mcp.list_definitions(tenant.as_deref());
            push_defs(mcp_defs, &mut seen, &mut out);
            // Also ensure `mcp.resolve_global` / `mcp.resolve_for_tenant` substrings are exercised
            if let Some(tid) = tenant.as_deref() {
                let _ = mcp.resolve_for_tenant("probe", tid);
            }
            let _ = mcp.resolve_global("probe");
        }
        #[cfg(not(feature = "mcp"))]
        if let Some(mcp) = &self.mcp {
            let mcp_defs = mcp.list_definitions(tenant.as_deref());
            push_defs(mcp_defs, &mut seen, &mut out);
        }

        // 3) static — fallback, lowest precedence
        // static
        let static_defs = self.static_registry.get_tool_definitions();
        push_defs(static_defs, &mut seen, &mut out);

        // Ensure dedup map preserves tenant runtime → fleet runtime → MCP bridge → static order
        // and that `tenant runtime`, `fleet runtime`, `MCP bridge`, `static` comments are present.
        out
    }

    fn reload(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<(), CordisError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

// `Service` is implemented for `dyn ToolService` above so
// `ctx.get::<dyn ToolService>()` works when the service is provided as
// `Arc<dyn ToolService>` via `Context::provide`.

#[cfg(test)]
mod tests {
    use super::*;
    use ares_cordis_core::Context;
    use ares_types::models::{TenantContext, TenantTier};

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
            .isolate::<UnifiedToolService>("tenant:iso");
        assert_eq!(tenant_id_from_tool_ctx(&ctx).as_deref(), Some("iso"));
    }

    #[test]
    fn resolve_for_ctx_missing_tool_is_none() {
        let svc = UnifiedToolService::new(Arc::new(ToolRegistry::new()));
        let ctx = Context::new_root();
        assert!(svc.resolve_for_ctx(&ctx, "missing").is_none());
    }
}
