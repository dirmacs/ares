use crate::types::{AppError, Claims, Result, TokenResponse};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};

pub struct AuthService {
    jwt_secret: String,
    access_expiry: i64,
    refresh_expiry: i64,
}

impl AuthService {
    pub fn new(jwt_secret: String, access_expiry: i64, refresh_expiry: i64) -> Self {
        Self {
            jwt_secret,
            access_expiry,
            refresh_expiry,
        }
    }

    pub fn hash_password(&self, password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();

        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|e| AppError::Auth(format!("Failed to hash password: {}", e)))
    }

    pub fn verify_password(&self, password: &str, hash: &str) -> Result<bool> {
        let parsed_hash = PasswordHash::new(hash)
            .map_err(|e| AppError::Auth(format!("Invalid password hash: {}", e)))?;

        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    }

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
