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

/// Products whose admin roles grant access to the ARES admin API.
const ADMIN_PRODUCTS: [&str; 3] = ["admin", "ares", "eruka"];

/// Roles that count as admin inside an accepted product.
const ADMIN_ROLES: [&str; 2] = ["admin", "super_admin"];

/// Check whether a validated issuer user context holds a platform admin role.
pub(crate) fn user_context_has_admin_role(user: &crate::auth::jwks::UserContext) -> bool {
    ADMIN_PRODUCTS
        .iter()
        .any(|product| ADMIN_ROLES.iter().any(|role| user.has_role(product, role)))
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

    // JWT path. Only EdDSA and RS256 tokens verified against the issuer
    // JWKS can mint an admin actor; HS256 carries no admin authority.
    if let Some(token) = admin_token_from_request(&req) {
        let jwks = req
            .extensions()
            .get::<Arc<crate::auth::jwks::JwksCache>>()
            .cloned();
        if let Some(mut actor) = admin_actor_from_token(&token, jwks).await {
            actor.client_ip = client_ip;
            req.extensions_mut().insert(actor);
            return next.run(req).await;
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

/// Build the admin identity from a request token when that token carries
/// an admin role. Returns `None` for a bad signature, an unsupported
/// algorithm, or a token without an admin role.
///
/// `jwks` is the injected key set; without one the shared env-configured
/// cache serves the asymmetric path.
async fn admin_actor_from_token(
    token: &str,
    jwks: Option<Arc<crate::auth::jwks::JwksCache>>,
) -> Option<AdminActor> {
    let header = jsonwebtoken::decode_header(token).ok()?;
    match header.alg {
        // HS256 carries no admin authority: only the JWKS (EdDSA/RS256)
        // path below can mint an admin actor.
        jsonwebtoken::Algorithm::HS256 => None,
        jsonwebtoken::Algorithm::EdDSA | jsonwebtoken::Algorithm::RS256 => {
            let jwks = jwks.unwrap_or_else(crate::auth::jwks::JwksCache::shared);
            let user = jwks.validate(token).await.ok()?;
            user_context_has_admin_role(&user).then(|| AdminActor {
                subject: Some(user.user_id),
                email: Some(user.email),
                auth: Some("jwt"),
                client_ip: None,
            })
        }
        _ => None,
    }
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

    // -------------------------------------------------------------------------
    // admin_middleware: dual verification (HS256 secret + EdDSA JWKS)
    // -------------------------------------------------------------------------

    async fn actor_handler(actor: AdminActor) -> String {
        actor.audit_actor().unwrap_or_default().to_string()
    }

    /// App with the middleware under test and the JWKS cache injected, as
    /// `create_router` wires it in production.
    fn admin_test_app(jwks: Arc<crate::auth::jwks::JwksCache>) -> axum::Router {
        axum::Router::new()
            .route("/", axum::routing::get(actor_handler))
            .layer(axum::middleware::from_fn(admin_middleware))
            .layer(axum::middleware::from_fn(
                move |mut req: axum::extract::Request, next: axum::middleware::Next| {
                    let jwks = Arc::clone(&jwks);
                    async move {
                        req.extensions_mut().insert(jwks);
                        next.run(req).await
                    }
                },
            ))
    }

    fn admin_test_request(token: &str) -> axum::extract::Request {
        Request::builder()
            .uri("/")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    async fn response_body(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn admin_middleware_accepts_eddsa_admin_token_via_jwks() {
        use crate::auth::test_keys::{eddsa_token, jwks_json, jwks_stub};
        use tower::ServiceExt;

        let seed = 21u8;
        let kid = "eruka-admin-1";
        let url = jwks_stub(jwks_json(&[(kid, &[seed; 32])])).await;
        let app = admin_test_app(Arc::new(crate::auth::jwks::JwksCache::new(url)));

        let token = eddsa_token(seed, Some(kid));
        let response = app.oneshot(admin_test_request(&token)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body(response).await, "user-1");
    }

    #[tokio::test]
    async fn admin_middleware_rejects_eddsa_token_without_admin_role() {
        use crate::auth::test_keys::{dirmacs_claims, jwks_json, jwks_stub, pem_for_seed};
        use tower::ServiceExt;

        let seed = 22u8;
        let kid = "eruka-admin-2";
        let url = jwks_stub(jwks_json(&[(kid, &[seed; 32])])).await;
        let app = admin_test_app(Arc::new(crate::auth::jwks::JwksCache::new(url)));

        let mut claims = dirmacs_claims();
        claims.roles = Some(std::collections::HashMap::from([(
            "ares".to_string(),
            vec![crate::auth::jwks::RoleEntry {
                role: "user".into(),
                resource_id: None,
            }],
        )]));
        let token =
            crate::auth::jwks::encode_token_with_pem_key(&claims, &pem_for_seed(seed), Some(kid))
                .expect("sign");
        let response = app.oneshot(admin_test_request(&token)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    fn admin_user_context(
        roles: std::collections::HashMap<String, Vec<crate::auth::jwks::RoleEntry>>,
    ) -> crate::auth::jwks::UserContext {
        crate::auth::jwks::UserContext {
            user_id: "user-1".into(),
            email: "admin@example.com".into(),
            roles,
            token_version: 0,
        }
    }

    fn admin_role_entry(role: &str) -> crate::auth::jwks::RoleEntry {
        crate::auth::jwks::RoleEntry {
            role: role.into(),
            resource_id: None,
        }
    }

    #[test]
    fn user_context_admin_role_accepts_ares_admin() {
        let roles = std::collections::HashMap::from([(
            "ares".to_string(),
            vec![admin_role_entry("admin")],
        )]);
        assert!(user_context_has_admin_role(&admin_user_context(roles)));
    }

    #[test]
    fn user_context_admin_role_rejects_ares_viewer_only() {
        let roles = std::collections::HashMap::from([(
            "ares".to_string(),
            vec![admin_role_entry("viewer"), admin_role_entry("editor")],
        )]);
        assert!(!user_context_has_admin_role(&admin_user_context(roles)));
    }

    #[test]
    fn user_context_admin_role_accepts_eruka_super_admin() {
        let roles = std::collections::HashMap::from([(
            "eruka".to_string(),
            vec![admin_role_entry("super_admin")],
        )]);
        assert!(user_context_has_admin_role(&admin_user_context(roles)));
    }

    #[test]
    fn user_context_admin_role_rejects_empty_roles() {
        assert!(!user_context_has_admin_role(&admin_user_context(
            std::collections::HashMap::new()
        )));
    }

    #[tokio::test]
    async fn admin_middleware_rejects_tampered_eddsa_token() {
        use crate::auth::test_keys::{eddsa_token, jwks_json, jwks_stub};
        use tower::ServiceExt;

        let seed = 23u8;
        let kid = "eruka-admin-3";
        let url = jwks_stub(jwks_json(&[(kid, &[seed; 32])])).await;
        let app = admin_test_app(Arc::new(crate::auth::jwks::JwksCache::new(url)));

        let token = eddsa_token(seed, Some(kid));
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        parts[1].push('x');
        let response = app
            .oneshot(admin_test_request(&parts.join(".")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_middleware_rejects_hs256_keeps_admin_secret_path() {
        use crate::auth::test_keys::{eddsa_token, jwks_json, jwks_stub, rewrite_alg};
        use tower::ServiceExt;

        let env_guard = shared::lock_admin_env();
        std::env::set_var("ADMIN_API_KEY", "test-admin-secret");

        let seed = 24u8;
        let kid = "eruka-admin-4";
        let url = jwks_stub(jwks_json(&[(kid, &[seed; 32])])).await;
        let app = admin_test_app(Arc::new(crate::auth::jwks::JwksCache::new(url)));

        // X-Admin-Secret stays the first path.
        let secret_req = Request::builder()
            .uri("/")
            .header("x-admin-secret", "test-admin-secret")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(secret_req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body(response).await, "admin_secret");

        // HS256 carries no admin authority: an HS256 admin token yields 401.
        let hs256_claims = serde_json::json!({
            "sub": "hs256-admin",
            "email": "admin@example.com",
            "exp": chrono::Utc::now().timestamp() + 3600,
            "iat": chrono::Utc::now().timestamp(),
            "roles": { "ares": [{ "role": "admin" }] },
        });
        let hs256_token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &hs256_claims,
            &jsonwebtoken::EncodingKey::from_secret(b"admin-test-secret-at-least-32-chars-long"),
        )
        .expect("sign");
        let response = app
            .clone()
            .oneshot(admin_test_request(&hs256_token))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Downgrade attempt: an EdDSA token whose header now says HS256
        // must not pass as an admin token.
        let downgraded = rewrite_alg(&eddsa_token(seed, Some(kid)), "HS256");
        let response = app
            .clone()
            .oneshot(admin_test_request(&downgraded))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        std::env::remove_var("ADMIN_API_KEY");
        drop(env_guard);
    }
}
