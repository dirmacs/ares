use crate::HttpError;
use crate::Result;
use ::cordis::Context;
use ares_types::types::AppError;
use std::sync::Arc;
// Admin handlers — decomposed via Cordis (Phase 6)
// Each domain lives in `admin/*.rs`; shared DTOs/helpers in `admin/shared.rs`.
// This shim re-exports domains and provides middleware + routing.

#[path = "admin/agents.rs"]
pub mod agents;
#[path = "admin/audit.rs"]
pub mod audit;
#[path = "admin/billing.rs"]
pub mod billing;
#[path = "admin/connectors.rs"]
pub mod connectors;
#[path = "admin/cordis.rs"]
pub mod cordis;
#[path = "admin/fleet_secrets.rs"]
pub mod fleet_secrets;
#[path = "admin/health.rs"]
pub mod health;
#[path = "admin/mcp.rs"]
pub mod mcp;
#[path = "admin/pipelines.rs"]
pub mod pipelines;
#[path = "admin/providers.rs"]
pub mod providers;
#[path = "admin/schedules.rs"]
pub mod schedules;
#[path = "admin/shared.rs"]
pub mod shared;
#[path = "admin/tenants.rs"]
pub mod tenants;
#[path = "admin/tools.rs"]
pub mod tools;
#[path = "admin/triggers.rs"]
pub mod triggers;

pub use agents::*;
pub use audit::*;
pub use billing::*;
pub use connectors::*;
pub use cordis::*;
pub use fleet_secrets::*;
pub use health::*;
pub use mcp::*;
pub use pipelines::*;
pub use providers::*;
pub use schedules::*;
pub use tenants::*;
pub use tools::*;
pub use triggers::*;

// Re-export shared DTOs/helpers so `use super::*;` in shards resolves.
pub use shared::*;

/// Extended JWT claims that include Eruka's roles map.
#[derive(Debug, Deserialize)]
pub(crate) struct AdminClaims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
    #[serde(default)]
    pub roles: HashMap<String, Vec<RoleEntry>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RoleEntry {
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

/// Identity of the admin request that passed [`admin_middleware`].
///
/// `subject` carries the JWT `sub` claim for token-authenticated requests.
/// `X-Admin-Secret` requests have no user identity, so `auth` records the
/// static secret instead and [`AdminActor::audit_actor`] reports it.
#[derive(Debug, Clone, Default)]
pub struct AdminActor {
    pub subject: Option<String>,
    pub email: Option<String>,
    /// `jwt` or `admin_secret`; `None` when no admin middleware ran.
    pub auth: Option<&'static str>,
    /// Client address from `X-Forwarded-For` / `X-Real-IP` / the socket.
    pub client_ip: Option<String>,
}

impl AdminActor {
    /// Value written to `admin_audit_log.actor`.
    pub fn audit_actor(&self) -> Option<&str> {
        self.subject
            .as_deref()
            .or_else(|| (self.auth == Some("admin_secret")).then_some("admin_secret"))
    }

    /// Value written to `admin_audit_log.admin_ip`.
    pub fn ip(&self) -> Option<&str> {
        self.client_ip.as_deref()
    }
}

impl<S> axum::extract::FromRequestParts<S> for AdminActor
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    /// Missing extension (no admin middleware on the route) yields the
    /// anonymous default: audit rows then carry a NULL actor, never a
    /// fabricated identity.
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<AdminActor>()
            .cloned()
            .unwrap_or_default())
    }
}

/// Client address for audit rows. Caddy sits in front of the server and
/// appends the connecting address to `X-Forwarded-For`, so the LAST entry is
/// the address Caddy saw. Falls back to `X-Real-IP`, then to the socket peer
/// (present when the server runs with `into_make_service_with_connect_info`).
fn admin_client_ip(req: &axum::extract::Request) -> Option<String> {
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(last) = forwarded
            .split(',')
            .map(str::trim)
            .rfind(|entry| !entry.is_empty())
        {
            return Some(last.to_string());
        }
    }
    if let Some(real_ip) = req.headers().get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let real_ip = real_ip.trim();
        if !real_ip.is_empty() {
            return Some(real_ip.to_string());
        }
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip().to_string())
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
            let hi = hex_value(bytes[i + 1]);
            let lo = hex_value(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out)
        .map_err(|e| HttpError::from(AppError::InvalidInput(format!("invalid utf8: {e}"))))
}

pub async fn admin_middleware(mut req: axum::extract::Request, next: Next) -> Response {
    let client_ip = admin_client_ip(&req);
    let admin_secret = std::env::var("ADMIN_API_KEY").ok();
    let header_secret = req
        .headers()
        .get("x-admin-secret")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if let (Some(expected), Some(given)) = (&admin_secret, &header_secret) {
        if expected == given {
            req.extensions_mut().insert(AdminActor {
                subject: None,
                email: None,
                auth: Some("admin_secret"),
                client_ip,
            });
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
                    req.extensions_mut().insert(AdminActor {
                        subject: Some(data.claims.sub.clone()),
                        email: Some(data.claims.email.clone()),
                        auth: Some("jwt"),
                        client_ip,
                    });
                    return next.run(req).await;
                }
            }
        }
    }
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .body(
            r#"{"error":"Admin access requires X-Admin-Secret header or JWT with admin role"}"#
                .into(),
        )
        .unwrap()
}

/// Merge all admin domain routers into one `Router<Arc<Context>>`.
pub fn admin_routes() -> axum::Router<Arc<Context>> {
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
        .merge(cordis::routes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::FromRequestParts;
    use axum::http::Request;

    fn request_with_headers(headers: &[(&str, &str)]) -> axum::extract::Request {
        let mut builder = Request::builder();
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn audit_actor_prefers_jwt_subject() {
        let actor = AdminActor {
            subject: Some("user-42".into()),
            email: Some("admin@example.com".into()),
            auth: Some("jwt"),
            client_ip: None,
        };
        assert_eq!(actor.audit_actor(), Some("user-42"));
    }

    #[test]
    fn audit_actor_reports_static_secret_provenance() {
        let actor = AdminActor {
            subject: None,
            email: None,
            auth: Some("admin_secret"),
            client_ip: None,
        };
        assert_eq!(actor.audit_actor(), Some("admin_secret"));
    }

    #[test]
    fn anonymous_actor_has_no_audit_actor() {
        assert_eq!(AdminActor::default().audit_actor(), None);
    }

    #[test]
    fn client_ip_uses_last_forwarded_entry() {
        let req = request_with_headers(&[("x-forwarded-for", "203.0.113.9, 10.0.0.7")]);
        assert_eq!(admin_client_ip(&req).as_deref(), Some("10.0.0.7"));
    }

    #[test]
    fn client_ip_falls_back_to_real_ip_then_socket() {
        let req = request_with_headers(&[("x-real-ip", "198.51.100.4")]);
        assert_eq!(admin_client_ip(&req).as_deref(), Some("198.51.100.4"));

        let mut req = request_with_headers(&[]);
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                4242,
            ))));
        assert_eq!(admin_client_ip(&req).as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn client_ip_ignores_empty_forwarded_entries() {
        let req = request_with_headers(&[("x-forwarded-for", " , ")]);
        assert_eq!(admin_client_ip(&req), None);
    }

    #[tokio::test]
    async fn actor_extractor_defaults_to_anonymous_without_extension() {
        let req = request_with_headers(&[]);
        let (mut parts, _body) = req.into_parts();
        let actor = AdminActor::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(actor.audit_actor(), None);
        assert_eq!(actor.ip(), None);
    }

    #[tokio::test]
    async fn actor_extractor_reads_inserted_extension() {
        let mut req = request_with_headers(&[]);
        req.extensions_mut().insert(AdminActor {
            subject: Some("user-7".into()),
            email: None,
            auth: Some("jwt"),
            client_ip: Some("10.1.2.3".into()),
        });
        let (mut parts, _body) = req.into_parts();
        let actor = AdminActor::from_request_parts(&mut parts, &())
            .await
            .unwrap();
        assert_eq!(actor.audit_actor(), Some("user-7"));
        assert_eq!(actor.ip(), Some("10.1.2.3"));
    }
}
