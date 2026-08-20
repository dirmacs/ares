//! Admin mcp domain — cordis Phase6 stub.
//! Decomposed from `admin.rs` (190KB/5946 lines). Real handlers remain in shim
//! `src/api/handlers/admin.rs` for one commit; this module will own the domain.

use axum::Router;

/// Route set for this domain.
pub type RouteSet = Router;

/// Stub routes for `mcp` domain.
// TODO: ctx.plugin(AdminMcpRoutes, ...) — register via RegistryService
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(AdminMcpRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminMcpService;
// impl Service for AdminMcpService {
//     fn name(&self) -> &'static str { "admin_mcp" }
//     fn check(&self) -> bool { true }
// }
