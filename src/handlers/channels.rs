//! Channel and membership handlers.

use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::channel::{
    AddMemberRequest, BrowsableChannel, Channel, ChannelType, CreateChannelRequest,
    CreateDirectRequest, ListChannelsQuery, MuteRequest, ReadReceipt, UpdateChannelRequest,
};
use crate::models::user::{MemberRole, Permission, UserPublic};
use crate::models::notification::NotificationKind;
use crate::models::ws_protocol::ServerEvent;
use crate::services::{access, notifications};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use uuid::Uuid;

/// Load a user's public projection for embedding in realtime member events.
async fn load_public(state: &AppState, id: Uuid) -> AppResult<UserPublic> {
    let user = sqlx::query_as::<_, crate::models::user::User>(
        r#"
        SELECT id, email, username, display_name, password_hash, role,
               avatar_url, is_active, created_at, updated_at,
               status_emoji, status_text, status_expires_at
        FROM users WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("user".into()))?;
    Ok(user.to_public())
}

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
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at,
                  archived_at, archived_by
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

/// `POST /api/v1/channels/direct` - create or fetch a 1:1 DM channel.
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
        SELECT c.id, c.name, c.topic, c.channel_type, c.created_by, c.created_at, c.updated_at,
               c.archived_at, c.archived_by
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
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at,
                  archived_at, archived_by
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

/// `POST /api/v1/channels/:id/read` - advance the caller's read cursor to now.
///
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

/// `PATCH /api/v1/channels/:id/mute` - mute or unmute this channel for the
/// caller. Muting suppresses all notifications from the channel (mentions,
/// DMs, thread replies, and `@channel` broadcasts) while still tracking unread
/// counts, so the sidebar can show quiet activity without interrupting.
pub async fn set_mute(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MuteRequest>,
) -> AppResult<Json<serde_json::Value>> {
    access::require_member(&state.db, id, user.id).await?;
    sqlx::query("UPDATE channel_members SET muted = $3 WHERE channel_id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.id)
        .bind(req.muted)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({ "muted": req.muted })))
}

/// `GET /api/v1/channels/:id/receipts` - read cursors for every member of the
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

/// `GET /api/v1/channels` - list channels the current user belongs to, each
/// annotated with the caller's unread count and last-read timestamp. Archived
/// channels are hidden unless `?include_archived=true` is passed.
pub async fn list_channels(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListChannelsQuery>,
) -> AppResult<Json<Vec<crate::models::channel::ChannelWithMeta>>> {
    let channels = sqlx::query_as::<_, crate::models::channel::ChannelWithMeta>(
        r#"
        SELECT c.id, c.name, c.topic, c.channel_type, c.created_by,
               c.created_at, c.updated_at, c.archived_at, c.archived_by,
               m.last_read_at,
               m.muted,
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
          AND ($2 OR c.archived_at IS NULL)
        ORDER BY c.updated_at DESC
        "#,
    )
    .bind(user.id)
    .bind(q.include_archived)
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
        SELECT id, name, topic, channel_type, created_by, created_at, updated_at,
               archived_at, archived_by
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
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at,
                  archived_at, archived_by
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
/// Query for `GET /channels/:id/members` pagination. `cursor` is the last
/// username seen; results continue strictly after it (case-insensitive order).
#[derive(Debug, serde::Deserialize)]
pub struct MembersQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct MembersPage {
    pub members: Vec<crate::models::user::UserPublic>,
    /// Pass back as `cursor` to fetch the next page; null when exhausted.
    pub next_cursor: Option<String>,
}

pub async fn list_members(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<MembersQuery>,
) -> AppResult<Json<MembersPage>> {
    access::require_member(&state.db, id, user.id).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    // Keyset paginate by username (the stable sort key). Fetch limit+1 to know
    // whether another page exists.
    let members: Vec<crate::models::user::User> = sqlx::query_as::<_, crate::models::user::User>(
        r#"
        SELECT u.id, u.email, u.username, u.display_name, u.password_hash, u.role,
               u.avatar_url, u.is_active, u.created_at, u.updated_at,
               u.status_emoji, u.status_text, u.status_expires_at
        FROM users u
        JOIN channel_members m ON m.user_id = u.id
        WHERE m.channel_id = $1
          AND ($2::text IS NULL OR u.username > $2)
        ORDER BY u.username
        LIMIT $3
        "#,
    )
    .bind(id)
    .bind(q.cursor.as_deref())
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;

    let has_more = members.len() as i64 > limit;
    let page: Vec<_> = members.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().map(|u| u.username.clone())
    } else {
        None
    };

    Ok(Json(MembersPage {
        members: page.into_iter().map(|u| u.to_public()).collect(),
        next_cursor,
    }))
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

    // Realtime: broadcast the new member so other members' rosters update live.
    if res.rows_affected() > 0 {
        if let Ok(joined) = load_public(&state, req.user_id).await {
            let name = joined.display_name.clone();
            state.events.emit_channel(
                id,
                ServerEvent::MemberJoined {
                    channel_id: id,
                    user: joined,
                },
            );
            super::messages::post_system_message(
                &state,
                id,
                format!("{name} joined the channel"),
            )
            .await;
        }
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

    // Last-admin guard: the final owner/admin can't leave while other members
    // remain, which would strand the channel with no one able to administer it.
    let target_role: Option<MemberRole> = sqlx::query_scalar(
        "SELECT role FROM channel_members WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(target)
    .fetch_optional(&state.db)
    .await?;

    if matches!(target_role, Some(MemberRole::Owner) | Some(MemberRole::Admin)) {
        let other_admins: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channel_members \
             WHERE channel_id = $1 AND user_id <> $2 AND role IN ('owner', 'admin')",
        )
        .bind(id)
        .bind(target)
        .fetch_one(&state.db)
        .await?;
        let other_members: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channel_members WHERE channel_id = $1 AND user_id <> $2",
        )
        .bind(id)
        .bind(target)
        .fetch_one(&state.db)
        .await?;
        if other_admins == 0 && other_members > 0 {
            return Err(AppError::BadRequest(
                "you're the last admin; promote another member before leaving".into(),
            ));
        }
    }

    sqlx::query("DELETE FROM channel_members WHERE channel_id = $1 AND user_id = $2")
        .bind(id)
        .bind(target)
        .execute(&state.db)
        .await?;

    state.events.emit_channel(
        id,
        ServerEvent::MemberLeft {
            channel_id: id,
            user_id: target,
        },
    );

    // Visible feed line. Distinguish a voluntary leave from an admin removal
    // (self-leave when the target removed themselves). The removing admin is
    // deliberately not named.
    if let Ok(who) = load_public(&state, target).await {
        let text = if target == user.id {
            format!("{} left the channel", who.display_name)
        } else {
            format!("{} was removed from the channel", who.display_name)
        };
        super::messages::post_system_message(&state, id, text).await;
    }
    Ok(super::ok())
}

/// Shared helper for archive/unarchive. Rejects DMs and groups (only named
/// public/private channels have a meaningful archived state).
async fn set_archived(
    state: &AppState,
    user: &AuthUser,
    id: Uuid,
    archived: bool,
) -> AppResult<Json<Channel>> {
    access::require_channel_admin(&state.db, id, user.id).await?;

    let channel_type: ChannelType =
        sqlx::query_scalar("SELECT channel_type FROM channels WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("channel".into()))?;

    if matches!(channel_type, ChannelType::Direct | ChannelType::Group) {
        return Err(AppError::BadRequest(
            "direct messages and groups cannot be archived".into(),
        ));
    }

    let (archived_at, archived_by) = if archived {
        (Some(chrono::Utc::now()), Some(user.id))
    } else {
        (None, None)
    };

    let channel: Channel = sqlx::query_as::<_, Channel>(
        r#"
        UPDATE channels
        SET archived_at = $2, archived_by = $3
        WHERE id = $1
        RETURNING id, name, topic, channel_type, created_by, created_at, updated_at,
                  archived_at, archived_by
        "#,
    )
    .bind(id)
    .bind(archived_at)
    .bind(archived_by)
    .fetch_one(&state.db)
    .await?;

    state.events.emit_channel(
        id,
        ServerEvent::ChannelArchived {
            channel_id: id,
            archived,
        },
    );

    Ok(Json(channel))
}

/// `POST /api/v1/channels/:id/archive` (channel-admin only).
pub async fn archive_channel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Channel>> {
    set_archived(&state, &user, id, true).await
}

/// `POST /api/v1/channels/:id/unarchive` (channel-admin only).
pub async fn unarchive_channel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Channel>> {
    set_archived(&state, &user, id, false).await
}

/// `POST /api/v1/channels/:id/join` - self-service join for public channels.
/// Private channels, DMs, groups, and archived channels reject the request.
pub async fn join_channel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let channel: Channel = sqlx::query_as::<_, Channel>(
        r#"
        SELECT id, name, topic, channel_type, created_by, created_at, updated_at,
               archived_at, archived_by
        FROM channels WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("channel".into()))?;

    if channel.channel_type != ChannelType::Public {
        return Err(AppError::Forbidden(
            "only public channels can be joined directly".into(),
        ));
    }
    if channel.archived_at.is_some() {
        return Err(AppError::BadRequest(
            "this channel is archived and can't be joined".into(),
        ));
    }

    let res = sqlx::query(
        "INSERT INTO channel_members (channel_id, user_id, role)
         VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(user.id)
    .execute(&state.db)
    .await?;

    // Broadcast only on a genuine first join.
    if res.rows_affected() > 0 {
        if let Ok(joined) = load_public(&state, user.id).await {
            let name = joined.display_name.clone();
            state.events.emit_channel(
                id,
                ServerEvent::MemberJoined {
                    channel_id: id,
                    user: joined,
                },
            );
            // Visible feed line: "<name> joined the channel".
            super::messages::post_system_message(
                &state,
                id,
                format!("{name} joined the channel"),
            )
            .await;
        }
    }
    Ok(super::ok())
}

/// `GET /api/v1/channels/browse` - the public channel directory. Returns every
/// non-archived public channel with its member count and whether the caller is
/// already a member, so the client can render Join/Open affordances.
pub async fn browse_channels(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<BrowsableChannel>>> {
    let channels = sqlx::query_as::<_, BrowsableChannel>(
        r#"
        SELECT c.id, c.name, c.topic, c.channel_type, c.created_by,
               c.created_at, c.updated_at,
               (SELECT COUNT(*) FROM channel_members m WHERE m.channel_id = c.id) AS member_count,
               EXISTS (
                   SELECT 1 FROM channel_members m
                   WHERE m.channel_id = c.id AND m.user_id = $1
               ) AS is_member
        FROM channels c
        WHERE c.channel_type = 'public'
          AND c.archived_at IS NULL
        ORDER BY c.name
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(channels))
}
