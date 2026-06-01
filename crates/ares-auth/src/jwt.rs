use ares_types::types::{AppError, Claims, Result, TokenResponse};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::Utc;
use jsonwebtoken::{
    decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =============================================================================
// Pure JWT helpers (resolution, validation, claim building)
// =============================================================================

/// JWT claims with optional tenant scoping for multi-tenant deployments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomClaims {
    /// Subject (user ID).
    pub sub: String,
    /// User's email address.
    pub email: String,
    /// Expiration time (Unix timestamp).
    pub exp: usize,
    /// Issued at time (Unix timestamp).
    pub iat: usize,
    /// JWT ID — unique per token (present on refresh tokens).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub jti: String,
    /// Tenant that issued or owns this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// Typed JWT validation failures for pure helpers and unit tests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    /// Token `exp` is in the past (after leeway).
    #[error("token expired")]
    Expired,
    /// HMAC/signature verification failed.
    #[error("invalid signature")]
    InvalidSignature,
    /// Malformed token, unsupported algorithm, or claim mismatch.
    #[error("invalid claims: {0}")]
    InvalidClaims(String),
}

impl CustomClaims {
    /// Standard [`Claims`] payload without tenant metadata.
    pub fn to_claims(&self) -> Claims {
        Claims {
            sub: self.sub.clone(),
            email: self.email.clone(),
            exp: self.exp,
            iat: self.iat,
            jti: self.jti.clone(),
        }
    }
}

impl From<Claims> for CustomClaims {
    fn from(claims: Claims) -> Self {
        Self {
            sub: claims.sub,
            email: claims.email,
            exp: claims.exp,
            iat: claims.iat,
            jti: claims.jti,
            tenant_id: None,
        }
    }
}

impl From<CustomClaims> for Claims {
    fn from(custom: CustomClaims) -> Self {
        custom.to_claims()
    }
}

/// Build standard JWT [`Claims`] from subject, email, and TTL.
pub fn build_claims(
    sub: impl Into<String>,
    email: impl Into<String>,
    issued_at: usize,
    expiry_secs: i64,
    jti: Option<String>,
) -> Claims {
    let exp = issued_at.saturating_add(expiry_secs.max(0) as usize);
    Claims {
        sub: sub.into(),
        email: email.into(),
        exp,
        iat: issued_at,
        jti: jti.unwrap_or_default(),
    }
}

/// Build tenant-scoped [`CustomClaims`].
pub fn build_custom_claims(
    sub: impl Into<String>,
    email: impl Into<String>,
    issued_at: usize,
    expiry_secs: i64,
    jti: Option<String>,
    tenant_id: Option<String>,
) -> CustomClaims {
    let claims = build_claims(sub, email, issued_at, expiry_secs, jti);
    CustomClaims {
        sub: claims.sub,
        email: claims.email,
        exp: claims.exp,
        iat: claims.iat,
        jti: claims.jti,
        tenant_id,
    }
}

/// Verify HS256 signature and decode standard claims.
pub fn verify_signature(token: &str, secret: &[u8], leeway: u64) -> std::result::Result<Claims, JwtError> {
    let header = decode_header(token).map_err(jwt_decode_error)?;
    if header.alg != Algorithm::HS256 {
        return Err(JwtError::InvalidClaims(format!(
            "unsupported algorithm {:?}, only HS256 is accepted",
            header.alg
        )));
    }

    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = leeway;

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(jwt_decode_error)
}

/// Return the subject (`sub`) when present and non-empty.
pub fn extract_subject(claims: &Claims) -> std::result::Result<&str, JwtError> {
    if claims.sub.trim().is_empty() {
        return Err(JwtError::InvalidClaims("missing subject".into()));
    }
    Ok(claims.sub.as_str())
}

/// Validate `exp` against `now`, honoring leeway for clock skew.
pub fn validate_expiration(
    exp: usize,
    leeway: u64,
    now: usize,
) -> std::result::Result<(), JwtError> {
    if exp.saturating_add(leeway as usize) < now {
        return Err(JwtError::Expired);
    }
    Ok(())
}

/// Ensure token tenant matches the expected tenant id.
pub fn validate_tenant(
    claims: &CustomClaims,
    expected_tenant_id: &str,
) -> std::result::Result<(), JwtError> {
    match claims.tenant_id.as_deref() {
        Some(tenant) if tenant == expected_tenant_id => Ok(()),
        Some(tenant) => Err(JwtError::InvalidClaims(format!(
            "tenant mismatch: expected {expected_tenant_id}, got {tenant}"
        ))),
        None => Err(JwtError::InvalidClaims("missing tenant_id claim".into())),
    }
}

fn jwt_decode_error(err: jsonwebtoken::errors::Error) -> JwtError {
    use jsonwebtoken::errors::ErrorKind;
    match err.kind() {
        ErrorKind::ExpiredSignature => JwtError::Expired,
        ErrorKind::InvalidSignature => JwtError::InvalidSignature,
        _ => JwtError::InvalidClaims(err.to_string()),
    }
}

fn jwt_error_to_app_error(err: JwtError) -> AppError {
    AppError::Auth(err.to_string())
}

fn sign_claims(claims: &Claims, secret: &[u8]) -> std::result::Result<String, JwtError> {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| JwtError::InvalidClaims(e.to_string()))
}

/// Authentication service for JWT token management and password hashing.
///
/// Provides secure password hashing using Argon2id and JWT token
/// generation/verification using HS256.
pub struct AuthService {
    jwt_secret: String,
    access_expiry: i64,
    refresh_expiry: i64,
    /// Leeway in seconds for token expiration validation (default: 60)
    /// This accounts for clock skew between servers.
    leeway: u64,
}

impl AuthService {
    /// Creates a new AuthService with the given configuration.
    ///
    /// # Arguments
    /// * `jwt_secret` - Secret key for signing JWTs (should be at least 32 chars)
    /// * `access_expiry` - Access token validity in seconds
    /// * `refresh_expiry` - Refresh token validity in seconds
    pub fn new(jwt_secret: String, access_expiry: i64, refresh_expiry: i64) -> Self {
        Self {
            jwt_secret,
            access_expiry,
            refresh_expiry,
            leeway: 60, // Default 60-second leeway for clock skew
        }
    }

    /// Creates a new AuthService with custom leeway for token validation.
    ///
    /// # Arguments
    /// * `jwt_secret` - Secret key for signing JWTs (should be at least 32 chars)
    /// * `access_expiry` - Access token validity in seconds
    /// * `refresh_expiry` - Refresh token validity in seconds
    /// * `leeway` - Leeway in seconds for expiration checks (0 for strict)
    pub fn with_leeway(
        jwt_secret: String,
        access_expiry: i64,
        refresh_expiry: i64,
        leeway: u64,
    ) -> Self {
        Self {
            jwt_secret,
            access_expiry,
            refresh_expiry,
            leeway,
        }
    }

    /// Hashes a password using Argon2id.
    ///
    /// Returns a PHC-formatted hash string.
    pub fn hash_password(&self, password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();

        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|e| AppError::Auth(format!("Failed to hash password: {}", e)))
    }

    /// Verifies a password against an Argon2 hash.
    pub fn verify_password(&self, password: &str, hash: &str) -> Result<bool> {
        let parsed_hash = PasswordHash::new(hash)
            .map_err(|e| AppError::Auth(format!("Invalid password hash: {}", e)))?;

        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    }

    /// Generates access and refresh tokens for a user.
    pub fn generate_tokens(&self, user_id: &str, email: &str) -> Result<TokenResponse> {
        let access_token = self.generate_access_token(user_id, email)?;
        let refresh_token = self.generate_refresh_token(user_id, email)?;

        Ok(TokenResponse {
            access_token,
            refresh_token,
            expires_in: self.access_expiry,
        })
    }

    fn generate_access_token(&self, user_id: &str, email: &str) -> Result<String> {
        let now = Utc::now().timestamp() as usize;
        let claims = build_claims(user_id, email, now, self.access_expiry, None);
        sign_claims(&claims, self.jwt_secret.as_bytes()).map_err(jwt_error_to_app_error)
    }

    fn generate_refresh_token(&self, user_id: &str, email: &str) -> Result<String> {
        let now = Utc::now().timestamp() as usize;
        let claims = build_claims(
            user_id,
            email,
            now,
            self.refresh_expiry,
            Some(Uuid::new_v4().to_string()),
        );
        sign_claims(&claims, self.jwt_secret.as_bytes()).map_err(jwt_error_to_app_error)
    }

    /// Verifies a JWT token and returns the claims.
    pub fn verify_token(&self, token: &str) -> Result<Claims> {
        self.verify_token_with_leeway(token, self.leeway)
    }

    /// Verifies a JWT token with a custom leeway (in seconds) for expiration checks.
    ///
    /// The leeway accounts for clock skew between servers. Default is 60 seconds.
    /// Use leeway of 0 for strict expiration checking (e.g., in tests).
    pub fn verify_token_with_leeway(&self, token: &str, leeway: u64) -> Result<Claims> {
        verify_signature(token, self.jwt_secret.as_bytes(), leeway).map_err(jwt_error_to_app_error)
    }

    /// Hashes a token using SHA256 for secure storage.
    pub fn hash_token(&self, token: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let result = hasher.finalize();
        result
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::time::Duration;

    const TEST_SECRET: &str = "test-secret-key-that-is-at-least-32-chars";

    fn test_secret() -> &'static [u8] {
        TEST_SECRET.as_bytes()
    }

    fn create_test_service() -> AuthService {
        AuthService::new(TEST_SECRET.to_string(), 900, 604_800)
    }

    fn now_ts() -> usize {
        Utc::now().timestamp() as usize
    }


    fn assert_claims_eq(actual: &Claims, expected: &Claims) {
        assert_eq!(actual.sub, expected.sub);
        assert_eq!(actual.email, expected.email);
        assert_eq!(actual.exp, expected.exp);
        assert_eq!(actual.iat, expected.iat);
        assert_eq!(actual.jti, expected.jti);
    }

    fn sign_custom_claims(claims: &CustomClaims, secret: &[u8]) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(secret),
        )
        .expect("encode custom claims")
    }

    // -------------------------------------------------------------------------
    // Claims / CustomClaims serde
    // -------------------------------------------------------------------------

    #[test]
    fn claims_serde_roundtrip() {
        let claims = build_claims("user-1", "a@b.com", 1_700_000_000, 900, None);
        let json = serde_json::to_string(&claims).expect("serialize");
        let back: Claims = serde_json::from_str(&json).expect("deserialize");
        assert_claims_eq(&back, &claims);
    }

    #[test]
    fn claims_serde_omits_empty_jti() {
        let claims = build_claims("user-1", "a@b.com", 1_700_000_000, 900, None);
        let json = serde_json::to_string(&claims).expect("serialize");
        assert!(!json.contains("jti"));
    }

    #[test]
    fn custom_claims_serde_roundtrip() {
        let claims = build_custom_claims(
            "user-2",
            "tenant@example.com",
            1_700_000_000,
            3600,
            Some("jid-1".into()),
            Some("tenant-a".into()),
        );
        let json = serde_json::to_string(&claims).expect("serialize");
        let back: CustomClaims = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, claims);
    }

    #[test]
    fn custom_claims_serde_omits_none_tenant_id() {
        let claims = build_custom_claims("u", "e@x.com", 100, 50, None, None);
        let json = serde_json::to_string(&claims).expect("serialize");
        assert!(!json.contains("tenant_id"));
    }

    #[test]
    fn custom_claims_from_standard_claims() {
        let standard = build_claims("sub", "e@mail.com", 10, 20, Some("j".into()));
        let custom: CustomClaims = standard.clone().into();
        assert_claims_eq(&custom.to_claims(), &standard);
        assert!(custom.tenant_id.is_none());
    }

    #[test]
    fn claims_deserialize_without_jti() {
        let claims: Claims = serde_json::from_str(
            r#"{"sub":"legacy-user","email":"legacy@example.com","exp":9999999999,"iat":1000000000}"#,
        )
        .expect("legacy claims without jti should deserialize");
        assert!(claims.jti.is_empty());
    }

    // -------------------------------------------------------------------------
    // build_claims
    // -------------------------------------------------------------------------

    #[test]
    fn build_claims_sets_exp_from_ttl() {
        let claims = build_claims("u", "e@x.com", 1_000, 900, None);
        assert_eq!(claims.iat, 1_000);
        assert_eq!(claims.exp, 1_900);
    }

    #[test]
    fn build_claims_negative_ttl_clamps_to_iat() {
        let claims = build_claims("u", "e@x.com", 500, -10, None);
        assert_eq!(claims.exp, 500);
    }

    #[test]
    fn build_claims_preserves_jti_when_provided() {
        let claims = build_claims("u", "e@x.com", 1, 2, Some("jid".into()));
        assert_eq!(claims.jti, "jid");
    }

    // -------------------------------------------------------------------------
    // extract_subject
    // -------------------------------------------------------------------------

    #[test]
    fn extract_subject_returns_sub() {
        let claims = build_claims("user-42", "e@x.com", 1, 2, None);
        assert_eq!(extract_subject(&claims).expect("subject"), "user-42");
    }

    #[test]
    fn extract_subject_rejects_whitespace_only() {
        let claims = build_claims("   ", "e@x.com", 1, 2, None);
        assert!(matches!(
            extract_subject(&claims),
            Err(JwtError::InvalidClaims(_))
        ));
    }

    // -------------------------------------------------------------------------
    // validate_expiration
    // -------------------------------------------------------------------------

    #[test]
    fn validate_expiration_accepts_future_exp() {
        let now = now_ts();
        assert!(validate_expiration(now + 3600, 0, now).is_ok());
    }

    #[test]
    fn validate_expiration_rejects_past_exp() {
        let now = now_ts();
        assert_eq!(
            validate_expiration(now - 120, 0, now),
            Err(JwtError::Expired)
        );
    }

    #[test]
    fn validate_expiration_respects_leeway() {
        let now = now_ts();
        assert!(validate_expiration(now - 30, 60, now).is_ok());
        assert_eq!(
            validate_expiration(now - 90, 60, now),
            Err(JwtError::Expired)
        );
    }

    // -------------------------------------------------------------------------
    // validate_tenant
    // -------------------------------------------------------------------------

    #[test]
    fn validate_tenant_accepts_matching_id() {
        let claims = build_custom_claims("u", "e@x.com", 1, 2, None, Some("tenant-1".into()));
        assert!(validate_tenant(&claims, "tenant-1").is_ok());
    }

    #[test]
    fn validate_tenant_rejects_mismatch() {
        let claims = build_custom_claims("u", "e@x.com", 1, 2, None, Some("tenant-a".into()));
        let err = validate_tenant(&claims, "tenant-b").unwrap_err();
        assert!(matches!(err, JwtError::InvalidClaims(_)));
        assert!(err.to_string().contains("tenant mismatch"));
    }

    #[test]
    fn validate_tenant_rejects_missing_tenant_id() {
        let claims = build_custom_claims("u", "e@x.com", 1, 2, None, None);
        assert!(matches!(
            validate_tenant(&claims, "tenant-1"),
            Err(JwtError::InvalidClaims(_))
        ));
    }

    // -------------------------------------------------------------------------
    // verify_signature / sign roundtrip
    // -------------------------------------------------------------------------

    #[test]
    fn verify_signature_roundtrip_valid_token() {
        let claims = build_claims("roundtrip", "r@example.com", now_ts(), 900, None);
        let token = sign_claims(&claims, test_secret()).expect("sign");
        let decoded = verify_signature(&token, test_secret(), 60).expect("verify");
        assert_eq!(decoded.sub, "roundtrip");
        assert_eq!(decoded.email, "r@example.com");
    }

    #[test]
    fn verify_signature_custom_claims_roundtrip() {
        let custom = build_custom_claims(
            "u",
            "e@x.com",
            now_ts(),
            900,
            Some("jti-1".into()),
            Some("tenant-z".into()),
        );
        let token = sign_custom_claims(&custom, test_secret());
        let decoded =
            verify_signature(&token, test_secret(), 0).expect("verify custom token");
        assert_eq!(decoded.sub, "u");
        validate_tenant(
            &CustomClaims {
                tenant_id: Some("tenant-z".into()),
                ..custom
            },
            "tenant-z",
        )
        .expect("tenant ok");
    }

    #[test]
    fn verify_signature_wrong_secret_is_invalid_signature() {
        let claims = build_claims("u", "e@x.com", now_ts(), 60, None);
        let token = sign_claims(&claims, test_secret()).expect("sign");
        let err = verify_signature(&token, b"other-secret-at-least-32-bytes-long!!", 0).unwrap_err();
        assert_eq!(err, JwtError::InvalidSignature);
    }

    #[test]
    fn verify_signature_tampered_payload_fails() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user", "user@example.com")
            .expect("generate");
        let mut segments: Vec<String> = tokens.access_token.split('.').map(String::from).collect();
        segments[1].push('x');
        let tampered = segments.join(".");
        let err = verify_signature(&tampered, test_secret(), 0).unwrap_err();
        assert!(matches!(
            err,
            JwtError::InvalidSignature | JwtError::InvalidClaims(_)
        ));
    }

    #[test]
    fn verify_signature_expired_token() {
        let claims = build_claims(
            "expired",
            "e@x.com",
            now_ts() - 200,
            -120,
            None,
        );
        let token = sign_claims(&claims, test_secret()).expect("sign");
        assert!(matches!(
            verify_signature(&token, test_secret(), 0),
            Err(JwtError::Expired)
        ));
    }

    #[test]
    fn verify_signature_rejects_non_hs256_algorithm() {
        let claims = build_claims("u", "e@x.com", now_ts(), 60, None);
        let token = encode(
            &Header::new(Algorithm::HS384),
            &claims,
            &EncodingKey::from_secret(test_secret()),
        )
        .expect("hs384 token");
        let err = verify_signature(&token, test_secret(), 0).unwrap_err();
        assert!(matches!(err, JwtError::InvalidClaims(_)));
        assert!(err.to_string().contains("HS256"));
    }

    #[test]
    fn verify_signature_rejects_malformed_token() {
        let err = verify_signature("not-a-jwt", test_secret(), 0).unwrap_err();
        assert!(matches!(err, JwtError::InvalidClaims(_)));
    }

    // -------------------------------------------------------------------------
    // JwtError display / debug / clone
    // -------------------------------------------------------------------------

    #[test]
    fn jwt_error_expired_display() {
        assert_eq!(JwtError::Expired.to_string(), "token expired");
    }

    #[test]
    fn jwt_error_invalid_signature_display() {
        assert_eq!(
            JwtError::InvalidSignature.to_string(),
            "invalid signature"
        );
    }

    #[test]
    fn jwt_error_invalid_claims_display_includes_detail() {
        let msg = JwtError::InvalidClaims("bad alg".into()).to_string();
        assert!(msg.contains("invalid claims"));
        assert!(msg.contains("bad alg"));
    }

    #[test]
    fn jwt_error_debug_clone() {
        let err = JwtError::Expired;
        let cloned = err.clone();
        let mut dbg = String::new();
        write!(&mut dbg, "{err:?}").unwrap();
        assert!(dbg.contains("Expired"));
        assert_eq!(cloned, err);
    }

    #[test]
    fn custom_claims_debug_clone() {
        let claims = build_custom_claims("u", "e@x.com", 1, 2, None, Some("t".into()));
        let cloned = claims.clone();
        let dbg = format!("{claims:?}");
        assert!(dbg.contains("tenant_id"));
        assert_eq!(cloned, claims);
    }

    #[test]
    fn claims_clone_preserves_fields() {
        let claims = build_claims("u", "e@x.com", 3, 4, Some("j".into()));
        let cloned = claims.clone();
        assert_eq!(cloned.sub, "u");
        assert_eq!(cloned.jti, "j");
    }


    #[test]
    fn validate_expiration_boundary_exactly_at_leeway() {
        let now = now_ts();
        assert!(validate_expiration(now - 60, 60, now).is_ok());
    }

    #[test]
    fn extract_subject_rejects_empty_string() {
        let claims = build_claims("", "e@x.com", 1, 2, None);
        assert_eq!(
            extract_subject(&claims),
            Err(JwtError::InvalidClaims("missing subject".into()))
        );
    }

    #[test]
    fn build_custom_claims_carries_tenant_id() {
        let claims = build_custom_claims("u", "e@x.com", 5, 10, None, Some("tenant-9".into()));
        assert_eq!(claims.tenant_id.as_deref(), Some("tenant-9"));
    }

    // -------------------------------------------------------------------------
    // AuthService integration (existing coverage, helper-backed)
    // -------------------------------------------------------------------------

    #[test]
    fn test_password_hashing() {
        let service = create_test_service();
        let password = "test_password_123";
        let hash = service.hash_password(password).expect("hash");
        assert_ne!(hash, password);
        assert!(hash.starts_with("$argon2"));
    }

    #[test]
    fn test_password_verification_success() {
        let service = create_test_service();
        let password = "secure_password_456";
        let hash = service.hash_password(password).expect("hash");
        assert!(service.verify_password(password, &hash).expect("verify"));
    }

    #[test]
    fn test_password_verification_failure() {
        let service = create_test_service();
        let hash = service
            .hash_password("correct_password")
            .expect("hash");
        assert!(!service
            .verify_password("wrong_password", &hash)
            .expect("verify"));
    }

    #[test]
    fn test_token_generation() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user-123", "test@example.com")
            .expect("generate");
        assert!(!tokens.access_token.is_empty());
        assert!(!tokens.refresh_token.is_empty());
        assert_eq!(tokens.expires_in, 900);
        assert_ne!(tokens.access_token, tokens.refresh_token);
    }

    #[test]
    fn test_token_verification_success() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user-456", "user@test.com")
            .expect("generate");
        let claims = service
            .verify_token(&tokens.access_token)
            .expect("verify");
        assert_eq!(claims.sub, "user-456");
        assert_eq!(claims.email, "user@test.com");
    }

    #[test]
    fn test_token_verification_invalid_token() {
        let service = create_test_service();
        assert!(service.verify_token("invalid.token.here").is_err());
    }

    #[test]
    fn test_token_verification_wrong_secret() {
        let service1 = AuthService::new("secret-one-that-is-32-chars-long".into(), 900, 604_800);
        let service2 = AuthService::new("secret-two-that-is-32-chars-long".into(), 900, 604_800);
        let tokens = service1
            .generate_tokens("user-789", "test@example.com")
            .expect("generate");
        assert!(service2.verify_token(&tokens.access_token).is_err());
    }

    #[test]
    fn test_hash_token() {
        let service = create_test_service();
        let hash1 = service.hash_token("some-refresh-token");
        let hash2 = service.hash_token("some-refresh-token");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
        assert!(hash1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_token_different_inputs() {
        let service = create_test_service();
        assert_ne!(service.hash_token("token-a"), service.hash_token("token-b"));
    }

    #[test]
    fn test_refresh_tokens_are_unique() {
        let service = create_test_service();
        let tokens1 = service
            .generate_tokens("user-refresh-unique", "refresh@example.com")
            .expect("generate");
        let tokens2 = service
            .generate_tokens("user-refresh-unique", "refresh@example.com")
            .expect("generate");
        assert_ne!(tokens1.refresh_token, tokens2.refresh_token);
        let claims1 = service.verify_token(&tokens1.refresh_token).expect("verify");
        let claims2 = service.verify_token(&tokens2.refresh_token).expect("verify");
        assert_ne!(claims1.jti, claims2.jti);
    }

    #[test]
    fn test_claims_expiration() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user", "user@example.com")
            .expect("generate");
        let claims = service.verify_token(&tokens.access_token).expect("verify");
        let now = now_ts();
        assert!(claims.iat <= now && claims.iat >= now.saturating_sub(5));
        let expected_exp = claims.iat + 900;
        assert!(claims.exp >= expected_exp.saturating_sub(5));
        assert!(claims.exp <= expected_exp + 5);
    }

    #[test]
    fn test_jwt_encode_decode_roundtrip() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("roundtrip-user", "roundtrip@example.com")
            .expect("generate");
        let access = service.verify_token(&tokens.access_token).expect("access");
        let refresh = service.verify_token(&tokens.refresh_token).expect("refresh");
        assert_eq!(access.sub, "roundtrip-user");
        assert!(!refresh.jti.is_empty());
    }

    #[test]
    fn test_expired_token_rejected_with_zero_leeway() {
        let secret = TEST_SECRET.to_string();
        let service = AuthService::with_leeway(secret.clone(), 900, 604_800, 0);
        let expired_claims = Claims {
            sub: "expired-user".into(),
            email: "expired@example.com".into(),
            exp: (Utc::now() - Duration::from_secs(60)).timestamp() as usize,
            iat: (Utc::now() - Duration::from_secs(120)).timestamp() as usize,
            jti: String::new(),
        };
        let expired_token = encode(
            &Header::new(Algorithm::HS256),
            &expired_claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode");
        assert!(service.verify_token_with_leeway(&expired_token, 0).is_err());
    }

    #[test]
    fn test_claim_validation_rejects_tampered_payload() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user", "user@example.com")
            .expect("generate");
        let mut segments: Vec<String> = tokens.access_token.split('.').map(String::from).collect();
        segments[1].push('x');
        assert!(service.verify_token(&segments.join(".")).is_err());
    }

    #[test]
    fn test_access_token_claims_have_empty_jti() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user", "user@example.com")
            .expect("generate");
        let claims = service.verify_token(&tokens.access_token).expect("verify");
        assert!(claims.jti.is_empty());
    }

    #[test]
    fn auth_service_verify_maps_expired_to_auth_error() {
        let claims = build_claims("u", "e@x.com", now_ts() - 300, -60, None);
        let token = sign_claims(&claims, test_secret()).expect("sign");
        let err = create_test_service()
            .verify_token_with_leeway(&token, 0)
            .expect_err("expired");
        assert!(matches!(err, AppError::Auth(msg) if msg.contains("expired")));
    }

    #[test]
    fn auth_service_verify_maps_invalid_signature_to_auth_error() {
        let claims = build_claims("u", "e@x.com", now_ts(), 60, None);
        let token = sign_claims(&claims, test_secret()).expect("sign");
        let err = AuthService::new("different-secret-32-chars-minimum!!".into(), 900, 604_800)
            .verify_token_with_leeway(&token, 0)
            .expect_err("bad sig");
        assert!(matches!(err, AppError::Auth(msg) if msg.contains("invalid signature")));
    }
}
