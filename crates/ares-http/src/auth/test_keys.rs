//! Test-only Ed25519 and JWKS helpers shared by the auth test modules.
//!
//! The keys here protect nothing. Seeds are fixed so tests stay
//! deterministic. The JWKS document and the signed token use the same
//! issuer wire format that Eruka uses.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use std::sync::Arc;

/// PKCS#8 PEM for the Ed25519 key derived from `seed`.
pub(crate) fn pem_for_seed(seed: u8) -> Vec<u8> {
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let der = signing.to_pkcs8_der().expect("PKCS#8 encode");
    let body = base64::engine::general_purpose::STANDARD.encode(der.as_bytes());

    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem.into_bytes()
}

/// Standard JWKS document with one Ed25519 key per `(kid, seed)` entry.
pub(crate) fn jwks_json(keys: &[(&str, &[u8; 32])]) -> String {
    let entries: Vec<serde_json::Value> = keys
        .iter()
        .map(|(kid, seed)| {
            let signing = SigningKey::from_bytes(seed);
            let x = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
            serde_json::json!({
                "kty": "OKP",
                "crv": "Ed25519",
                "x": x,
                "alg": "EdDSA",
                "use": "sig",
                "kid": kid,
            })
        })
        .collect();
    serde_json::json!({ "keys": entries }).to_string()
}

/// Claims with an `ares/admin` role, matching an enriched Eruka token.
pub(crate) fn dirmacs_claims() -> crate::auth::jwks::IssuerClaims {
    let now = chrono::Utc::now().timestamp();
    crate::auth::jwks::IssuerClaims {
        sub: "user-1".into(),
        email: "admin@example.com".into(),
        exp: now + 3600,
        iat: now,
        roles: Some(std::collections::HashMap::from([(
            "ares".to_string(),
            vec![crate::auth::jwks::RoleEntry {
                role: "admin".into(),
                resource_id: None,
            }],
        )])),
        token_version: Some(0),
    }
}

/// Signs the standard admin claims with the Ed25519 key derived from `seed`.
pub(crate) fn eddsa_token(seed: u8, kid: Option<&str>) -> String {
    let pem = pem_for_seed(seed);
    crate::auth::jwks::encode_token_with_pem_key(&dirmacs_claims(), &pem, kid).expect("sign EdDSA")
}

/// HMAC token with a chosen `kid` and algorithm (wrong-secret attacks).
pub(crate) fn hs256_token(secret: &[u8], kid: Option<&str>, alg: Algorithm) -> String {
    let mut header = Header::new(alg);
    header.kid = kid.map(String::from);
    let claims = crate::auth::jwt::build_claims(
        "user-1",
        "admin@example.com",
        chrono::Utc::now().timestamp() as usize,
        3600,
        None,
    );
    encode(&header, &claims, &EncodingKey::from_secret(secret)).expect("sign HMAC")
}

/// Rewrites the JWT header algorithm while keeping payload and signature.
/// Builds downgrade attacks: the header then names a path the signature
/// cannot satisfy.
pub(crate) fn rewrite_alg(token: &str, alg: &str) -> String {
    let mut parts = token.split('.');
    let header_b64 = parts.next().expect("header part");
    let payload = parts.next().expect("payload part");
    let signature = parts.next().expect("signature part");
    let mut header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header_b64).expect("header base64"))
            .expect("header json");
    header["alg"] = serde_json::Value::String(alg.to_string());
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header json"));
    format!("{encoded}.{payload}.{signature}")
}

/// Serves one fixed JWKS body on a loopback port and returns its URL.
pub(crate) async fn jwks_stub(body: impl Into<String>) -> String {
    let body = body.into();
    jwks_stub_rotating(move || body.clone()).await
}

/// Serves a JWKS body that may change per request (key rotation tests).
pub(crate) async fn jwks_stub_rotating<F>(body: F) -> String
where
    F: Fn() -> String + Send + Sync + 'static,
{
    use axum::routing::get;

    let body = Arc::new(body);
    let app = axum::Router::new().route(
        "/.well-known/jwks.json",
        get(move || {
            let body = Arc::clone(&body);
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/.well-known/jwks.json")
}
