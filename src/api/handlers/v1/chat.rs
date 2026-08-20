//! V1 chat domain — cordis Phase6 stub.
//! Decomposed from `v1.rs` (73KB). Real handler remains in shim.

use axum::Router;

pub type RouteSet = Router;

/// Stub routes for `v1::chat`.
 // TODO: ctx.plugin(V1ChatRoutes, ...)
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(V1ChatRoutes, ...) — Service stub
// use ares_cordis_core::Service;
// pub struct V1ChatService;
// impl Service for V1ChatService {
//     fn name(&self) -> &'static str { "v1_chat" }
//     fn check(&self) -> bool { true }
// }
