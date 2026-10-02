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

use ::cordis::Service;
