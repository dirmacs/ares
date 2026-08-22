//! HTTP mapping for [`ares_types::AppError`].
//!
//! `IntoResponse` cannot be implemented on the foreign `AppError` type here
//! (orphan rule). Handlers return [`HttpError`], which converts from `AppError`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// HTTP adapter around [`ares_types::AppError`].
#[derive(Debug)]
pub struct HttpError(pub ares_types::AppError);

impl From<ares_types::AppError> for HttpError {
    fn from(value: ares_types::AppError) -> Self {
        Self(value)
    }
}

impl From<std::io::Error> for HttpError {
    fn from(err: std::io::Error) -> Self {
        Self(err.into())
    }
}

impl From<serde_json::Error> for HttpError {
    fn from(err: serde_json::Error) -> Self {
        Self(err.into())
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for HttpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        app_error_into_response(self.0)
    }
}

/// Map [`ares_types::AppError`] to an Axum response using [`ares_types::AppError::status_code`].
pub fn app_error_into_response(err: ares_types::AppError) -> Response {
    if matches!(
        err,
        ares_types::AppError::Database(_)
            | ares_types::AppError::LLM(_)
            | ares_types::AppError::Configuration(_)
            | ares_types::AppError::Internal(_)
    ) {
        tracing::error!(error = %err, code = ?err.code(), "Internal error occurred");
    }

    let status = StatusCode::from_u16(err.status_code())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = err.to_string();
    let body = serde_json::json!({
        "error": message,
        "code": err.code()
    });
    (status, Json(body)).into_response()
}
