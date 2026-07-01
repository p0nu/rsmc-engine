//! App link (workspace bookmark) handlers.
//!
//! Reads are open to any authenticated user (everyone sees the same shortcuts);
//! writes are admin-only. The total count is capped at `MAX_APP_LINKS`.

use crate::error::{AppError, AppResult};
use crate::middleware::{AdminUser, AuthUser, ValidatedJson};
use crate::models::app_link::{AppLink, CreateAppLinkRequest, MAX_APP_LINKS};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;

/// `GET /api/v1/app-links` — list bookmarks (any authenticated user).
pub async fn list_app_links(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Vec<AppLink>>> {
    let links: Vec<AppLink> = sqlx::query_as::<_, AppLink>(
        r#"
        SELECT id, name, url, position, created_at
        FROM app_links ORDER BY position, created_at
        "#,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(links))
}

/// `POST /api/v1/app-links` — add a bookmark (admin only).
pub async fn create_app_link(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    ValidatedJson(req): ValidatedJson<CreateAppLinkRequest>,
) -> AppResult<Json<AppLink>> {
    // Enforce the cap server-side so it holds regardless of the client.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM app_links")
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_APP_LINKS {
        return Err(AppError::BadRequest(format!(
            "at most {MAX_APP_LINKS} app links are allowed"
        )));
    }

    // New link goes last.
    let next_pos: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM app_links",
    )
    .fetch_one(&state.db)
    .await?;

    let link: AppLink = sqlx::query_as::<_, AppLink>(
        r#"
        INSERT INTO app_links (id, name, url, position)
        VALUES ($1, $2, $3, $4)
        RETURNING id, name, url, position, created_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(req.name.trim())
    .bind(req.url.trim())
    .bind(next_pos)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(link))
}

/// `DELETE /api/v1/app-links/:id` — remove a bookmark (admin only).
pub async fn delete_app_link(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let res = sqlx::query("DELETE FROM app_links WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("app link".into()));
    }
    Ok(super::ok())
}
