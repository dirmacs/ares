use crate::{
    types::{AppError, LoginRequest, RegisterRequest, Result, TokenResponse},
    AppState,
};
use std::sync::Arc;
use cordis::Context;
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

const REGISTER_VALIDATION_MSG: &str =
    "Email required and password must be at least 8 characters";
const LOGIN_VALIDATION_MSG: &str = "Email and password are required";
const LOGOUT_SUCCESS_MESSAGE: &str = "Logged out successfully";

/// Validates registration email and password before hitting the database.
fn validate_register_input(email: &str, password: &str) -> Result<()> {
    if email.is_empty() || !password_meets_minimum_length(password) {
        return Err(AppError::InvalidInput(REGISTER_VALIDATION_MSG.to_string()));
    }
    Ok(())
}

fn password_meets_minimum_length(password: &str) -> bool {
    password.len() >= 8
}

/// Validates login email and password before hitting the database.
fn validate_login_input(email: &str, password: &str) -> Result<()> {
    if email.is_empty() || password.is_empty() {
        return Err(AppError::InvalidInput(LOGIN_VALIDATION_MSG.to_string()));
    }
    Ok(())
}

/// Unix timestamp when a refresh-token session should expire.
fn session_expires_at(expires_in: i64) -> i64 {
    chrono::Utc::now().timestamp() + expires_in
}

fn user_already_exists_error() -> AppError {
    AppError::InvalidInput("User already exists".to_string())
}

fn invalid_credentials_error() -> AppError {
    AppError::Auth("Invalid credentials".to_string())
}

fn revoked_refresh_token_error() -> AppError {
    AppError::Auth("Refresh token has been revoked or expired".to_string())
}

fn build_logout_response() -> LogoutResponse {
    LogoutResponse {
        message: LOGOUT_SUCCESS_MESSAGE.to_string(),
    }
}

fn refresh_token_from_request(payload: &RefreshTokenRequest) -> &str {
    &payload.refresh_token
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
    State(ctx): State<Arc<Context>>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<TokenResponse>> {
    validate_register_input(&payload.email, &payload.password)?;

    // Check if user exists
    if ctx.get::<crate::db::PostgresClient>().expect("not provided").get_user_by_email(&payload.email).await?.is_some() {
        return Err(user_already_exists_error());
    }

    // Hash password
    let password_hash = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").hash_password(&payload.password)?;

    // Create user
    let user_id = Uuid::new_v4().to_string();
    ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .create_user(&user_id, &payload.email, &password_hash, &payload.name)
        .await?;

    // Generate tokens
    let tokens = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided")
        .generate_tokens(&user_id, &payload.email)?;

    // Store refresh token
    let token_hash = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .create_session(
            &session_id,
            &user_id,
            &token_hash,
            session_expires_at(tokens.expires_in),
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
    State(ctx): State<Arc<Context>>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<TokenResponse>> {
    validate_login_input(&payload.email, &payload.password)?;

    // Get user
    let user = ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .get_user_by_email(&payload.email)
        .await?
        .ok_or_else(invalid_credentials_error)?;

    // Verify password
    if !ctx.get::<crate::auth::jwt::AuthService>().expect("not provided")
        .verify_password(&payload.password, &user.password_hash)?
    {
        return Err(invalid_credentials_error());
    }

    // Generate tokens
    let tokens = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").generate_tokens(&user.id, &user.email)?;

    // Store refresh token
    let token_hash = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .create_session(
            &session_id,
            &user.id,
            &token_hash,
            session_expires_at(tokens.expires_in),
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
    State(ctx): State<Arc<Context>>,
    Json(payload): Json<LogoutRequest>,
) -> Result<Json<LogoutResponse>> {
    // Hash the refresh token and delete the session
    let token_hash = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").hash_token(&payload.refresh_token);

    // Attempt to delete the session - we don't error if it doesn't exist
    // (token may already be expired/revoked, which is fine for logout)
    ctx.get::<crate::db::PostgresClient>().expect("not provided").delete_session_by_token_hash(&token_hash).await?;

    Ok(Json(build_logout_response()))
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
    State(ctx): State<Arc<Context>>,
    Json(payload): Json<RefreshTokenRequest>,
) -> Result<Json<TokenResponse>> {
    let refresh_token = refresh_token_from_request(&payload);

    // Verify refresh token JWT signature and expiry
    let claims = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").verify_token(refresh_token)?;

    // Hash the refresh token and validate it exists in the database
    let token_hash = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").hash_token(refresh_token);
    let user_id = ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .validate_session(&token_hash)
        .await?
        .ok_or_else(revoked_refresh_token_error)?;

    validate_token_user_match(&user_id, &claims.sub)?;

    // Invalidate the old refresh token (one-time use)
    ctx.get::<crate::db::PostgresClient>().expect("not provided").delete_session_by_token_hash(&token_hash).await?;

    // Generate new tokens
    let tokens = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided")
        .generate_tokens(&claims.sub, &claims.email)?;

    // Store the new refresh token in a new session
    let new_token_hash = ctx.get::<crate::auth::jwt::AuthService>().expect("not provided").hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .create_session(
            &session_id,
            &claims.sub,
            &new_token_hash,
            session_expires_at(tokens.expires_in),
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
    #[test]
    fn validate_login_input_rejects_empty_email() {
        let err = validate_login_input("", "password").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains(LOGIN_VALIDATION_MSG));
    }

    #[test]
    fn validate_login_input_rejects_empty_password() {
        let err = validate_login_input("user@example.com", "").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn validate_login_input_accepts_non_empty_credentials() {
        assert!(validate_login_input("user@example.com", "secret").is_ok());
    }

    #[test]
    fn password_meets_minimum_length_boundary() {
        assert!(!password_meets_minimum_length("1234567"));
        assert!(password_meets_minimum_length("12345678"));
    }

    #[test]
    fn session_expires_at_adds_offset_to_now() {
        let before = chrono::Utc::now().timestamp();
        let expires = session_expires_at(3600);
        assert!(expires >= before + 3600);
        assert!(expires <= before + 3601);
    }

    #[test]
    fn user_already_exists_error_is_invalid_input() {
        let err = user_already_exists_error();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("User already exists"));
    }

    #[test]
    fn invalid_credentials_error_message() {
        let err = invalid_credentials_error();
        assert!(matches!(err, AppError::Auth(_)));
        assert!(err.to_string().contains("Invalid credentials"));
    }

    #[test]
    fn revoked_refresh_token_error_message() {
        let err = revoked_refresh_token_error();
        assert!(err.to_string().contains("revoked or expired"));
    }

    #[test]
    fn build_logout_response_uses_success_message() {
        let resp = build_logout_response();
        assert_eq!(resp.message, LOGOUT_SUCCESS_MESSAGE);
    }

    #[test]
    fn validate_register_input_rejects_empty_password() {
        let err = validate_register_input("user@example.com", "").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains(REGISTER_VALIDATION_MSG));
    }

    #[test]
    fn validate_token_user_match_rejects_whitespace_mismatch() {
        assert!(validate_token_user_match("user", "user ").is_err());
    }

    #[test]
    fn validate_token_user_match_accepts_both_empty() {
        assert!(validate_token_user_match("", "").is_ok());
    }

    #[test]
    fn validate_token_user_match_rejects_nonempty_vs_empty() {
        assert!(validate_token_user_match("user-1", "").is_err());
    }

    #[test]
    fn login_request_rejects_missing_email() {
        let err = serde_json::from_str::<LoginRequest>(r#"{"password":"pw"}"#).unwrap_err();
        assert!(err.to_string().contains("email"));
    }

    #[test]
    fn login_request_rejects_missing_password() {
        let err = serde_json::from_str::<LoginRequest>(r#"{"email":"a@b.co"}"#).unwrap_err();
        assert!(err.to_string().contains("password"));
    }

    #[test]
    fn register_request_rejects_missing_name() {
        let err =
            serde_json::from_str::<RegisterRequest>(r#"{"email":"a@b.co","password":"secret123"}"#)
                .unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn register_request_rejects_missing_email() {
        let err = serde_json::from_str::<RegisterRequest>(
            r#"{"password":"secret123","name":"Alice"}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("email"));
    }

    #[test]
    fn register_request_rejects_missing_password() {
        let err = serde_json::from_str::<RegisterRequest>(r#"{"email":"a@b.co","name":"Alice"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("password"));
    }

    #[test]
    fn token_response_deserializes_roundtrip() {
        let json = r#"{"access_token":"a","refresh_token":"r","expires_in":7200}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "a");
        assert_eq!(resp.refresh_token, "r");
        assert_eq!(resp.expires_in, 7200);
        let back = serde_json::to_string(&resp).unwrap();
        assert!(back.contains("\"expires_in\":7200"));
    }

    #[test]
    fn logout_request_rejects_missing_refresh_token() {
        let err = serde_json::from_str::<LogoutRequest>(r#"{}"#).unwrap_err();
        assert!(err.to_string().contains("refresh_token"));
    }

    #[test]
    fn logout_response_serializes_message_field() {
        let json = serde_json::to_string(&build_logout_response()).unwrap();
        assert!(json.contains("\"message\""));
        assert!(json.contains(LOGOUT_SUCCESS_MESSAGE));
    }

    #[test]
    fn refresh_token_request_accepts_empty_string_value() {
        let req: RefreshTokenRequest = serde_json::from_str(r#"{"refresh_token":""}"#).unwrap();
        assert!(req.refresh_token.is_empty());
    }

    #[test]
    fn refresh_token_from_request_returns_payload_field() {
        let req = RefreshTokenRequest {
            refresh_token: "rt-xyz".to_string(),
        };
        assert_eq!(refresh_token_from_request(&req), "rt-xyz");
    }

    #[test]
    fn login_validation_message_constant_matches_error() {
        assert_eq!(LOGIN_VALIDATION_MSG, "Email and password are required");
    }

    #[test]
    fn register_validation_message_constant_matches_error() {
        assert!(REGISTER_VALIDATION_MSG.contains("8 characters"));
    }

}
