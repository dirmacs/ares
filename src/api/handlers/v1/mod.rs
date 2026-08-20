// cordis Phase6: decomposed into v1/{chat,stream,agents} — 3 modules
//! V1 domain re-exports — cordis Phase6.
//! Real compilation uses `src/api/handlers/v1.rs` as entry (`#[path = "v1.rs"]`).
//! This file mirrors the `pub mod` declarations for tooling completeness.

pub mod chat;
pub mod stream;
pub mod agents;

pub use chat::routes as chat_routes;
pub use stream::routes as stream_routes;
pub use agents::routes as agents_routes;
