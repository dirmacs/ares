//! Cached JWKS fetch and verification for asymmetric (EdDSA / RS256) tokens.
//!
//! [`dirmacs_auth::JwksKeySet`] holds the verification logic and the
//! algorithm-confusion protection. It has no HTTP client, so this module
//! fetches the JWKS document, caches the parsed key set, and refreshes it:
//!
//! - at startup, through [`JwksCache::warmup`];
//! - after the key set is older than [`JWKS_TTL`];
//! - once when a token names an unknown `kid`, so key rotation heals.
//!
//! When the issuer is down, the last good key set keeps serving requests.
//! Fetch logs carry the URL and the error only, never key material or tokens.

use dirmacs_auth::{AuthError, JwksKeySet, UserContext};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

/// Default issuer JWKS endpoint (Eruka).
pub const DEFAULT_JWKS_URL: &str = "https://eruka.dirmacs.com/.well-known/jwks.json";

/// Environment variable that overrides [`DEFAULT_JWKS_URL`].
pub const JWKS_URL_ENV: &str = "ERUKA_JWKS_URL";

/// Key set lifetime before the cache tries a background refresh.
pub const JWKS_TTL: Duration = Duration::from_secs(3600);

/// Timeout for one JWKS document fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Failures from the cached JWKS path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwksError {
    /// No cached key set exists and the issuer could not be reached.
    #[error("no JWKS key set available from {0}")]
    Unavailable(String),
    /// The document could not be fetched or parsed.
    #[error("JWKS fetch failed: {0}")]
    Fetch(String),
    /// The key set was fetched but rejected the token.
    #[error("JWKS verification failed: {0}")]
    Verify(String),
}

impl From<AuthError> for JwksError {
    fn from(err: AuthError) -> Self {
        JwksError::Verify(err.to_string())
    }
}

#[derive(Default)]
struct JwksState {
    keys: Option<JwksKeySet>,
    fetched_at: Option<Instant>,
}

/// Fetches, caches, and refreshes the issuer JWKS document.
///
/// Cloning is shallow: the shared `Arc` of callers keeps one state.
pub struct JwksCache {
    url: String,
    ttl: Duration,
    client: reqwest::Client,
    state: tokio::sync::RwLock<JwksState>,
}

impl JwksCache {
    /// Creates an empty cache that reads the key set from `url`.
    pub fn new(url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            url: url.into(),
            ttl: JWKS_TTL,
            client,
            state: tokio::sync::RwLock::new(JwksState::default()),
        }
    }

    /// Creates a cache from `ERUKA_JWKS_URL`, or the default issuer URL.
    pub fn from_env() -> Self {
        let url = std::env::var(JWKS_URL_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_JWKS_URL.to_string());
        Self::new(url.trim())
    }

    /// Process-wide cache for callers that hold no injected instance.
    pub fn shared() -> Arc<Self> {
        static SHARED: LazyLock<Arc<JwksCache>> =
            LazyLock::new(|| Arc::new(JwksCache::from_env()));
        Arc::clone(&SHARED)
    }

    /// The JWKS document URL this cache reads.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Overrides the key set lifetime (tests use a short TTL).
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Fetches the key set once at startup. Failure is not fatal: the
    /// next verification retries and the previous set stays valid.
    pub async fn warmup(&self) {
        match self.fetch_and_store().await {
            Ok(_) => tracing::info!(url = %self.url, "JWKS key set loaded"),
            Err(err) => tracing::warn!(
                url = %self.url,
                error = %err,
                "initial JWKS fetch failed; verification will retry on demand"
            ),
        }
    }

    /// Verifies a token and returns its user context.
    ///
    /// A token whose `kid` is unknown to the cached set triggers one
    /// forced refresh, then the check repeats against the fresh set.
    pub async fn validate(&self, token: &str) -> Result<UserContext, JwksError> {
        let keys = self.keys().await?;
        match keys.validate(token) {
            Err(AuthError::UnknownKid(kid)) => {
                tracing::info!(url = %self.url, kid = %kid, "unknown JWKS kid; refreshing key set");
                match self.fetch_and_store().await {
                    Ok(fresh) => fresh.validate(token).map_err(JwksError::from),
                    Err(err) => {
                        tracing::warn!(
                            url = %self.url,
                            error = %err,
                            "JWKS refresh after unknown kid failed"
                        );
                        Err(JwksError::Verify(AuthError::UnknownKid(kid).to_string()))
                    }
                }
            }
            other => other.map_err(JwksError::from),
        }
    }

    /// Returns a fresh key set, refreshing when the cached one is stale.
    ///
    /// A failed refresh falls back to the cached set, so an issuer outage
    /// does not break tokens that the cached set can still verify.
    async fn keys(&self) -> Result<JwksKeySet, JwksError> {
        {
            let state = self.state.read().await;
            if let (Some(keys), Some(fetched_at)) = (&state.keys, state.fetched_at) {
                if fetched_at.elapsed() < self.ttl {
                    return Ok(keys.clone());
                }
            }
        }

        match self.fetch_and_store().await {
            Ok(keys) => Ok(keys),
            Err(err) => {
                let state = self.state.read().await;
                match &state.keys {
                    Some(keys) => {
                        tracing::warn!(
                            url = %self.url,
                            error = %err,
                            "JWKS refresh failed; serving the cached key set"
                        );
                        Ok(keys.clone())
                    }
                    None => Err(err),
                }
            }
        }
    }

    /// Fetches the document, parses it, and swaps the cached set.
    async fn fetch_and_store(&self) -> Result<JwksKeySet, JwksError> {
        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .map_err(|err| JwksError::Fetch(err.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(JwksError::Fetch(format!(
                "unexpected status {} from {}",
                status, self.url
            )));
        }
        let body = response
            .text()
            .await
            .map_err(|err| JwksError::Fetch(err.to_string()))?;
        let keys = JwksKeySet::from_jwks_json(&body)
            .map_err(|err| JwksError::Fetch(err.to_string()))?;

        let mut state = self.state.write().await;
        state.keys = Some(keys.clone());
        state.fetched_at = Some(Instant::now());
        tracing::info!(url = %self.url, "fetched JWKS key set");
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::test_keys::{eddsa_token, jwks_json, jwks_stub, jwks_stub_rotating};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KID_ONE: &str = "eruka-k1";
    const KID_TWO: &str = "eruka-k2";

    #[tokio::test]
    async fn warmup_loads_the_key_set_and_accepts_a_token() {
        let key = 7u8;
        let url = jwks_stub(jwks_json(&[(KID_ONE, &[key; 32])])).await;
        let cache = JwksCache::new(url);

        cache.warmup().await;

        let token = eddsa_token(key, Some(KID_ONE));
        let ctx = cache.validate(&token).await.expect("eddsa token accepted");
        assert_eq!(ctx.user_id, "user-1");
        assert!(ctx.has_role("ares", "admin"));
    }

    #[tokio::test]
    async fn unknown_kid_triggers_one_refresh_fetch() {
        let key_one = 7u8;
        let key_two = 9u8;

        // First response holds key one only; every later response adds key two.
        let first = jwks_json(&[(KID_ONE, &[key_one; 32])]);
        let rotated = jwks_json(&[(KID_ONE, &[key_one; 32]), (KID_TWO, &[key_two; 32])]);
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_stub = Arc::clone(&hits);
        let url = jwks_stub_rotating(move || {
            let call = hits_for_stub.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                first.clone()
            } else {
                rotated.clone()
            }
        })
        .await;

        let cache = JwksCache::new(url);
        cache.warmup().await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let token = eddsa_token(key_two, Some(KID_TWO));
        let ctx = cache.validate(&token).await.expect("rotated key accepted");
        assert_eq!(ctx.user_id, "user-1");
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // The refreshed set is cached; a second token needs no more fetches.
        cache.validate(&token).await.expect("second token accepted");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cached_key_set_survives_issuer_downtime() {
        let key = 3u8;
        let url = jwks_stub(jwks_json(&[(KID_ONE, &[key; 32])])).await;
        // A zero TTL forces every check to try a refresh first.
        let cache = JwksCache::new(url).with_ttl(Duration::ZERO);
        cache.warmup().await;

        // The stub is gone now; the cached set must still verify.
        let token = eddsa_token(key, Some(KID_ONE));
        let ctx = cache.validate(&token).await.expect("cached key set served");
        assert_eq!(ctx.user_id, "user-1");
    }

    #[tokio::test]
    async fn missing_key_set_and_dead_issuer_is_an_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/jwks.json", listener.local_addr().unwrap());
        drop(listener);
        let cache = JwksCache::new(url);
        assert!(cache.validate("a.b.c").await.is_err());
    }

    #[tokio::test]
    async fn tampered_token_is_rejected() {
        let key = 5u8;
        let url = jwks_stub(jwks_json(&[(KID_ONE, &[key; 32])])).await;
        let cache = JwksCache::new(url);
        cache.warmup().await;

        let token = eddsa_token(key, Some(KID_ONE));
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        parts[1].push('x');
        assert!(cache.validate(&parts.join(".")).await.is_err());
    }

    #[tokio::test]
    async fn hs256_token_never_passes_the_jwks_path() {
        let key = 5u8;
        let url = jwks_stub(jwks_json(&[(KID_ONE, &[key; 32])])).await;
        let cache = JwksCache::new(url);
        cache.warmup().await;

        // An attacker signs HS256 with the seed and reuses the kid.
        let token = crate::auth::test_keys::hs256_token(
            &[key; 32],
            Some(KID_ONE),
            jsonwebtoken::Algorithm::HS256,
        );
        assert!(cache.validate(&token).await.is_err());
    }
}
