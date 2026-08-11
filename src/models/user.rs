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
    /// Custom status: an optional emoji + short text, with an optional expiry.
    /// `#[sqlx(default)]` keeps queries that omit these columns working.
    #[serde(default)]
    #[sqlx(default)]
    pub status_emoji: Option<String>,
    #[serde(default)]
    #[sqlx(default)]
    pub status_text: Option<String>,
    #[serde(default)]
    #[sqlx(default)]
    pub status_expires_at: Option<DateTime<Utc>>,
    /// Per-user notification preferences (0.10.0). `#[sqlx(default)]` keeps
    /// queries that omit these columns working, and each defaults to the
    /// permissive value so behavior is unchanged until a user opts out.
    #[serde(default = "default_true")]
    #[sqlx(default)]
    pub notify_broadcast: bool,
    #[serde(default = "default_true")]
    #[sqlx(default)]
    pub notify_thread_reply: bool,
    #[serde(default = "default_true")]
    #[sqlx(default)]
    pub notify_dm: bool,
}

fn default_true() -> bool {
    true
}

impl User {
    pub fn to_public(&self) -> UserPublic {
        // Treat an expired status as absent so clients never render a stale one.
        let expired = self
            .status_expires_at
            .map(|exp| exp <= Utc::now())
            .unwrap_or(false);
        let (status_emoji, status_text, status_expires_at) = if expired {
            (None, None, None)
        } else {
            (
                self.status_emoji.clone(),
                self.status_text.clone(),
                self.status_expires_at,
            )
        };
        UserPublic {
            id: self.id,
            email: self.email.clone(),
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            role: self.role,
            avatar_url: self.avatar_url.clone(),
            is_active: self.is_active,
            created_at: self.created_at,
            status_emoji,
            status_text,
            status_expires_at,
            email_verified: None,
            notification_prefs: None,
            mfa_enabled: None,
        }
    }

    /// The caller's own notification preferences, as a small struct. Attached
    /// only to self endpoints (never exposed for other users).
    pub fn notification_prefs(&self) -> NotificationPrefs {
        NotificationPrefs {
            broadcast: self.notify_broadcast,
            thread_reply: self.notify_thread_reply,
            dm: self.notify_dm,
        }
    }
}

/// Per-user notification preferences. Each flag, when false, suppresses the
/// corresponding notification type for this user. Named @you mentions are
/// intentionally not represented here: personal pings are always delivered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NotificationPrefs {
    pub broadcast: bool,
    pub thread_reply: bool,
    pub dm: bool,
}

/// Body for `PATCH /users/me/notification-prefs`. All fields optional so a
/// client can update a single toggle; omitted fields are left unchanged.
#[derive(Debug, Deserialize)]
pub struct UpdateNotificationPrefsRequest {
    pub broadcast: Option<bool>,
    pub thread_reply: Option<bool>,
    pub dm: Option<bool>,
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
    #[serde(default)]
    pub status_emoji: Option<String>,
    #[serde(default)]
    pub status_text: Option<String>,
    #[serde(default)]
    pub status_expires_at: Option<DateTime<Utc>>,
    /// Only populated on the self endpoint (`/users/me`); None elsewhere so a
    /// user's verification state is never exposed to other users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_verified: Option<bool>,
    /// Only populated on self endpoints; None elsewhere so a user's
    /// preferences are never exposed to other users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_prefs: Option<NotificationPrefs>,
    /// Self-only: whether the caller has MFA enabled. None elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mfa_enabled: Option<bool>,
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

/// Login can either complete (tokens) or require a second factor. The client
/// distinguishes on `mfa_required`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum LoginOutcome {
    Authenticated(AuthResponse),
    MfaChallenge {
        mfa_required: bool,
        mfa_token: String,
    },
}

/// Body for `POST /auth/mfa/verify`: the challenge token + a TOTP code.
#[derive(Debug, Deserialize, Validate)]
pub struct MfaVerifyRequest {
    #[validate(length(min = 1))]
    pub mfa_token: String,
    #[validate(length(min = 6, max = 8))]
    pub code: String,
}

/// Body for MFA enroll confirm / disable: just a TOTP code.
#[derive(Debug, Deserialize, Validate)]
pub struct MfaCodeRequest {
    #[validate(length(min = 6, max = 8))]
    pub code: String,
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

/// Body for `PUT /users/me/status`. All fields optional: sending an empty body
/// (or all-null) clears the status. `expires_in_secs`, when present, sets an
/// auto-expiry that many seconds from now.
#[derive(Debug, Deserialize, Validate)]
pub struct SetStatusRequest {
    #[validate(length(max = 32))]
    pub emoji: Option<String>,
    #[validate(length(max = 100))]
    pub text: Option<String>,
    /// Optional auto-expiry, in seconds from now (e.g. 3600 = clear in an hour).
    pub expires_in_secs: Option<i64>,
}
