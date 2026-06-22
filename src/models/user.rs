//! User, role, and permission domain models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Type;
use uuid::Uuid;
use validator::Validate;

/// System-wide role for a user. Workspace/channel-level membership roles are
/// tracked separately on the membership records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// Full administrative access to the instance.
    Admin,
    /// Standard member.
    Member,
    /// Read-only / restricted guest.
    Guest,
}

impl UserRole {
    /// Coarse-grained capability check used by middleware and services.
    pub fn can(&self, perm: Permission) -> bool {
        use Permission::*;
        match self {
            UserRole::Admin => true,
            UserRole::Member => matches!(
                perm,
                CreateChannel | SendMessage | UploadFile | ReadMessage | ManageOwnMessage
            ),
            UserRole::Guest => matches!(perm, SendMessage | ReadMessage | ManageOwnMessage),
        }
    }
}

/// Discrete capabilities checked across the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ManageUsers,
    CreateChannel,
    DeleteChannel,
    SendMessage,
    ReadMessage,
    ManageOwnMessage,
    UploadFile,
    ManageWebhooks,
}

/// Channel-level membership role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "member_role", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum MemberRole {
    Owner,
    Admin,
    Member,
}

/// The full user row as stored in the database. The password hash is never
/// serialized to API responses (see [`UserPublic`]).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub display_name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: UserRole,
    pub avatar_url: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    pub fn to_public(&self) -> UserPublic {
        UserPublic {
            id: self.id,
            email: self.email.clone(),
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            role: self.role,
            avatar_url: self.avatar_url.clone(),
            is_active: self.is_active,
            created_at: self.created_at,
        }
    }
}

/// Public projection of a user, safe to return in API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
    pub avatar_url: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct SignupRequest {
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 3, max = 32))]
    pub username: String,
    #[validate(length(min = 1, max = 64))]
    pub display_name: String,
    #[validate(length(min = 8, max = 128))]
    pub password: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 1))]
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub user: UserPublic,
}

#[derive(Debug, Deserialize, Validate)]
pub struct RefreshRequest {
    #[validate(length(min = 1))]
    pub refresh_token: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateUserRequest {
    #[validate(length(min = 1, max = 64))]
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}
