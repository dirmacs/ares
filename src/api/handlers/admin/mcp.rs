//! Admin mcp domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use axum::Json;
use sha2::Digest;

pub async fn runtime_tool_capabilities() -> Json<RuntimeToolCapabilitiesResponse> {
    Json(RuntimeToolCapabilitiesResponse {
        tool_types: vec!["http", "mcp", "script", "sql"],
    })
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/mcp/runtime_tool_capabilities", get(runtime_tool_capabilities))
}

// TODO: ctx.plugin(AdminMcpRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminMcpService;
// impl Service for AdminMcpService {
//     fn name(&self) -> &'static str { "admin_mcp" }
//     fn check(&self) -> bool { true }
// }