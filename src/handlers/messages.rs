//! Message handlers: send, history, threads, edit, delete.

use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::message::{
    EditMessageRequest, HistoryQuery, HistoryResponse, Message, MessageView, ReactionRequest,
    SearchQuery, SearchResponse, SendMessageRequest,
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
pub(crate) async fn enrich(state: &AppState, messages: Vec<Message>) -> AppResult<Vec<MessageView>> {
    if messages.is_empty() {
        return Ok(vec![]);
    }
    // System messages have no author (user_id is None), so skip them here.
    let author_ids: Vec<Uuid> = messages.iter().filter_map(|m| m.user_id).collect();
    let message_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();

    // Fetch all distinct authors in one query.
    let authors: Vec<crate::models::user::User> = sqlx::query_as::<_, crate::models::user::User>(
        r#"
        SELECT id, email, username, display_name, password_hash, role,
               avatar_url, is_active, created_at, updated_at,
               status_emoji, status_text, status_expires_at
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
            // System messages have no author; user messages resolve theirs
            // (falling back to a placeholder if the account is gone).
            let author = m.user_id.map(|uid| {
                author_map
                    .get(&uid)
                    .cloned()
                    .unwrap_or_else(|| placeholder_user(uid))
            });
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

/// Post a system message (no author) to a channel and broadcast it live, so it
/// appears in the feed like "<user> joined the channel". Best-effort: a failure
/// is logged and swallowed, never bubbled to the caller's main operation.
pub(crate) async fn post_system_message(state: &AppState, channel_id: Uuid, content: String) {
    let inserted: Result<Message, _> = sqlx::query_as::<_, Message>(
        r#"
        INSERT INTO messages (id, channel_id, user_id, content, message_type)
        VALUES ($1, $2, NULL, $3, 'system')
        RETURNING id, channel_id, user_id, content, parent_id, reply_count,
                  edited_at, deleted_at, created_at, message_type
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(channel_id)
    .bind(&content)
    .fetch_one(&state.db)
    .await;

    let message = match inserted {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "could not post system message");
            return;
        }
    };

    if let Ok(mut views) = enrich(state, vec![message]).await {
        if let Some(view) = views.pop() {
            state.events.emit_channel(
                channel_id,
                ServerEvent::MessageCreated {
                    channel_id,
                    message: Box::new(view),
                },
            );
        }
    }
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
        status_emoji: None,
        status_text: None,
        status_expires_at: None,
        email_verified: None,
        notification_prefs: None,
        mfa_enabled: None,
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

    // Archived channels are read-only.
    let archived: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT archived_at FROM channels WHERE id = $1")
            .bind(channel_id)
            .fetch_optional(&state.db)
            .await?
            .flatten();
    if archived.is_some() {
        return Err(AppError::BadRequest(
            "this channel is archived and read-only".into(),
        ));
    }

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
                  edited_at, deleted_at, created_at, message_type
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
                    "SELECT cm.user_id FROM channel_members cm \
                     JOIN users u ON u.id = cm.user_id \
                     WHERE cm.channel_id = $1 AND cm.user_id <> $2 \
                       AND NOT cm.muted AND u.notify_dm",
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
        if let Ok(Some(author)) = sqlx::query_scalar::<_, Uuid>(
            "SELECT m.user_id FROM messages m \
             JOIN channel_members cm ON cm.channel_id = m.channel_id AND cm.user_id = m.user_id \
             JOIN users u ON u.id = m.user_id \
             WHERE m.id = $1 AND NOT cm.muted AND u.notify_thread_reply",
        )
        .bind(parent)
        .fetch_optional(&state.db)
        .await
        {
            if Some(author) != view.message.user_id {
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
            WHERE LOWER(u.username) = ANY($1) AND u.id <> $3 AND NOT m.muted
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
    // Broadcast mention (@channel / @here / @everyone): notify every other
    // member of the channel. Skipped for direct channels (those already send a
    // DirectMessage notification) and for thread replies.
    if parent_id.is_none() && notifications::has_broadcast_mention(&view.message.content) {
        let is_direct = sqlx::query_scalar::<_, String>(
            "SELECT channel_type::text FROM channels WHERE id = $1",
        )
        .bind(channel_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|t| t == "direct")
        .unwrap_or(false);

        if !is_direct {
            if let Ok(members) = sqlx::query_scalar::<_, Uuid>(
                "SELECT cm.user_id FROM channel_members cm \
                 JOIN users u ON u.id = cm.user_id \
                 WHERE cm.channel_id = $1 AND cm.user_id <> $2 \
                   AND NOT cm.muted AND u.notify_broadcast",
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
                        NotificationKind::Mention,
                        serde_json::json!({
                            "channel_id": channel_id,
                            "message_id": view.message.id,
                            "actor_id": view.message.user_id,
                            "broadcast": true,
                        }),
                    )
                    .await;
                }
            }
        }
    }
}

/// `GET /api/v1/channels/:id/messages` - paginated history (newest first).
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
                   m.edited_at, m.deleted_at, m.created_at, m.pinned_at, m.pinned_by
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
                   edited_at, deleted_at, created_at, pinned_at, pinned_by, message_type
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

/// `GET /api/v1/messages/:id/thread` - replies to a thread root.
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
               edited_at, deleted_at, created_at, pinned_at, pinned_by, message_type
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
                  edited_at, deleted_at, created_at, pinned_at, pinned_by, message_type
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

/// `DELETE /api/v1/messages/:id` - soft delete.
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

/// `POST /api/v1/messages/:id/reactions` - add an emoji reaction.
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

/// `DELETE /api/v1/messages/:id/reactions` - remove the caller's reaction.
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

/// `GET /api/v1/search?q=...&channel_id=...&limit=...` - full-text message
/// search. Scoped to one channel when `channel_id` is given, otherwise across
/// every channel the caller is a member of. Deleted messages are excluded.
pub async fn search(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<SearchResponse>> {
    let parsed = q.parse();
    let text = parsed.text.trim().to_string();
    let limit = q.clamped_limit();

    // A search must have at least free text or one narrowing filter; an empty
    // request returns nothing rather than the whole workspace.
    let has_filter = parsed.from.is_some()
        || parsed.in_channel.is_some()
        || parsed.has.is_some()
        || parsed.after.is_some()
        || parsed.before.is_some()
        || q.channel_id.is_some();
    if text.is_empty() && !has_filter {
        return Ok(Json(SearchResponse { results: Vec::new() }));
    }

    // Resolve a channel scope from either the explicit channel_id or `in:name`.
    // Membership is enforced in both cases: an explicit channel_id via an
    // up-front check, and an `in:` name via the membership join that is kept
    // whenever the scope did not come from an already-authorized channel_id.
    let mut scope_authorized = false;
    let channel_scope: Option<Uuid> = if let Some(cid) = q.channel_id {
        access::require_member(&state.db, cid, user.id).await?;
        scope_authorized = true;
        Some(cid)
    } else if let Some(name) = &parsed.in_channel {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM channels WHERE LOWER(name) = LOWER($1) LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };

    // If `in:` named a channel that doesn't exist, return empty rather than
    // silently searching everything.
    if parsed.in_channel.is_some() && q.channel_id.is_none() && channel_scope.is_none() {
        return Ok(Json(SearchResponse { results: Vec::new() }));
    }

    // Resolve `from:username` to a user id; an unknown user yields no rows.
    let from_id: Option<Uuid> = if let Some(uname) = &parsed.from {
        let id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM users WHERE LOWER(username) = LOWER($1) LIMIT 1",
        )
        .bind(uname)
        .fetch_optional(&state.db)
        .await?;
        if id.is_none() {
            return Ok(Json(SearchResponse { results: Vec::new() }));
        }
        id
    } else {
        None
    };

    let wants_files = parsed
        .has
        .as_deref()
        .map(|h| h == "file" || h == "files" || h == "attachment")
        .unwrap_or(false);

    // Build one dynamic query with only the clauses the filters require.
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT m.id, m.channel_id, m.user_id, m.content, m.parent_id, m.reply_count, \
         m.edited_at, m.deleted_at, m.created_at, m.pinned_at, m.pinned_by \
         FROM messages m ",
    );

    // Scope to caller's channels unless a specific, already-authorized channel
    // is targeted. When the scope came from `in:name` we keep the join, so a
    // caller can't read a channel they don't belong to.
    if !scope_authorized {
        qb.push("JOIN channel_members cm ON cm.channel_id = m.channel_id AND cm.user_id = ");
        qb.push_bind(user.id);
        qb.push(" ");
    }

    qb.push("WHERE m.deleted_at IS NULL ");

    if let Some(cid) = channel_scope {
        qb.push("AND m.channel_id = ");
        qb.push_bind(cid);
        qb.push(" ");
    }
    if let Some(fid) = from_id {
        qb.push("AND m.user_id = ");
        qb.push_bind(fid);
        qb.push(" ");
    }
    if let Some(after) = parsed.after {
        qb.push("AND m.created_at >= ");
        qb.push_bind(after);
        qb.push(" ");
    }
    if let Some(before) = parsed.before {
        qb.push("AND m.created_at < ");
        qb.push_bind(before);
        qb.push(" ");
    }
    if wants_files {
        qb.push(
            "AND EXISTS (SELECT 1 FROM message_attachments ma WHERE ma.message_id = m.id) ",
        );
    }
    if !text.is_empty() {
        qb.push("AND (to_tsvector('english', m.content) @@ websearch_to_tsquery('english', ");
        qb.push_bind(text.clone());
        qb.push(") OR m.content ILIKE '%' || ");
        qb.push_bind(text.clone());
        qb.push(" || '%') ");
    }

    qb.push("ORDER BY m.created_at DESC LIMIT ");
    qb.push_bind(limit);

    let messages: Vec<Message> = qb
        .build_query_as::<Message>()
        .fetch_all(&state.db)
        .await?;

    Ok(Json(SearchResponse {
        results: enrich(&state, messages).await?,
    }))
}

/// `POST /api/v1/messages/:id/pin` - pin a message in its channel.
pub async fn pin_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
) -> AppResult<Json<MessageView>> {
    let channel_id = message_channel(&state, message_id).await?;
    access::require_member(&state.db, channel_id, user.id).await?;

    let updated: Message = sqlx::query_as::<_, Message>(
        r#"
        UPDATE messages
        SET pinned_at = COALESCE(pinned_at, now()), pinned_by = $2
        WHERE id = $1 AND deleted_at IS NULL
        RETURNING id, channel_id, user_id, content, parent_id, reply_count,
                  edited_at, deleted_at, created_at, pinned_at, pinned_by, message_type
        "#,
    )
    .bind(message_id)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;

    let view = enrich(&state, vec![updated])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::NotFound("message".into()))?;
    state
        .events
        .emit_channel(channel_id, ServerEvent::MessageUpdated { channel_id, message: Box::new(view.clone()) });
    Ok(Json(view))
}

/// `DELETE /api/v1/messages/:id/pin` - unpin a message.
pub async fn unpin_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(message_id): Path<Uuid>,
) -> AppResult<Json<MessageView>> {
    let channel_id = message_channel(&state, message_id).await?;
    access::require_member(&state.db, channel_id, user.id).await?;

    let updated: Message = sqlx::query_as::<_, Message>(
        r#"
        UPDATE messages
        SET pinned_at = NULL, pinned_by = NULL
        WHERE id = $1 AND deleted_at IS NULL
        RETURNING id, channel_id, user_id, content, parent_id, reply_count,
                  edited_at, deleted_at, created_at, pinned_at, pinned_by, message_type
        "#,
    )
    .bind(message_id)
    .fetch_one(&state.db)
    .await?;

    let view = enrich(&state, vec![updated])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::NotFound("message".into()))?;
    state
        .events
        .emit_channel(channel_id, ServerEvent::MessageUpdated { channel_id, message: Box::new(view.clone()) });
    Ok(Json(view))
}

/// `GET /api/v1/channels/:id/pins` - list pinned messages in a channel,
/// newest pin first.
/// Query for `GET /channels/:id/pins` pagination. `cursor` is the `pinned_at`
/// (RFC3339) of the last pin seen; results continue strictly older than it.
#[derive(Debug, serde::Deserialize)]
pub struct PinsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, serde::Serialize)]
pub struct PinsPage {
    pub pins: Vec<MessageView>,
    /// Pass back as `cursor` for the next page; null when exhausted.
    pub next_cursor: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list_pins(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
    Query(q): Query<PinsQuery>,
) -> AppResult<Json<PinsPage>> {
    access::require_member(&state.db, channel_id, user.id).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let messages: Vec<Message> = sqlx::query_as::<_, Message>(
        r#"
        SELECT id, channel_id, user_id, content, parent_id, reply_count,
               edited_at, deleted_at, created_at, pinned_at, pinned_by, message_type
        FROM messages
        WHERE channel_id = $1 AND pinned_at IS NOT NULL AND deleted_at IS NULL
          AND ($2::timestamptz IS NULL OR pinned_at < $2)
        ORDER BY pinned_at DESC
        LIMIT $3
        "#,
    )
    .bind(channel_id)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;

    let has_more = messages.len() as i64 > limit;
    let page: Vec<_> = messages.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page.last().and_then(|m| m.pinned_at)
    } else {
        None
    };

    Ok(Json(PinsPage {
        pins: enrich(&state, page).await?,
        next_cursor,
    }))
}
