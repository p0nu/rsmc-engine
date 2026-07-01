//! Notification handlers.

use crate::error::AppResult;
use crate::middleware::AuthUser;
use crate::models::notification::{Notification, NotificationQuery};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use uuid::Uuid;

/// `GET /api/v1/notifications`
pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<NotificationQuery>,
) -> AppResult<Json<Vec<Notification>>> {
    let limit = q.clamped_limit();
    let rows: Vec<Notification> = sqlx::query_as::<_, Notification>(
        r#"
        SELECT id, user_id, kind, payload, read_at, created_at
        FROM notifications
        WHERE user_id = $1 AND ($2 = FALSE OR read_at IS NULL)
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(user.id)
    .bind(q.unread_only)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// `GET /api/v1/notifications/unread_count`
pub async fn unread_count(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(serde_json::json!({ "unread": count })))
}

/// `POST /api/v1/notifications/:id/read`
pub async fn mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query(
        "UPDATE notifications SET read_at = now()
         WHERE id = $1 AND user_id = $2 AND read_at IS NULL",
    )
    .bind(id)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    Ok(super::ok())
}

/// `POST /api/v1/notifications/read_all`
pub async fn mark_all_read(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1 AND read_at IS NULL")
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(super::ok())
}
