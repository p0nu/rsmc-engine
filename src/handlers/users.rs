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
     status_emoji, status_text, status_expires_at";

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
    Ok(Json(load_user(&state, user.id).await?.to_public()))
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
    AdminUser(_admin): AdminUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SetRoleRequest>,
) -> AppResult<Json<UserPublic>> {
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

    Ok(Json(updated.to_public()))
}
