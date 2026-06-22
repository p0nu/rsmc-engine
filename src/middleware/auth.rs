//! Authentication extractor.
//!
//! [`AuthUser`] is an Axum extractor that validates the `Authorization:
//! Bearer <token>` header and yields the authenticated user's id and role.
//! Any handler that takes an `AuthUser` argument is implicitly protected.

use crate::auth::TokenKind;
use crate::error::AppError;
use crate::models::user::{Permission, UserRole};
use crate::state::AppState;
use axum::{
    extract::FromRequestParts,
    http::request::Parts,
};
use uuid::Uuid;

/// The authenticated principal for a request.
#[derive(Debug, Clone, Copy)]
pub struct AuthUser {
    pub id: Uuid,
    pub role: UserRole,
}

impl AuthUser {
    /// Authorize a coarse-grained permission, returning `Forbidden` if denied.
    pub fn require(&self, perm: Permission) -> Result<(), AppError> {
        if self.role.can(perm) {
            Ok(())
        } else {
            Err(AppError::Forbidden("insufficient permissions".into()))
        }
    }
}

#[async_trait::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or(AppError::Unauthorized)?;

        let claims = state.jwt.verify(token, TokenKind::Access)?;

        Ok(AuthUser {
            id: claims.sub,
            role: claims.role,
        })
    }
}

/// Extractor variant that additionally requires the admin system role.
#[derive(Debug, Clone, Copy)]
pub struct AdminUser(pub AuthUser);

#[async_trait::async_trait]
impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.role != UserRole::Admin {
            return Err(AppError::Forbidden("admin role required".into()));
        }
        Ok(AdminUser(user))
    }
}
