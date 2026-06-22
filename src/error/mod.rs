//! Centralized error handling for the RSMC engine.
//!
//! A single [`AppError`] enum is used across the whole codebase. It implements
//! [`axum::response::IntoResponse`] so that any handler can return
//! `Result<T, AppError>` and have errors mapped to consistent JSON responses.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;

/// The canonical error type used throughout the application.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("authentication required")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("rate limited")]
    RateLimited,

    #[error("database error")]
    Database(#[from] sqlx::Error),

    #[error("jwt error")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("password hashing error: {0}")]
    PasswordHash(String),

    #[error("redis error: {0}")]
    Redis(String),

    #[error("serialization error")]
    Serialization(#[from] serde_json::Error),

    #[error("internal error: {0}")]
    Internal(String),

    /// An operational error from an admin-triggered action (e.g. a failed
    /// pg_dump). Returns 500 but, unlike `Internal`, surfaces its message to the
    /// caller because the detail is actionable for the operator and not a
    /// security-sensitive internal leak.
    #[error("{0}")]
    Operation(String),
}

/// Shape of the JSON error body returned to clients.
#[derive(Serialize)]
struct ErrorBody {
    error: String,
    message: String,
}

impl AppError {
    /// Maps each variant to an HTTP status code and a machine-readable code.
    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            AppError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_error"),
            AppError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            AppError::Jwt(_) => (StatusCode::UNAUTHORIZED, "invalid_token"),
            AppError::Database(sqlx::Error::RowNotFound) => (StatusCode::NOT_FOUND, "not_found"),
            AppError::Database(_)
            | AppError::PasswordHash(_)
            | AppError::Redis(_)
            | AppError::Serialization(_)
            | AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
            AppError::Operation(_) => (StatusCode::INTERNAL_SERVER_ERROR, "operation_failed"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();

        // Log server-side faults with full detail; keep client messages terse.
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed with server error");
        } else {
            tracing::debug!(error = %self, "request failed with client error");
        }

        let message = match (status.is_server_error(), &self) {
            // Operational admin errors carry an actionable message; surface it.
            (true, AppError::Operation(m)) => m.clone(),
            (true, _) => "an internal error occurred".to_string(),
            (false, _) => self.to_string(),
        };

        let body = Json(json!(ErrorBody {
            error: code.to_string(),
            message,
        }));

        (status, body).into_response()
    }
}

/// Convenience alias so handlers can write `AppResult<T>`.
pub type AppResult<T> = Result<T, AppError>;

impl From<argon2::password_hash::Error> for AppError {
    fn from(e: argon2::password_hash::Error) -> Self {
        AppError::PasswordHash(e.to_string())
    }
}

impl From<validator::ValidationErrors> for AppError {
    fn from(e: validator::ValidationErrors) -> Self {
        AppError::Validation(e.to_string())
    }
}

#[cfg(feature = "redis-pubsub")]
impl From<redis::RedisError> for AppError {
    fn from(e: redis::RedisError) -> Self {
        AppError::Redis(e.to_string())
    }
}
