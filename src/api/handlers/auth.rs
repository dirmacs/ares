use crate::{
    types::{AppError, LoginRequest, RegisterRequest, Result, TokenResponse},
    AppState,
};
use axum::{extract::State, Json};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

/// Request payload for refreshing an access token
#[derive(Debug, Deserialize, ToSchema)]
pub struct RefreshTokenRequest {
    /// The refresh token issued during login or registration
    pub refresh_token: String,
}

/// Validates registration email and password before hitting the database.
fn validate_register_input(email: &str, password: &str) -> Result<()> {
    if email.is_empty() || password.len() < 8 {
        return Err(AppError::InvalidInput(
            "Email required and password must be at least 8 characters".to_string(),
        ));
    }
    Ok(())
}

/// Ensures the refresh-token session user matches JWT subject claims.
fn validate_token_user_match(session_user_id: &str, claims_sub: &str) -> Result<()> {
    if session_user_id != claims_sub {
        return Err(AppError::Auth("Token mismatch".to_string()));
    }
    Ok(())
}

/// Register a new user
#[utoipa::path(
    post,
    path = "/api/auth/register",
    request_body = RegisterRequest,
    responses(
        (status = 200, description = "User registered successfully", body = TokenResponse),
        (status = 400, description = "Invalid input"),
        (status = 409, description = "User already exists")
    ),
    tag = "auth"
)]
pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<TokenResponse>> {
    validate_register_input(&payload.email, &payload.password)?;

    // Check if user exists
    if state.db.get_user_by_email(&payload.email).await?.is_some() {
        return Err(AppError::InvalidInput("User already exists".to_string()));
    }

    // Hash password
    let password_hash = state.auth_service.hash_password(&payload.password)?;

    // Create user
    let user_id = Uuid::new_v4().to_string();
    state
        .db
        .create_user(&user_id, &payload.email, &password_hash, &payload.name)
        .await?;

    // Generate tokens
    let tokens = state
        .auth_service
        .generate_tokens(&user_id, &payload.email)?;

    // Store refresh token
    let token_hash = state.auth_service.hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    state
        .db
        .create_session(
            &session_id,
            &user_id,
            &token_hash,
            chrono::Utc::now().timestamp() + tokens.expires_in,
        )
        .await?;

    Ok(Json(tokens))
}

/// Login with email and password
#[utoipa::path(
    post,
    path = "/api/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Login successful", body = TokenResponse),
        (status = 401, description = "Invalid credentials")
    ),
    tag = "auth"
)]
pub async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<TokenResponse>> {
    // Get user
    let user = state
        .db
        .get_user_by_email(&payload.email)
        .await?
        .ok_or_else(|| AppError::Auth("Invalid credentials".to_string()))?;

    // Verify password
    if !state
        .auth_service
        .verify_password(&payload.password, &user.password_hash)?
    {
        return Err(AppError::Auth("Invalid credentials".to_string()));
    }

    // Generate tokens
    let tokens = state.auth_service.generate_tokens(&user.id, &user.email)?;

    // Store refresh token
    let token_hash = state.auth_service.hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    state
        .db
        .create_session(
            &session_id,
            &user.id,
            &token_hash,
            chrono::Utc::now().timestamp() + tokens.expires_in,
        )
        .await?;

    Ok(Json(tokens))
}

/// Request payload for logout
#[derive(Debug, Deserialize, ToSchema)]
pub struct LogoutRequest {
    /// The refresh token to invalidate
    pub refresh_token: String,
}

/// Response for logout
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct LogoutResponse {
    /// Success message
    pub message: String,
}

/// Logout and invalidate refresh token
#[utoipa::path(
    post,
    path = "/api/auth/logout",
    request_body = LogoutRequest,
    responses(
        (status = 200, description = "Logout successful", body = LogoutResponse),
        (status = 401, description = "Invalid token")
    ),
    tag = "auth"
)]
pub async fn logout(
    State(state): State<AppState>,
    Json(payload): Json<LogoutRequest>,
) -> Result<Json<LogoutResponse>> {
    // Hash the refresh token and delete the session
    let token_hash = state.auth_service.hash_token(&payload.refresh_token);

    // Attempt to delete the session - we don't error if it doesn't exist
    // (token may already be expired/revoked, which is fine for logout)
    state.db.delete_session_by_token_hash(&token_hash).await?;

    Ok(Json(LogoutResponse {
        message: "Logged out successfully".to_string(),
    }))
}

/// Refresh access token
#[utoipa::path(
    post,
    path = "/api/auth/refresh",
    request_body = RefreshTokenRequest,
    responses(
        (status = 200, description = "Token refreshed successfully", body = TokenResponse),
        (status = 401, description = "Invalid or expired refresh token")
    ),
    tag = "auth"
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    Json(payload): Json<RefreshTokenRequest>,
) -> Result<Json<TokenResponse>> {
    let refresh_token = &payload.refresh_token;

    // Verify refresh token JWT signature and expiry
    let claims = state.auth_service.verify_token(refresh_token)?;

    // Hash the refresh token and validate it exists in the database
    let token_hash = state.auth_service.hash_token(refresh_token);
    let user_id = state
        .db
        .validate_session(&token_hash)
        .await?
        .ok_or_else(|| AppError::Auth("Refresh token has been revoked or expired".to_string()))?;

    validate_token_user_match(&user_id, &claims.sub)?;

    // Invalidate the old refresh token (one-time use)
    state.db.delete_session_by_token_hash(&token_hash).await?;

    // Generate new tokens
    let tokens = state
        .auth_service
        .generate_tokens(&claims.sub, &claims.email)?;

    // Store the new refresh token in a new session
    let new_token_hash = state.auth_service.hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    state
        .db
        .create_session(
            &session_id,
            &claims.sub,
            &new_token_hash,
            chrono::Utc::now().timestamp() + tokens.expires_in,
        )
        .await?;

    Ok(Json(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AppError;

    #[test]
    fn refresh_token_request_deserializes_from_json() {
        let json = r#"{"refresh_token":"rt-abc123"}"#;
        let req: RefreshTokenRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.refresh_token, "rt-abc123");
    }

    #[test]
    fn refresh_token_request_rejects_missing_field() {
        let err = serde_json::from_str::<RefreshTokenRequest>(r#"{}"#).unwrap_err();
        assert!(err.to_string().contains("refresh_token"));
    }

    #[test]
    fn logout_request_deserializes_from_json() {
        let json = r#"{"refresh_token":"logout-token"}"#;
        let req: LogoutRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.refresh_token, "logout-token");
    }

    #[test]
    fn logout_response_serializes_message() {
        let resp = LogoutResponse {
            message: "Logged out successfully".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Logged out successfully"));
    }

    #[test]
    fn validate_register_input_rejects_empty_email() {
        let err = validate_register_input("", "longpassword").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(
            err.to_string()
                .contains("Email required and password must be at least 8 characters")
        );
    }

    #[test]
    fn validate_register_input_rejects_short_password() {
        let err = validate_register_input("user@example.com", "short").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn validate_register_input_accepts_valid_payload() {
        assert!(validate_register_input("user@example.com", "password123").is_ok());
    }

    #[test]
    fn validate_token_user_match_rejects_mismatch() {
        let err = validate_token_user_match("user-a", "user-b").unwrap_err();
        assert!(matches!(err, AppError::Auth(_)));
        assert!(err.to_string().contains("Token mismatch"));
    }

    #[test]
    fn validate_token_user_match_accepts_matching_ids() {
        assert!(validate_token_user_match("same-user", "same-user").is_ok());
    }

    #[test]
    fn validate_register_input_does_not_depend_on_env() {
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("JWT_SECRET");
        assert!(validate_register_input("user@example.com", "password123").is_ok());
    }

    #[test]
    fn validate_register_input_accepts_password_exactly_eight_chars() {
        assert!(validate_register_input("user@example.com", "12345678").is_ok());
    }

    #[test]
    fn validate_register_input_rejects_password_seven_chars() {
        let err = validate_register_input("user@example.com", "1234567").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn register_request_deserializes_email_and_password() {
        let req: RegisterRequest =
            serde_json::from_str(r#"{"email":"a@b.co","password":"secret123","name":"Alice"}"#).unwrap();
        assert_eq!(req.email, "a@b.co");
        assert_eq!(req.password, "secret123");
        assert_eq!(req.name, "Alice");
    }

    #[test]
    fn login_request_deserializes_from_json() {
        let req: LoginRequest =
            serde_json::from_str(r#"{"email":"login@example.com","password":"pw"}"#).unwrap();
        assert_eq!(req.email, "login@example.com");
        assert_eq!(req.password, "pw");
    }

    #[test]
    fn token_response_serializes_token_fields() {
        let resp = TokenResponse {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_in: 3600,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"access_token\":\"access\""));
        assert!(json.contains("\"refresh_token\":\"refresh\""));
        assert!(json.contains("\"expires_in\":3600"));
    }
}
