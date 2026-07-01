//! Message handlers: send, history, threads, edit, delete.

use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::message::{
    EditMessageRequest, HistoryQuery, HistoryResponse, Message, MessageView, ReactionRequest,
    SendMessageRequest,
};
use crate::models::notification::NotificationKind;
use crate::models::user::{Permission, UserPublic};
use crate::models::ws_protocol::ServerEvent;
use crate::services::{access, notifications};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use uuid::Uuid;

/// Enrich raw messages with author + attachments in a bounded number of queries.
async fn enrich(state: &AppState, messages: Vec<Message>) -> AppResult<Vec<MessageView>> {
    if messages.is_empty() {
        return Ok(vec![]);
    }
    let author_ids: Vec<Uuid> = messages.iter().map(|m| m.user_id).collect();
    let message_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();

    // Fetch all distinct authors in one query.
    let authors: Vec<crate::models::user::User> = sqlx::query_as::<_, crate::models::user::User>(
        r#"
        SELECT id, email, username, display_name, password_hash, role,
               avatar_url, is_active, created_at, updated_at
        FROM users WHERE id = ANY($1)
        "#,
    )
    .bind(&author_ids)
    .fetch_all(&state.db)
    .await?;
    let author_map: std::collections::HashMap<Uuid, UserPublic> =
        authors.into_iter().map(|u| (u.id, u.to_public())).collect();

    // Fetch all attachments for these messages in one query.
    #[derive(sqlx::FromRow)]
    struct AttRow {
        message_id: Uuid,
        #[sqlx(flatten)]
        att: crate::models::file::Attachment,
    }
    let att_rows: Vec<AttRow> = sqlx::query_as::<_, AttRow>(
        r#"
        SELECT ma.message_id,
               a.id, a.uploader_id, a.channel_id, a.filename, a.content_type,
               a.size_bytes, a.storage_path, a.created_at
        FROM message_attachments ma
        JOIN attachments a ON a.id = ma.attachment_id
        WHERE ma.message_id = ANY($1)
        "#,
    )
    .bind(&message_ids)
    .fetch_all(&state.db)
    .await?;
    let mut att_map: std::collections::HashMap<Uuid, Vec<crate::models::file::Attachment>> =
        std::collections::HashMap::new();
    for row in att_rows {
        att_map.entry(row.message_id).or_default().push(row.att);
    }

    // Fetch + group reactions for these messages (ordered by first-reacted).
    #[derive(sqlx::FromRow)]
    struct ReactRow {
        message_id: Uuid,
        user_id: Uuid,
        emoji: String,
    }
    let react_rows: Vec<ReactRow> = sqlx::query_as::<_, ReactRow>(
        "SELECT message_id, user_id, emoji FROM reactions WHERE message_id = ANY($1) ORDER BY created_at",
    )
    .bind(&message_ids)
    .fetch_all(&state.db)
    .await?;
    let mut react_map: std::collections::HashMap<
        Uuid,
        Vec<crate::models::message::ReactionGroup>,
    > = std::collections::HashMap::new();
    for row in react_rows {
        let groups = react_map.entry(row.message_id).or_default();
        if let Some(g) = groups.iter_mut().find(|g| g.emoji == row.emoji) {
            g.count += 1;
            g.user_ids.push(row.user_id);
        } else {
            groups.push(crate::models::message::ReactionGroup {
                emoji: row.emoji,
                count: 1,
                user_ids: vec![row.user_id],
            });
        }
    }

    Ok(messages
        .into_iter()
        .map(|m| {
            let author = author_map
                .get(&m.user_id)
                .cloned()
                .unwrap_or_else(|| placeholder_user(m.user_id));
            let attachments = att_map.remove(&m.id).unwrap_or_default();
            let reactions = react_map.remove(&m.id).unwrap_or_default();
            MessageView {
                message: m,
                author,
                attachments,
                reactions,
            }
        })
        .collect())
}

fn placeholder_user(id: Uuid) -> UserPublic {
    UserPublic {
        id,
        email: String::new(),
        username: "unknown".into(),
        display_name: "Unknown User".into(),
        role: crate::models::user::UserRole::Member,
        avatar_url: None,
        is_active: false,
        created_at: chrono::Utc::now(),
    }
}

/// `POST /api/v1/channels/:id/messages`
pub async fn send_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<SendMessageRequest>,
) -> AppResult<Json<MessageView>> {
    user.require(Permission::SendMessage)?;
    access::require_member(&state.db, channel_id, user.id).await?;

    // A message must carry something: text, attachments, or both.
    if req.content.trim().is_empty() && req.attachment_ids.is_empty() {
        return Err(AppError::BadRequest(
            "message must have text or at least one attachment".into(),
        ));
    }

    // If replying, ensure the parent exists in this channel.
    if let Some(parent) = req.parent_id {
        let ok: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM messages WHERE id = $1 AND channel_id = $2")
                .bind(parent)
                .bind(channel_id)
                .fetch_optional(&state.db)
                .await?;
        if ok.is_none() {
            return Err(AppError::BadRequest("parent message not found".into()));
        }
    }

    let mut tx = state.db.begin().await?;
    let message: Message = sqlx::query_as::<_, Message>(
        r#"
        INSERT INTO messages (id, channel_id, user_id, content, parent_id)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, channel_id, user_id, content, parent_id, reply_count,
                  edited_at, deleted_at, created_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(channel_id)
    .bind(user.id)
    .bind(&req.content)
    .bind(req.parent_id)
    .fetch_one(&mut *tx)
    .await?;

    // Bump reply count + touch channel for ordering.
    if let Some(parent) = req.parent_id {
        sqlx::query("UPDATE messages SET reply_count = reply_count + 1 WHERE id = $1")
            .bind(parent)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE channels SET updated_at = now() WHERE id = $1")
        .bind(channel_id)
        .execute(&mut *tx)
        .await?;

    // Link attachments owned by this user that aren't already attached.
    for att_id in &req.attachment_ids {
        let res = sqlx::query(
            r#"
            INSERT INTO message_attachments (message_id, attachment_id)
            SELECT $1, $2 FROM attachments
            WHERE id = $2 AND uploader_id = $3
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(message.id)
        .bind(att_id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() > 0 {
            sqlx::query("UPDATE attachments SET channel_id = $1 WHERE id = $2")
                .bind(channel_id)
                .bind(att_id)
                .execute(&mut *tx)
                .await?;
        }
    }

    tx.commit().await?;

    let view = enrich(&state, vec![message])
        .await?
        .into_iter()
        .next()
        .expect("one message");

    // Realtime fan-out.
    state.events.emit_channel(
        channel_id,
        ServerEvent::MessageCreated {
            channel_id,
            message: Box::new(view.clone()),
        },
    );

    // Notifications: thread reply + mentions (best-effort, non-fatal).
    dispatch_notifications(&state, channel_id, &view, req.parent_id).await;

    Ok(Json(view))
}

async fn dispatch_notifications(
    state: &AppState,
    channel_id: Uuid,
    view: &MessageView,
    parent_id: Option<Uuid>,
) {
    // Direct-message notification: notify the other member(s) of a DM channel.
    if parent_id.is_none() {
        if let Ok(Some(ctype)) = sqlx::query_scalar::<_, String>(
            "SELECT channel_type::text FROM channels WHERE id = $1",
        )
        .bind(channel_id)
        .fetch_optional(&state.db)
        .await
        {
            if ctype == "direct" {
                if let Ok(members) = sqlx::query_scalar::<_, Uuid>(
                    "SELECT user_id FROM channel_members WHERE channel_id = $1 AND user_id <> $2",
                )
                .bind(channel_id)
                .bind(view.message.user_id)
                .fetch_all(&state.db)
                .await
                {
                    for uid in members {
                        let _ = notifications::notify(
                            &state.db,
                            &state.events,
                            uid,
                            NotificationKind::DirectMessage,
                            serde_json::json!({
                                "channel_id": channel_id,
                                "message_id": view.message.id,
                                "actor_id": view.message.user_id,
                            }),
                        )
                        .await;
                    }
                }
            }
        }
    }

    // Notify thread root author of a new reply.
    if let Some(parent) = parent_id {
        if let Ok(Some(author)) =
            sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM messages WHERE id = $1")
                .bind(parent)
                .fetch_optional(&state.db)
                .await
        {
            if author != view.message.user_id {
                let _ = notifications::notify(
                    &state.db,
                    &state.events,
                    author,
                    NotificationKind::ThreadReply,
                    serde_json::json!({
                        "channel_id": channel_id,
                        "message_id": view.message.id,
                        "actor_id": view.message.user_id,
                    }),
                )
                .await;
            }
        }
    }

    // Notify @mentioned users who are members of the channel.
    let mentions = notifications::extract_mentions(&view.message.content);
    if !mentions.is_empty() {
        if let Ok(rows) = sqlx::query_as::<_, (Uuid,)>(
            r#"
            SELECT u.id FROM users u
            JOIN channel_members m ON m.user_id = u.id AND m.channel_id = $2
            WHERE LOWER(u.username) = ANY($1) AND u.id <> $3
            "#,
        )
        .bind(&mentions)
        .bind(channel_id)
        .bind(view.message.user_id)
        .fetch_all(&state.db)
        .await
        {
            for (uid,) in rows {
                let _ = notifications::notify(
                    &state.db,
                    &state.events,
                    uid,
                    NotificationKind::Mention,
                    serde_json::json!({
                        "channel_id": channel_id,
                        "message_id": view.message.id,
                        "actor_id": view.message.user_id,
                    }),
                )
                .await;
            }
        }
    }
}

/// `GET /api/v1/channels/:id/messages` — paginated history (newest first).
pub async fn history(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<HistoryResponse>> {
    access::require_member(&state.db, channel_id, user.id).await?;
    let limit = q.clamped_limit();

    // Keyset pagination using (created_at, id) of the cursor message.
    let messages: Vec<Message> = if let Some(before) = q.before {
        sqlx::query_as::<_, Message>(
            r#"
            SELECT m.id, m.channel_id, m.user_id, m.content, m.parent_id, m.reply_count,
                   m.edited_at, m.deleted_at, m.created_at
            FROM messages m
            JOIN messages c ON c.id = $2
            WHERE m.channel_id = $1
              AND m.parent_id IS NULL
              AND (m.created_at, m.id) < (c.created_at, c.id)
            ORDER BY m.created_at DESC, m.id DESC
            LIMIT $3
            "#,
        )
        .bind(channel_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, Message>(
            r#"
            SELECT id, channel_id, user_id, content, parent_id, reply_count,
                   edited_at, deleted_at, created_at
            FROM messages
            WHERE channel_id = $1 AND parent_id IS NULL
            ORDER BY created_at DESC, id DESC
            LIMIT $2
            "#,
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&state.db)
        .await?
    };

    let next_cursor = if messages.len() as i64 == limit {
        messages.last().map(|m| m.id)
    } else {
        None
    };

    // Mark channel as read up to now for this user.
    sqlx::query(
        "UPDATE channel_members SET last_read_at = now() WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user.id)
    .execute(&state.db)
    .await?;

    Ok(Json(HistoryResponse {
        messages: enrich(&state, messages).await?,
        next_cursor,
    }))
}

/// `GET /api/v1/messages/:id/thread` — replies to a thread root.
pub async fn thread(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
) -> AppResult<Json<Vec<MessageView>>> {
    // Resolve channel for access control.
    let channel_id: Option<Uuid> =
        sqlx::query_scalar("SELECT channel_id FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_optional(&state.db)
            .await?;
    let channel_id = channel_id.ok_or_else(|| AppError::NotFound("message".into()))?;
    access::require_member(&state.db, channel_id, user.id).await?;

    let replies: Vec<Message> = sqlx::query_as::<_, Message>(
        r#"
        SELECT id, channel_id, user_id, content, parent_id, reply_count,
               edited_at, deleted_at, created_at
        FROM messages
        WHERE parent_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(message_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(enrich(&state, replies).await?))
}

/// `PATCH /api/v1/messages/:id`
pub async fn edit_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<EditMessageRequest>,
) -> AppResult<Json<MessageView>> {
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM messages WHERE id = $1 AND deleted_at IS NULL")
            .bind(message_id)
            .fetch_optional(&state.db)
            .await?;
    let owner = owner.ok_or_else(|| AppError::NotFound("message".into()))?;
    if owner != user.id {
        return Err(AppError::Forbidden("can only edit your own messages".into()));
    }

    let message: Message = sqlx::query_as::<_, Message>(
        r#"
        UPDATE messages SET content = $2, edited_at = now()
        WHERE id = $1
        RETURNING id, channel_id, user_id, content, parent_id, reply_count,
                  edited_at, deleted_at, created_at
        "#,
    )
    .bind(message_id)
    .bind(&req.content)
    .fetch_one(&state.db)
    .await?;

    let view = enrich(&state, vec![message])
        .await?
        .into_iter()
        .next()
        .unwrap();
    state.events.emit_channel(
        view.message.channel_id,
        ServerEvent::MessageUpdated {
            channel_id: view.message.channel_id,
            message: Box::new(view.clone()),
        },
    );
    Ok(Json(view))
}

/// `DELETE /api/v1/messages/:id` — soft delete.
pub async fn delete_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let row: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT user_id, channel_id FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_optional(&state.db)
            .await?;
    let (owner, channel_id) = row.ok_or_else(|| AppError::NotFound("message".into()))?;

    // Owner, or a channel admin, may delete.
    if owner != user.id {
        access::require_channel_admin(&state.db, channel_id, user.id).await?;
    }

    sqlx::query("UPDATE messages SET deleted_at = now(), content = '' WHERE id = $1")
        .bind(message_id)
        .execute(&state.db)
        .await?;

    state.events.emit_channel(
        channel_id,
        ServerEvent::MessageDeleted {
            channel_id,
            message_id,
        },
    );
    Ok(super::ok())
}

/// Resolve the channel a (non-deleted) message belongs to, or 404.
async fn message_channel(state: &AppState, message_id: Uuid) -> AppResult<Uuid> {
    let channel_id: Option<Uuid> =
        sqlx::query_scalar("SELECT channel_id FROM messages WHERE id = $1 AND deleted_at IS NULL")
            .bind(message_id)
            .fetch_optional(&state.db)
            .await?;
    channel_id.ok_or_else(|| AppError::NotFound("message".into()))
}

/// `POST /api/v1/messages/:id/reactions` — add an emoji reaction.
pub async fn add_reaction(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReactionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let channel_id = message_channel(&state, message_id).await?;
    access::require_member(&state.db, channel_id, user.id).await?;

    let res = sqlx::query(
        "INSERT INTO reactions (id, message_id, user_id, emoji) VALUES ($1, $2, $3, $4)
         ON CONFLICT (message_id, user_id, emoji) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(message_id)
    .bind(user.id)
    .bind(&req.emoji)
    .execute(&state.db)
    .await?;

    if res.rows_affected() > 0 {
        state.events.emit_channel(
            channel_id,
            ServerEvent::ReactionAdded {
                channel_id,
                message_id,
                emoji: req.emoji.clone(),
                user_id: user.id,
            },
        );
    }
    Ok(super::ok())
}

/// `DELETE /api/v1/messages/:id/reactions` — remove the caller's reaction.
pub async fn remove_reaction(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReactionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let channel_id = message_channel(&state, message_id).await?;
    access::require_member(&state.db, channel_id, user.id).await?;

    let res = sqlx::query("DELETE FROM reactions WHERE message_id = $1 AND user_id = $2 AND emoji = $3")
        .bind(message_id)
        .bind(user.id)
        .bind(&req.emoji)
        .execute(&state.db)
        .await?;

    if res.rows_affected() > 0 {
        state.events.emit_channel(
            channel_id,
            ServerEvent::ReactionRemoved {
                channel_id,
                message_id,
                emoji: req.emoji.clone(),
                user_id: user.id,
            },
        );
    }
    Ok(super::ok())
}
