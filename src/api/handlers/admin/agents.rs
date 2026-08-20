//! Admin agents domain — cordis Phase6 stub.
//! Decomposed from `admin.rs` (190KB/5946 lines). Real handlers remain in shim
//! `src/api/handlers/admin.rs` for one commit; this module will own the domain.

use axum::Router;

/// Route set for this domain.
pub type RouteSet = Router;

/// Stub routes for `agents` domain.
// TODO: ctx.plugin(AdminAgentsRoutes, ...) — register via RegistryService
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(AdminAgentsRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminAgentsService;
// impl Service for AdminAgentsService {
//     fn name(&self) -> &'static str { "admin_agents" }
//     fn check(&self) -> bool { true }
// }
