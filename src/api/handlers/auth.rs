use crate::{
    db::traits::DatabaseClient,
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
    // Validate input
    if payload.email.is_empty() || payload.password.len() < 8 {
        return Err(AppError::InvalidInput(
            "Email required and password must be at least 8 characters".to_string(),
        ));
    }

    // Check if user exists
    if state
        .turso
        .get_user_by_email(&payload.email)
        .await?
        .is_some()
    {
        return Err(AppError::InvalidInput("User already exists".to_string()));
    }

    // Hash password
    let password_hash = state.auth_service.hash_password(&payload.password)?;

    // Create user
    let user_id = Uuid::new_v4().to_string();
    state
        .turso
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
        .turso
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
        .turso
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
        .turso
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
    state
        .turso
        .delete_session_by_token_hash(&token_hash)
        .await?;

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
        .turso
        .validate_session(&token_hash)
        .await?
        .ok_or_else(|| AppError::Auth("Refresh token has been revoked or expired".to_string()))?;

    // Ensure the token belongs to the claimed user
    if user_id != claims.sub {
        return Err(AppError::Auth("Token mismatch".to_string()));
    }

    // Invalidate the old refresh token (one-time use)
    state
        .turso
        .delete_session_by_token_hash(&token_hash)
        .await?;

    // Generate new tokens
    let tokens = state
        .auth_service
        .generate_tokens(&claims.sub, &claims.email)?;

    // Store the new refresh token in a new session
    let new_token_hash = state.auth_service.hash_token(&tokens.refresh_token);
    let session_id = Uuid::new_v4().to_string();
    state
        .turso
        .create_session(
            &session_id,
            &claims.sub,
            &new_token_hash,
            chrono::Utc::now().timestamp() + tokens.expires_in,
        )
        .await?;

    Ok(Json(tokens))
}
