//! User profile & management handlers.

use crate::error::{AppError, AppResult};
use crate::middleware::{AdminUser, AuthUser, ValidatedJson};
use crate::models::user::{SetStatusRequest, UpdateUserRequest, User, UserPublic, UserRole};
use crate::models::ws_protocol::ServerEvent;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

/// Column list shared by every user SELECT/RETURNING query, so the status and
/// avatar columns added in 0.7.0 are included consistently everywhere.
const USER_COLS: &str = "id, email, username, display_name, password_hash, role, \
     avatar_url, is_active, created_at, updated_at, \
     status_emoji, status_text, status_expires_at, \
     notify_broadcast, notify_thread_reply, notify_dm";

async fn load_user(state: &AppState, id: Uuid) -> AppResult<User> {
    sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLS} FROM users WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("user".into()))
}

/// `GET /api/v1/users/me`
pub async fn me(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<UserPublic>> {
    let full = load_user(&state, user.id).await?;
    let prefs = full.notification_prefs();
    let mut public = full.to_public();
    // Attach the caller's own verification state (self endpoint only).
    let verified: bool = sqlx::query_scalar("SELECT email_verified FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    public.email_verified = Some(verified);
    // Attach the caller's own notification preferences (self endpoint only).
    public.notification_prefs = Some(prefs);
    // Attach MFA state so the UI can show enable/disable correctly.
    let mfa: bool = sqlx::query_scalar("SELECT mfa_enabled FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    public.mfa_enabled = Some(mfa);
    Ok(Json(public))
}

/// `PATCH /api/v1/users/me/notification-prefs`
///
/// Update one or more of the caller's notification toggles. Omitted fields are
/// left unchanged (COALESCE). Returns the caller's updated public user with the
/// preferences attached, so the client can reflect the new state immediately.
pub async fn update_notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<crate::models::user::UpdateNotificationPrefsRequest>,
) -> AppResult<Json<UserPublic>> {
    let updated: User = sqlx::query_as::<_, User>(&format!(
        r#"
        UPDATE users
        SET notify_broadcast    = COALESCE($2, notify_broadcast),
            notify_thread_reply = COALESCE($3, notify_thread_reply),
            notify_dm           = COALESCE($4, notify_dm)
        WHERE id = $1
        RETURNING {USER_COLS}
        "#
    ))
    .bind(user.id)
    .bind(req.broadcast)
    .bind(req.thread_reply)
    .bind(req.dm)
    .fetch_one(&state.db)
    .await?;

    let prefs = updated.notification_prefs();
    let mut public = updated.to_public();
    public.notification_prefs = Some(prefs);
    Ok(Json(public))
}

/// `PATCH /api/v1/users/me`
pub async fn update_me(
    State(state): State<AppState>,
    user: AuthUser,
    ValidatedJson(req): ValidatedJson<UpdateUserRequest>,
) -> AppResult<Json<UserPublic>> {
    let updated: User = sqlx::query_as::<_, User>(&format!(
        r#"
        UPDATE users
        SET display_name = COALESCE($2, display_name),
            avatar_url   = COALESCE($3, avatar_url)
        WHERE id = $1
        RETURNING {USER_COLS}
        "#
    ))
    .bind(user.id)
    .bind(req.display_name)
    .bind(req.avatar_url)
    .fetch_one(&state.db)
    .await?;

    let public = updated.to_public();

    // Broadcast the profile change workspace-wide so every client can patch its
    // cached copies of this user (message authors, DM peers, member rosters).
    state.events.emit_all(ServerEvent::ProfileUpdated {
        user_id: public.id,
        display_name: public.display_name.clone(),
        avatar_url: public.avatar_url.clone(),
    });

    Ok(Json(public))
}

/// `PUT /api/v1/users/me/status` - set or clear the caller's custom status.
///
/// An empty body (or all-null fields) clears the status. When `expires_in_secs`
/// is provided, the status auto-expires that many seconds from now (the API
/// treats an expired status as absent). Emits `StatusChanged`.
pub async fn set_status(
    State(state): State<AppState>,
    user: AuthUser,
    ValidatedJson(req): ValidatedJson<SetStatusRequest>,
) -> AppResult<Json<UserPublic>> {
    // Normalize: blank strings clear that field.
    let emoji = req.emoji.filter(|s| !s.trim().is_empty());
    let text = req.text.filter(|s| !s.trim().is_empty());
    let expires_at = req
        .expires_in_secs
        .filter(|s| *s > 0)
        .map(|s| chrono::Utc::now() + chrono::Duration::seconds(s));

    let updated: User = sqlx::query_as::<_, User>(&format!(
        r#"
        UPDATE users
        SET status_emoji = $2, status_text = $3, status_expires_at = $4
        WHERE id = $1
        RETURNING {USER_COLS}
        "#
    ))
    .bind(user.id)
    .bind(&emoji)
    .bind(&text)
    .bind(expires_at)
    .fetch_one(&state.db)
    .await?;

    let public = updated.to_public();
    state.events.emit_all(ServerEvent::StatusChanged {
        user_id: public.id,
        emoji: public.status_emoji.clone(),
        text: public.status_text.clone(),
    });

    Ok(Json(public))
}

/// `GET /api/v1/users/:id`
pub async fn get_user(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UserPublic>> {
    Ok(Json(load_user(&state, id).await?.to_public()))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Optional case-insensitive search over username/display_name.
    pub q: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}
fn default_limit() -> i64 {
    50
}

/// `GET /api/v1/users`
pub async fn list_users(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<UserPublic>>> {
    let limit = q.limit.clamp(1, 200);
    let pattern = q.q.map(|s| format!("%{s}%"));

    let users: Vec<User> = sqlx::query_as::<_, User>(&format!(
        r#"
        SELECT {USER_COLS}
        FROM users
        WHERE ($1::text IS NULL
               OR username ILIKE $1
               OR display_name ILIKE $1)
        ORDER BY username
        LIMIT $2
        "#
    ))
    .bind(pattern)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(users.into_iter().map(|u| u.to_public()).collect()))
}

#[derive(Debug, Deserialize)]
pub struct SetRoleRequest {
    pub role: UserRole,
}

/// `PUT /api/v1/users/:id/role` (admin only)
pub async fn set_role(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SetRoleRequest>,
) -> AppResult<Json<UserPublic>> {
    // Capture the prior role so the audit entry records the transition.
    let old_role: Option<crate::models::user::UserRole> =
        sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let updated: User = sqlx::query_as::<_, User>(&format!(
        r#"
        UPDATE users SET role = $2
        WHERE id = $1
        RETURNING {USER_COLS}
        "#
    ))
    .bind(id)
    .bind(req.role)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("user".into()))?;

    crate::services::audit::record(
        &state.db,
        "role.changed",
        Some(admin.id),
        Some(crate::services::audit::Target::new(
            "user",
            updated.id,
            updated.username.clone(),
        )),
        serde_json::json!({
            "old_role": old_role.map(|r| format!("{r:?}")),
            "new_role": format!("{:?}", req.role),
        }),
    )
    .await;

    Ok(Json(updated.to_public()))
}

/// `POST /api/v1/users/:id/deactivate` (admin only)
pub async fn deactivate(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UserPublic>> {
    set_active(&state, admin, id, false).await
}

/// `POST /api/v1/users/:id/activate` (admin only) - reverses a deactivation.
pub async fn activate(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UserPublic>> {
    set_active(&state, admin, id, true).await
}

/// Shared implementation for (de)activating a user. Returns the updated public
/// user so the client can reflect the new state without a refetch. Guards
/// against an admin deactivating their own account (which would lock them out).
async fn set_active(
    state: &AppState,
    admin: crate::middleware::AuthUser,
    id: Uuid,
    active: bool,
) -> AppResult<Json<UserPublic>> {
    if !active && admin.id == id {
        return Err(AppError::BadRequest(
            "you cannot deactivate your own account".into(),
        ));
    }
    let updated: User = sqlx::query_as::<_, User>(&format!(
        r#"
        UPDATE users SET is_active = $2
        WHERE id = $1
        RETURNING {USER_COLS}
        "#
    ))
    .bind(id)
    .bind(active)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("user".into()))?;

    // On deactivation, revoke all of the user's refresh tokens so they can't
    // renew the session they currently hold; their short-lived access token
    // expires on its own shortly after.
    if !active {
        let _ = sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE user_id = $1 AND NOT revoked")
            .bind(id)
            .execute(&state.db)
            .await;
    }

    crate::services::audit::record(
        &state.db,
        if active { "user.activated" } else { "user.deactivated" },
        Some(admin.id),
        Some(crate::services::audit::Target::new(
            "user",
            updated.id,
            updated.username.clone(),
        )),
        serde_json::json!({ "is_active": active }),
    )
    .await;

    Ok(Json(updated.to_public()))
}

/// GET /api/v1/users/me/sessions
///
/// List the caller's active (non-revoked, unexpired) sessions with coarse
/// device info and last-used time. No IP is stored or returned.
pub async fn list_sessions(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let rows = sqlx::query_as::<_, SessionRow>(
        r#"
        SELECT id, user_agent, last_used_at, created_at, expires_at
        FROM refresh_tokens
        WHERE user_id = $1 AND NOT revoked AND expires_at > now()
        ORDER BY last_used_at DESC NULLS LAST, created_at DESC
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "sessions": rows })))
}

/// DELETE /api/v1/users/me/sessions/:id
///
/// Revoke one of the caller's sessions by id. Scoped to the caller so a user
/// can only revoke their own sessions.
pub async fn revoke_session(
    State(state): State<AppState>,
    user: AuthUser,
    Path(session_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let affected =
        sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE id = $1 AND user_id = $2 AND NOT revoked")
            .bind(session_id)
            .bind(user.id)
            .execute(&state.db)
            .await?
            .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("session".into()));
    }
    Ok(Json(serde_json::json!({ "revoked": affected })))
}

/// POST /api/v1/users/me/sessions/revoke-all
///
/// Revoke all of the caller's sessions. Their current short-lived access token
/// keeps working until it expires, after which they must sign in again. Useful
/// as a "sign out everywhere" control after a suspected compromise.
pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let affected =
        sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE user_id = $1 AND NOT revoked")
            .bind(user.id)
            .execute(&state.db)
            .await?
            .rows_affected();
    Ok(Json(serde_json::json!({ "revoked": affected })))
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    user_agent: Option<String>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

/// POST /api/v1/users/me/mfa/enroll
///
/// Begin TOTP enrollment: generate a secret, store it ENCRYPTED (mfa_enabled
/// stays false until confirmed), and return the provisioning URI + base32 for
/// the user's authenticator app. Re-enrolling overwrites any pending secret.
pub async fn mfa_enroll(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    // Refuse if already fully enabled (must disable first).
    let enabled: bool = sqlx::query_scalar("SELECT mfa_enabled FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    if enabled {
        return Err(AppError::BadRequest("MFA already enabled".into()));
    }

    let secret = crate::services::mfa::generate_secret();
    let enc = crate::services::mfa::encrypt_secret(&state.config.auth.jwt_secret, &secret)
        .map_err(|_| AppError::Internal("mfa secret encryption failed".into()))?;

    // Fetch a label for the provisioning URI.
    let username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;

    sqlx::query("UPDATE users SET mfa_secret_enc = $2, mfa_enabled = FALSE WHERE id = $1")
        .bind(user.id)
        .bind(&enc)
        .execute(&state.db)
        .await?;

    let uri = crate::services::mfa::provisioning_uri(&secret, &username, "RSMC");
    Ok(Json(serde_json::json!({
        "secret_base32": crate::services::mfa::secret_base32(&secret),
        "otpauth_uri": uri,
    })))
}

/// POST /api/v1/users/me/mfa/confirm
///
/// Confirm enrollment by proving a valid TOTP code against the pending secret.
/// Only then is MFA activated.
pub async fn mfa_confirm(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<crate::models::user::MfaCodeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let enc: Option<String> = sqlx::query_scalar("SELECT mfa_secret_enc FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    let enc = enc.ok_or_else(|| AppError::BadRequest("no pending MFA enrollment".into()))?;
    let secret = crate::services::mfa::decrypt_secret(&state.config.auth.jwt_secret, &enc)
        .map_err(|_| AppError::Internal("mfa secret decryption failed".into()))?;

    if !crate::services::mfa::verify(&secret, &req.code) {
        return Err(AppError::BadRequest("invalid code".into()));
    }

    sqlx::query("UPDATE users SET mfa_enabled = TRUE WHERE id = $1")
        .bind(user.id)
        .execute(&state.db)
        .await?;

    crate::services::audit::record(
        &state.db,
        "mfa.enabled",
        Some(user.id),
        None,
        serde_json::json!({}),
    )
    .await;

    Ok(Json(serde_json::json!({ "mfa_enabled": true })))
}

/// POST /api/v1/users/me/mfa/disable
///
/// Disable MFA. Requires a valid current TOTP code to prevent an attacker with
/// a hijacked session from silently removing the second factor.
pub async fn mfa_disable(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<crate::models::user::MfaCodeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let enc: Option<String> = sqlx::query_scalar(
        "SELECT mfa_secret_enc FROM users WHERE id = $1 AND mfa_enabled",
    )
    .bind(user.id)
    .fetch_optional(&state.db)
    .await?
    .flatten();
    let enc = enc.ok_or_else(|| AppError::BadRequest("MFA is not enabled".into()))?;
    let secret = crate::services::mfa::decrypt_secret(&state.config.auth.jwt_secret, &enc)
        .map_err(|_| AppError::Internal("mfa secret decryption failed".into()))?;

    if !crate::services::mfa::verify(&secret, &req.code) {
        return Err(AppError::BadRequest("invalid code".into()));
    }

    sqlx::query("UPDATE users SET mfa_enabled = FALSE, mfa_secret_enc = NULL WHERE id = $1")
        .bind(user.id)
        .execute(&state.db)
        .await?;

    crate::services::audit::record(
        &state.db,
        "mfa.disabled",
        Some(user.id),
        None,
        serde_json::json!({}),
    )
    .await;

    Ok(Json(serde_json::json!({ "mfa_enabled": false })))
}

/// GET /api/v1/users/me/export
///
/// GDPR data export: returns all of the caller's personal data as JSON.
pub async fn export_my_data(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let data = crate::services::gdpr::export_user_data(&state, user.id)
        .await
        .map_err(|e| AppError::Internal(format!("export failed: {e}")))?;
    Ok(Json(data))
}

/// DELETE /api/v1/users/me
///
/// GDPR account deletion. Behavior follows the instance policy
/// (config.gdpr.deletion_mode): anonymize (default) or hard_delete.
pub async fn delete_my_account(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let mode = state.config.gdpr.deletion_mode;
    crate::services::gdpr::delete_account(&state, user.id, mode)
        .await
        .map_err(|e| AppError::BadRequest(format!("account deletion failed: {e}")))?;

    crate::services::audit::record(
        &state.db,
        "account.deleted",
        Some(user.id),
        Some(crate::services::audit::Target::new("user", user.id, "self")),
        serde_json::json!({ "mode": format!("{:?}", mode) }),
    )
    .await;

    Ok(Json(serde_json::json!({
        "deleted": true,
        "mode": format!("{:?}", mode),
    })))
}
