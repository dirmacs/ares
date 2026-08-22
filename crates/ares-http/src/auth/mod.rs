//! JWT Authentication and Middleware
//!
//! This module provides authentication infrastructure for the A.R.E.S API,
//! including JWT token generation/validation and Axum middleware.
//!
//! # Module Structure
//!
//! - [`auth::jwt`](crate::auth::jwt) - JWT token encoding, decoding, and claims
//! - [`auth::middleware`](crate::auth::middleware) - Axum layers and extractors for authentication
//!
//! # Security Features
//!
//! - **Password Hashing**: Uses Argon2id (memory-hard) for secure password storage
//! - **JWT Tokens**: HS256 signed tokens with configurable expiration
//! - **Claims**: Standard JWT claims plus custom user data
//!
//! # Usage
//!
//! ## Token Generation
//!
//! ```ignore
//! use ares::auth::jwt::{encode_jwt, Claims};
//!
//! let claims = Claims::new(user_id, username, &config.jwt_secret, expiry_hours);
//! let token = encode_jwt(&claims, &config.jwt_secret)?;
//! ```
//!
//! ## Middleware
//!
//! The `AuthLayer` middleware validates JWT tokens and injects `Claims` into
//! the request extensions:
//!
//! ```ignore
//! use ares::auth::middleware::AuthLayer;
//!
//! let app = Router::new()
//!     .route("/protected", get(handler))
//!     .layer(AuthLayer::new(jwt_secret));
//! ```
//!
//! ## Extracting Claims in Handlers
//!
//! ```ignore
//! async fn protected_handler(
//!     Extension(claims): Extension<Claims>,
//! ) -> impl IntoResponse {
//!     format!("Hello, {}!", claims.sub)
//! }
//! ```
//!
//! # Configuration
//!
//! Configure via `ares.toml`:
//! ```toml
//! [server]
//! jwt_secret = "your-secret-key"  # Required, use a strong random value
//! jwt_expiry_hours = 24           # Token validity duration
//! ```

/// JWT token generation, validation, and password hashing services.
pub mod jwt;
/// Authentication middleware and extractors for protected routes.
pub mod middleware;
