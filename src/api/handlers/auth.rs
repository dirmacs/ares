use crate::{
    types::{AppError, LoginRequest, RegisterRequest, Result, TokenResponse},
    AppState,
};
use axum::{extract::State, Json};
use uuid::Uuid;

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

/// Refresh access token
pub async fn refresh_token(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<TokenResponse>> {
    let refresh_token = payload
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::InvalidInput("Refresh token required".to_string()))?;

    // Verify refresh token
    let claims = state.auth_service.verify_token(refresh_token)?;

    // Generate new tokens
    let tokens = state
        .auth_service
        .generate_tokens(&claims.sub, &claims.email)?;

    Ok(Json(tokens))
}
