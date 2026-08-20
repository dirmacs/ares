//! Admin billing domain — cordis Phase6 stub.
//! Decomposed from `admin.rs` (190KB/5946 lines). Real handlers remain in shim
//! `src/api/handlers/admin.rs` for one commit; this module will own the domain.

use axum::Router;

/// Route set for this domain.
pub type RouteSet = Router;

/// Stub routes for `billing` domain.
// TODO: ctx.plugin(AdminBillingRoutes, ...) — register via RegistryService
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(AdminBillingRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminBillingService;
// impl Service for AdminBillingService {
//     fn name(&self) -> &'static str { "admin_billing" }
//     fn check(&self) -> bool { true }
// }
