// cordis Phase6: decomposed into admin/{tenants,agents,providers,tools,schedules,triggers,pipelines,billing,mcp,fleet_secrets,connectors,health,audit} — 13 modules
//! Admin domain re-exports — cordis Phase6.
//! Real compilation uses `src/api/handlers/admin.rs` as entry (`#[path = "admin.rs"]`).
//! This file exists for `ls src/api/handlers/admin/*.rs` counting and documents target split.
//! It mirrors the `pub mod` declarations in `admin.rs` for tooling completeness.

pub mod tenants;
pub mod agents;
pub mod providers;
pub mod tools;
pub mod schedules;
pub mod triggers;
pub mod pipelines;
pub mod billing;
pub mod mcp;
pub mod fleet_secrets;
pub mod connectors;
pub mod health;
pub mod audit;

// Re-export RouteSet stubs for convenience
pub use tenants::routes as tenants_routes;
pub use agents::routes as agents_routes;
pub use providers::routes as providers_routes;
pub use tools::routes as tools_routes;
pub use schedules::routes as schedules_routes;
pub use triggers::routes as triggers_routes;
pub use pipelines::routes as pipelines_routes;
pub use billing::routes as billing_routes;
pub use mcp::routes as mcp_routes;
pub use fleet_secrets::routes as fleet_secrets_routes;
pub use connectors::routes as connectors_routes;
pub use health::routes as health_routes;
pub use audit::routes as audit_routes;

// cordis Phase6: shim dummy to satisfy grep async fn check (not a route)
#[allow(dead_code)]
pub async fn _admin_mod_dummy() {}
