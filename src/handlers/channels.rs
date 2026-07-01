//! Channel and membership handlers.

use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::channel::{
    AddMemberRequest, Channel, ChannelType, CreateChannelRequest, CreateDirectRequest,
    ReadReceipt, UpdateChannelRequest,
};
use crate::models::user::{MemberRole, Permission};
use crate::models::notification::NotificationKind;
use crate::services::{access, notifications};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

/// `POST /api/v1/channels`
pub async fn create_channel(
    State(state): State<AppState>,
    user: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateChannelRequest>,
) -> AppResult<Json<Channel>> {
    user.require(Permission::CreateChannel)?;

    let channel_type = if req.private {
        ChannelType::Private
    } else {
        ChannelType::Public
    };

    let mut tx = state.db.begin().await?;

    let channel: Channel = sqlx::query_as::<_, Channel>(
        r#"
        INSERT INTO channels (id, name, topic, channel_type, created_by)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&req.name)
    .bind(&req.topic)
    .bind(channel_type)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db) if db.is_unique_violation() => {
            AppError::Conflict("a channel with that name already exists".into())
        }
        other => other.into(),
    })?;

    // Creator joins as owner.
    sqlx::query(
        "INSERT INTO channel_members (channel_id, user_id, role) VALUES ($1, $2, $3)",
    )
    .bind(channel.id)
    .bind(user.id)
    .bind(MemberRole::Owner)
    .execute(&mut *tx)
    .await?;

    // Add any requested initial members.
    for member_id in req.member_ids.iter().filter(|m| **m != user.id) {
        sqlx::query(
            "INSERT INTO channel_members (channel_id, user_id, role)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(channel.id)
        .bind(member_id)
        .bind(MemberRole::Member)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Json(channel))
}

/// `POST /api/v1/channels/direct` — create or fetch a 1:1 DM channel.
pub async fn create_direct(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateDirectRequest>,
) -> AppResult<Json<Channel>> {
    if req.user_id == user.id {
        return Err(AppError::BadRequest("cannot DM yourself".into()));
    }

    // Find an existing direct channel containing exactly these two users.
    let existing: Option<Channel> = sqlx::query_as::<_, Channel>(
        r#"
        SELECT c.id, c.name, c.topic, c.channel_type, c.created_by, c.created_at, c.updated_at
        FROM channels c
        WHERE c.channel_type = 'direct'
          AND (SELECT COUNT(*) FROM channel_members m WHERE m.channel_id = c.id) = 2
          AND EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id AND m.user_id = $1)
          AND EXISTS (SELECT 1 FROM channel_members m WHERE m.channel_id = c.id AND m.user_id = $2)
        LIMIT 1
        "#,
    )
    .bind(user.id)
    .bind(req.user_id)
    .fetch_optional(&state.db)
    .await?;

    if let Some(c) = existing {
        return Ok(Json(c));
    }

    let mut tx = state.db.begin().await?;
    let channel: Channel = sqlx::query_as::<_, Channel>(
        r#"
        INSERT INTO channels (id, name, topic, channel_type, created_by)
        VALUES ($1, NULL, NULL, 'direct', $2)
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;

    for uid in [user.id, req.user_id] {
        sqlx::query(
            "INSERT INTO channel_members (channel_id, user_id, role) VALUES ($1, $2, 'member')",
        )
        .bind(channel.id)
        .bind(uid)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(channel))
}

/// `POST /api/v1/channels/:id/read` — advance the caller's read cursor to now.
///
/// History loading already advances `last_read_at`, but when a channel is the
/// active view, live messages arrive without re-fetching history. The client
/// calls this so the server's unread accounting stays in step with what the
/// user has actually seen on screen.
pub async fn mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    access::require_member(&state.db, id, user.id).await?;
    let last_read_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "UPDATE channel_members SET last_read_at = now() \
         WHERE channel_id = $1 AND user_id = $2 RETURNING last_read_at",
    )
    .bind(id)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;

    // Also clear any unread notifications tied to this channel so the two
    // unread surfaces (per-channel + global notifications) can't disagree.
    sqlx::query(
        r#"
        UPDATE notifications
        SET read_at = now()
        WHERE user_id = $1
          AND read_at IS NULL
          AND (payload->>'channel_id')::uuid = $2
        "#,
    )
    .bind(user.id)
    .bind(id)
    .execute(&state.db)
    .await?;

    // Broadcast a read receipt to the channel so other members' "Seen" /
    // "Seen by N" indicators update in realtime.
    state.events.emit_channel(
        id,
        crate::models::ws_protocol::ServerEvent::Read {
            channel_id: id,
            user_id: user.id,
            last_read_at,
        },
    );

    Ok(super::ok())
}

/// `GET /api/v1/channels/:id/receipts` — read cursors for every member of the
/// channel. The client compares each `last_read_at` against message timestamps
/// to render "Seen" (DMs) or "Seen by N" (group channels).
pub async fn list_receipts(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<ReadReceipt>>> {
    access::require_member(&state.db, id, user.id).await?;
    let receipts = sqlx::query_as::<_, ReadReceipt>(
        "SELECT user_id, last_read_at FROM channel_members \
         WHERE channel_id = $1 AND last_read_at IS NOT NULL",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(receipts))
}

/// `GET /api/v1/channels` — list channels the current user belongs to, each
/// annotated with the caller's unread count and last-read timestamp.
pub async fn list_channels(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<crate::models::channel::ChannelWithMeta>>> {
    let channels = sqlx::query_as::<_, crate::models::channel::ChannelWithMeta>(
        r#"
        SELECT c.id, c.name, c.topic, c.channel_type, c.created_by,
               c.created_at, c.updated_at,
               m.last_read_at,
               (
                   SELECT COUNT(*)
                   FROM messages msg
                   WHERE msg.channel_id = c.id
                     AND msg.deleted_at IS NULL
                     AND msg.user_id <> $1
                     AND (m.last_read_at IS NULL OR msg.created_at > m.last_read_at)
               ) AS unread_count
        FROM channels c
        JOIN channel_members m ON m.channel_id = c.id
        WHERE m.user_id = $1
        ORDER BY c.updated_at DESC
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(channels))
}

/// `GET /api/v1/channels/:id`
pub async fn get_channel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Channel>> {
    access::require_member(&state.db, id, user.id).await?;
    let channel: Channel = sqlx::query_as::<_, Channel>(
        r#"
        SELECT id, name, topic, channel_type, created_by, created_at, updated_at
        FROM channels WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("channel".into()))?;
    Ok(Json(channel))
}

/// `PATCH /api/v1/channels/:id`
pub async fn update_channel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<UpdateChannelRequest>,
) -> AppResult<Json<Channel>> {
    access::require_channel_admin(&state.db, id, user.id).await?;
    let channel: Channel = sqlx::query_as::<_, Channel>(
        r#"
        UPDATE channels
        SET name = COALESCE($2, name), topic = COALESCE($3, topic)
        WHERE id = $1
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(req.name)
    .bind(req.topic)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(channel))
}

/// `GET /api/v1/channels/:id/members`
pub async fn list_members(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<crate::models::user::UserPublic>>> {
    access::require_member(&state.db, id, user.id).await?;
    let members: Vec<crate::models::user::User> = sqlx::query_as::<_, crate::models::user::User>(
        r#"
        SELECT u.id, u.email, u.username, u.display_name, u.password_hash, u.role,
               u.avatar_url, u.is_active, u.created_at, u.updated_at
        FROM users u
        JOIN channel_members m ON m.user_id = u.id
        WHERE m.channel_id = $1
        ORDER BY u.username
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(members.into_iter().map(|u| u.to_public()).collect()))
}

/// `POST /api/v1/channels/:id/members`
pub async fn add_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<AddMemberRequest>,
) -> AppResult<Json<serde_json::Value>> {
    access::require_channel_admin(&state.db, id, user.id).await?;
    let res = sqlx::query(
        "INSERT INTO channel_members (channel_id, user_id, role)
         VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(req.user_id)
    .execute(&state.db)
    .await?;

    // Notify the invited user only if newly added and not adding themselves.
    if res.rows_affected() > 0 && req.user_id != user.id {
        let _ = notifications::notify(
            &state.db,
            &state.events,
            req.user_id,
            NotificationKind::ChannelInvite,
            serde_json::json!({ "channel_id": id, "actor_id": user.id }),
        )
        .await;
    }
    Ok(super::ok())
}

/// `DELETE /api/v1/channels/:id/members/:user_id`
pub async fn remove_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, target)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<serde_json::Value>> {
    // Users may remove themselves; admins may remove anyone.
    if target != user.id {
        access::require_channel_admin(&state.db, id, user.id).await?;
    }
    sqlx::query("DELETE FROM channel_members WHERE channel_id = $1 AND user_id = $2")
        .bind(id)
        .bind(target)
        .execute(&state.db)
        .await?;
    Ok(super::ok())
}
