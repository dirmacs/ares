//! V1 stream domain — cordis Phase6 stub.
//! Decomposed from `v1.rs` (73KB). Real handler remains in shim.

use axum::Router;

pub type RouteSet = Router;

/// Stub routes for `v1::stream`.
 // TODO: ctx.plugin(V1StreamRoutes, ...)
pub fn routes() -> RouteSet {
    Router::new()
}

// TODO: ctx.plugin(V1StreamRoutes, ...) — Service stub
// use ares_cordis_core::Service;
// pub struct V1StreamService;
// impl Service for V1StreamService {
//     fn name(&self) -> &'static str { "v1_stream" }
//     fn check(&self) -> bool { true }
// }
