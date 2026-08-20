//! V1 agents domain — cordis Phase6 stub.
//! Decomposed from `v1.rs` (73KB). Real handler remains in shim.

use axum::Router;

pub type RouteSet = Router;

/// Stub routes for `v1::agents`.
 // TODO: ctx.plugin(V1AgentsRoutes, ...)
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(V1AgentsRoutes, ...) — Service stub
// use ares_cordis_core::Service;
// pub struct V1AgentsService;
// impl Service for V1AgentsService {
//     fn name(&self) -> &'static str { "v1_agents" }
//     fn check(&self) -> bool { true }
// }
