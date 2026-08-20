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

/// Stub unified implementation that delegates to the static registry first.
///
/// Future phases will extend `resolve`/`list` to check:
/// 1. `runtime` tenant-scoped (`get_for_tenant`)
/// 2. `runtime` fleet-scoped (`get`)
/// 3. `mcp` bridge (`McpRegistry` clients)
/// 4. `static_registry` (`ToolRegistry`)
///
/// For this commit the stub only delegates to `static_registry` to keep the
/// compile green while preserving the `ToolRegistry` shim (`pub use registry::ToolRegistry`).
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
    fn resolve(&self, name: &str, _tenant: Option<TenantId>) -> Option<Arc<dyn Tool>> {
        // Precedence comment for future extension:
        // tenant runtime → fleet runtime → MCP bridge → static
        // 1) if let Some(rt) = &self.runtime {
        //        if let Some(tid) = tenant.as_deref() {
        //            if let Some(tool) = rt.get_for_tenant(name, Some(tid)) { return Some(tool); }
        //        }
        //        if let Some(tool) = rt.get(name) { return Some(tool); }
        //    }
        // 2) if let Some(mcp) = &self.mcp { /* bridge lookup */ }
        // 3) static fallback:
        self.static_registry.get(name).cloned()
    }

    fn list(&self, _tenant: Option<TenantId>) -> Vec<ToolDefinition> {
        // Stub: delegate to static registry only.
        // Future: merge `runtime.get_tool_definitions_for_tenant` + MCP `list_tools`.
        self.static_registry.get_tool_definitions()
    }

    fn reload(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<(), CordisError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

// `Service` is implemented for `dyn ToolService` above so
// `ctx.get::<dyn ToolService>()` works when the service is provided as
// `Arc<dyn ToolService>` via `Context::provide`.
