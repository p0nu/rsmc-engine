//! Compose handlers: scheduled messages and per-channel drafts.

use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::compose::{
    Draft, SaveDraftRequest, ScheduleMessageRequest, ScheduledMessage,
};
use crate::models::user::Permission;
use crate::services::access;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

/// `POST /api/v1/channels/:id/scheduled` - queue a message for later delivery.
pub async fn create_scheduled(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ScheduleMessageRequest>,
) -> AppResult<Json<ScheduledMessage>> {
    user.require(Permission::SendMessage)?;
    access::require_member(&state.db, channel_id, user.id).await?;

    if req.content.trim().is_empty() {
        return Err(AppError::BadRequest(
            "scheduled message content can't be empty".into(),
        ));
    }
    if req.scheduled_for <= chrono::Utc::now() {
        return Err(AppError::BadRequest(
            "scheduled_for must be in the future".into(),
        ));
    }

    // Archived channels are read-only; don't accept new scheduled sends.
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

    // Validate the thread parent, if any, belongs to this channel.
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

    let scheduled: ScheduledMessage = sqlx::query_as::<_, ScheduledMessage>(
        r#"
        INSERT INTO scheduled_messages (id, channel_id, user_id, content, parent_id, scheduled_for)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, channel_id, user_id, content, parent_id, scheduled_for, sent_at, created_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(channel_id)
    .bind(user.id)
    .bind(&req.content)
    .bind(req.parent_id)
    .bind(req.scheduled_for)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(scheduled))
}

/// `GET /api/v1/channels/:id/scheduled` - the caller's pending (unsent)
/// scheduled messages in this channel, soonest first.
pub async fn list_scheduled(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
) -> AppResult<Json<Vec<ScheduledMessage>>> {
    access::require_member(&state.db, channel_id, user.id).await?;
    let rows = sqlx::query_as::<_, ScheduledMessage>(
        r#"
        SELECT id, channel_id, user_id, content, parent_id, scheduled_for, sent_at, created_at
        FROM scheduled_messages
        WHERE channel_id = $1 AND user_id = $2 AND sent_at IS NULL
        ORDER BY scheduled_for ASC
        "#,
    )
    .bind(channel_id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// `DELETE /api/v1/scheduled/:id` - cancel a pending scheduled message. Only the
/// author can cancel, and only before it has been sent.
pub async fn cancel_scheduled(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let res = sqlx::query(
        "DELETE FROM scheduled_messages WHERE id = $1 AND user_id = $2 AND sent_at IS NULL",
    )
    .bind(id)
    .bind(user.id)
    .execute(&state.db)
    .await?;

    if res.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "scheduled message (already sent or not yours)".into(),
        ));
    }
    Ok(super::ok())
}

/// `GET /api/v1/channels/:id/draft` - the caller's saved draft for this channel.
/// Returns `{ content: "" }` when there is none, so the client always gets a
/// well-formed shape.
pub async fn get_draft(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
) -> AppResult<Json<Draft>> {
    access::require_member(&state.db, channel_id, user.id).await?;
    let existing = sqlx::query_as::<_, Draft>(
        r#"
        SELECT user_id, channel_id, content, updated_at
        FROM drafts WHERE user_id = $1 AND channel_id = $2
        "#,
    )
    .bind(user.id)
    .bind(channel_id)
    .fetch_optional(&state.db)
    .await?;

    Ok(Json(existing.unwrap_or_else(|| Draft {
        user_id: user.id,
        channel_id,
        content: String::new(),
        updated_at: chrono::Utc::now(),
    })))
}

/// `PUT /api/v1/channels/:id/draft` - upsert the caller's draft. An empty
/// (whitespace-only) body deletes it.
pub async fn save_draft(
    State(state): State<AppState>,
    user: AuthUser,
    Path(channel_id): Path<Uuid>,
    Json(req): Json<SaveDraftRequest>,
) -> AppResult<Json<serde_json::Value>> {
    access::require_member(&state.db, channel_id, user.id).await?;

    if req.content.trim().is_empty() {
        sqlx::query("DELETE FROM drafts WHERE user_id = $1 AND channel_id = $2")
            .bind(user.id)
            .bind(channel_id)
            .execute(&state.db)
            .await?;
        return Ok(Json(serde_json::json!({ "ok": true, "cleared": true })));
    }

    sqlx::query(
        r#"
        INSERT INTO drafts (user_id, channel_id, content, updated_at)
        VALUES ($1, $2, $3, now())
        ON CONFLICT (user_id, channel_id)
        DO UPDATE SET content = EXCLUDED.content, updated_at = now()
        "#,
    )
    .bind(user.id)
    .bind(channel_id)
    .bind(&req.content)
    .execute(&state.db)
    .await?;

    Ok(super::ok())
}
