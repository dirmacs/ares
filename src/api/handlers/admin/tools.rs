//! Admin tools domain — cordis Phase6 stub.
//! Decomposed from `admin.rs` (190KB/5946 lines). Real handlers remain in shim
//! `src/api/handlers/admin.rs` for one commit; this module will own the domain.

use axum::Router;

/// Route set for this domain.
pub type RouteSet = Router;

/// Stub routes for `tools` domain.
// TODO: ctx.plugin(AdminToolsRoutes, ...) — register via RegistryService
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(AdminToolsRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminToolsService;
// impl Service for AdminToolsService {
//     fn name(&self) -> &'static str { "admin_tools" }
//     fn check(&self) -> bool { true }
// }
