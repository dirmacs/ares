use std::sync::Arc;

use axum::{extract::State, response::IntoResponse};
use ares_cordis_core::Context;

/// Health check via Cordis `Context` — proves `State<Arc<Context>>` compiles.
///
/// Mirrors the existing `/health` handler but resolves services via `ctx.get::<...>()`
/// instead of receiving `AppState`. Kept alongside `health_check` for shim phase.
pub async fn health_context(State(ctx): State<Arc<Context>>) -> impl IntoResponse {
    // Prove `ctx.get` works for optional Cordis services; result unused — just type-checks.
    let _events = ctx.get::<ares_cordis_core::EventsService>();
    let _registry = ctx.get::<ares_cordis_core::RegistryService>();
    let _resolver = ctx.get::<crate::agents::resolver::AgentResolverService>();
    "OK"
}
