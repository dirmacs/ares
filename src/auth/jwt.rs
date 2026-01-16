use crate::types::{AppError, Claims, Result, TokenResponse};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};

/// Authentication service for JWT token management and password hashing.
///
/// Provides secure password hashing using Argon2id and JWT token
/// generation/verification using HS256.
pub struct AuthService {
    jwt_secret: String,
    access_expiry: i64,
    refresh_expiry: i64,
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
        let claims = Claims {
            sub: user_id.to_string(),
            email: email.to_string(),
            exp: (Utc::now() + Duration::seconds(self.access_expiry)).timestamp() as usize,
            iat: Utc::now().timestamp() as usize,
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )
        .map_err(|e| AppError::Auth(format!("Failed to generate token: {}", e)))
    }

    fn generate_refresh_token(&self, user_id: &str, email: &str) -> Result<String> {
        let claims = Claims {
            sub: user_id.to_string(),
            email: email.to_string(),
            exp: (Utc::now() + Duration::seconds(self.refresh_expiry)).timestamp() as usize,
            iat: Utc::now().timestamp() as usize,
        };

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )
        .map_err(|e| AppError::Auth(format!("Failed to generate refresh token: {}", e)))
    }

    /// Verifies a JWT token and returns the claims.
    pub fn verify_token(&self, token: &str) -> Result<Claims> {
        let validation = Validation::new(Algorithm::HS256);

        decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
            &validation,
        )
        .map(|data| data.claims)
        .map_err(|e| AppError::Auth(format!("Invalid token: {}", e)))
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

    fn create_test_service() -> AuthService {
        AuthService::new(
            "test-secret-key-that-is-at-least-32-chars".to_string(),
            900,    // 15 minutes
            604800, // 7 days
        )
    }

    #[test]
    fn test_password_hashing() {
        let service = create_test_service();
        let password = "test_password_123";

        let hash = service
            .hash_password(password)
            .expect("should hash password");

        // Hash should not equal the original password
        assert_ne!(hash, password);

        // Hash should be in PHC format (starts with $argon2)
        assert!(hash.starts_with("$argon2"), "hash should be in PHC format");
    }

    #[test]
    fn test_password_verification_success() {
        let service = create_test_service();
        let password = "secure_password_456";

        let hash = service
            .hash_password(password)
            .expect("should hash password");
        let is_valid = service
            .verify_password(password, &hash)
            .expect("should verify");

        assert!(is_valid, "correct password should verify successfully");
    }

    #[test]
    fn test_password_verification_failure() {
        let service = create_test_service();
        let password = "correct_password";
        let wrong_password = "wrong_password";

        let hash = service
            .hash_password(password)
            .expect("should hash password");
        let is_valid = service
            .verify_password(wrong_password, &hash)
            .expect("should verify");

        assert!(!is_valid, "wrong password should fail verification");
    }

    #[test]
    fn test_token_generation() {
        let service = create_test_service();
        let user_id = "user-123";
        let email = "test@example.com";

        let tokens = service
            .generate_tokens(user_id, email)
            .expect("should generate tokens");

        assert!(
            !tokens.access_token.is_empty(),
            "access token should not be empty"
        );
        assert!(
            !tokens.refresh_token.is_empty(),
            "refresh token should not be empty"
        );
        assert_eq!(
            tokens.expires_in, 900,
            "expires_in should match configured access expiry"
        );

        // Tokens should be different
        assert_ne!(
            tokens.access_token, tokens.refresh_token,
            "access and refresh tokens should differ"
        );
    }

    #[test]
    fn test_token_verification_success() {
        let service = create_test_service();
        let user_id = "user-456";
        let email = "user@test.com";

        let tokens = service
            .generate_tokens(user_id, email)
            .expect("should generate tokens");
        let claims = service
            .verify_token(&tokens.access_token)
            .expect("should verify token");

        assert_eq!(claims.sub, user_id, "subject should match user_id");
        assert_eq!(claims.email, email, "email should match");
    }

    #[test]
    fn test_token_verification_invalid_token() {
        let service = create_test_service();

        let result = service.verify_token("invalid.token.here");

        assert!(result.is_err(), "invalid token should fail verification");
    }

    #[test]
    fn test_token_verification_wrong_secret() {
        let service1 =
            AuthService::new("secret-one-that-is-32-chars-long".to_string(), 900, 604800);
        let service2 =
            AuthService::new("secret-two-that-is-32-chars-long".to_string(), 900, 604800);

        let tokens = service1
            .generate_tokens("user-789", "test@example.com")
            .expect("should generate");
        let result = service2.verify_token(&tokens.access_token);

        assert!(result.is_err(), "token from different secret should fail");
    }

    #[test]
    fn test_hash_token() {
        let service = create_test_service();
        let token = "some-refresh-token";

        let hash1 = service.hash_token(token);
        let hash2 = service.hash_token(token);

        // Same token should produce same hash
        assert_eq!(hash1, hash2, "same token should hash to same value");

        // Hash should be a hex string (64 chars for SHA256)
        assert_eq!(hash1.len(), 64, "SHA256 hash should be 64 hex characters");
        assert!(
            hash1.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex"
        );
    }

    #[test]
    fn test_hash_token_different_inputs() {
        let service = create_test_service();

        let hash1 = service.hash_token("token-a");
        let hash2 = service.hash_token("token-b");

        assert_ne!(
            hash1, hash2,
            "different tokens should have different hashes"
        );
    }

    #[test]
    fn test_claims_expiration() {
        let service = create_test_service();
        let tokens = service
            .generate_tokens("user", "user@example.com")
            .expect("should generate");
        let claims = service
            .verify_token(&tokens.access_token)
            .expect("should verify");

        let now = chrono::Utc::now().timestamp() as usize;

        // iat should be around now
        assert!(
            claims.iat <= now && claims.iat >= now - 5,
            "iat should be current timestamp"
        );

        // exp should be iat + access_expiry (900 seconds)
        let expected_exp = claims.iat + 900;
        assert!(
            claims.exp >= expected_exp - 5 && claims.exp <= expected_exp + 5,
            "exp should be iat + 900 seconds"
        );
    }
}
