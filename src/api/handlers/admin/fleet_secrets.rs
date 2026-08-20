//! Admin fleet_secrets domain — cordis Phase6 stub.
//! Decomposed from `admin.rs` (190KB/5946 lines). Real handlers remain in shim
//! `src/api/handlers/admin.rs` for one commit; this module will own the domain.

use axum::Router;

/// Route set for this domain.
pub type RouteSet = Router;

/// Stub routes for `fleet_secrets` domain.
// TODO: ctx.plugin(AdminFleetSecretsRoutes, ...) — register via RegistryService
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(AdminFleetSecretsRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminFleetSecretsService;
// impl Service for AdminFleetSecretsService {
//     fn name(&self) -> &'static str { "admin_fleet_secrets" }
//     fn check(&self) -> bool { true }
// }
