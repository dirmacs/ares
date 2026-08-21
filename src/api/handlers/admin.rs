// Admin handlers — decomposed via Cordis (Phase 6)
// Each domain lives in `admin/*.rs`; shared DTOs/helpers in `admin/shared.rs`.
// This shim re-exports domains and provides middleware + routing.

#[path = "admin/tenants.rs"] pub mod tenants;
#[path = "admin/agents.rs"] pub mod agents;
#[path = "admin/providers.rs"] pub mod providers;
#[path = "admin/tools.rs"] pub mod tools;
#[path = "admin/schedules.rs"] pub mod schedules;
#[path = "admin/triggers.rs"] pub mod triggers;
#[path = "admin/pipelines.rs"] pub mod pipelines;
#[path = "admin/billing.rs"] pub mod billing;
#[path = "admin/mcp.rs"] pub mod mcp;
#[path = "admin/fleet_secrets.rs"] pub mod fleet_secrets;
#[path = "admin/connectors.rs"] pub mod connectors;
#[path = "admin/health.rs"] pub mod health;
#[path = "admin/audit.rs"] pub mod audit;
#[path = "admin/shared.rs"] pub mod shared;

pub use tenants::*;
pub use agents::*;
pub use providers::*;
pub use tools::*;
pub use schedules::*;
pub use triggers::*;
pub use pipelines::*;
pub use billing::*;
pub use mcp::*;
pub use fleet_secrets::*;
pub use connectors::*;
pub use health::*;
pub use audit::*;


// Re-export shared DTOs/helpers so `use super::*;` in shards resolves.
pub use shared::*;

/// Extended JWT claims that include Eruka's roles map.
#[derive(Debug, Deserialize)]
struct AdminClaims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
    #[serde(default)]
    pub roles: HashMap<String, Vec<RoleEntry>>,
}

#[derive(Debug, Deserialize)]
struct RoleEntry {
    pub role: String,
    #[allow(dead_code)]
    pub resource_id: Option<String>,
}

/// Check if JWT claims have admin or super_admin role in any of: "admin", "ares", "eruka".
pub(crate) fn has_admin_role(claims: &AdminClaims) -> bool {
    for product in ["admin", "ares", "eruka"] {
        if let Some(entries) = claims.roles.get(product) {
            if entries
                .iter()
                .any(|e| matches!(e.role.as_str(), "admin" | "super_admin"))
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn admin_token_from_request(req: &axum::extract::Request) -> Option<String> {
    if let Some(token) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
    {
        return Some(token.to_string());
    }
    req.uri().query().and_then(|query| {
        query.split('&').find_map(|param| {
            let (key, value) = param.split_once('=')?;
            if key == "token" && !value.is_empty() {
                admin_percent_decode(value).ok()
            } else {
                None
            }
        })
    })
}


fn admin_percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_value(bytes[i+1]);
            let lo = hex_value(bytes[i+2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).map_err(|e| AppError::InvalidInput(format!("invalid utf8: {e}")))
}
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub async fn admin_middleware(req: axum::extract::Request, next: Next) -> Response {
    let admin_secret = std::env::var("ADMIN_API_KEY").ok();
    let header_secret = req
        .headers()
        .get("x-admin-secret")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if let (Some(expected), Some(given)) = (&admin_secret, &header_secret) {
        if expected == given {
            return next.run(req).await;
        }
    }
    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_default();
    if !jwt_secret.is_empty() {
        if let Some(token) = admin_token_from_request(&req) {
            let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
            validation.leeway = 60;
            if let Ok(data) = jsonwebtoken::decode::<AdminClaims>(
                token,
                &jsonwebtoken::DecodingKey::from_secret(jwt_secret.as_bytes()),
                &validation,
            ) {
                if has_admin_role(&data.claims) {
                    return next.run(req).await;
                }
            }
        }
    }
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .body(r#"{"error":"Admin access requires X-Admin-Secret header or JWT with admin role"}"#.into())
        .unwrap()
}

/// Merge all admin domain routers into one `Router<AppState>`.
pub fn admin_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .merge(tenants::routes())
        .merge(agents::routes())
        .merge(providers::routes())
        .merge(tools::routes())
        .merge(schedules::routes())
        .merge(triggers::routes())
        .merge(pipelines::routes())
        .merge(billing::routes())
        .merge(mcp::routes())
        .merge(fleet_secrets::routes())
        .merge(connectors::routes())
        .merge(health::routes())
        .merge(audit::routes())
}
