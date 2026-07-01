//! Webhook (integration) management handlers.

use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::user::Permission;
use crate::models::webhook::{CreateWebhookRequest, CreateWebhookResponse, Webhook};
use crate::services::access;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use base64::Engine;
use rand::RngCore;
use uuid::Uuid;

fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `POST /api/v1/webhooks`
pub async fn create_webhook(
    State(state): State<AppState>,
    user: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateWebhookRequest>,
) -> AppResult<Json<CreateWebhookResponse>> {
    user.require(Permission::ManageWebhooks).or_else(|_| {
        // Members can register webhooks scoped to channels they administer.
        if req.channel_id.is_some() {
            Ok(())
        } else {
            Err(AppError::Forbidden(
                "instance-wide webhooks require admin".into(),
            ))
        }
    })?;

    if let Some(cid) = req.channel_id {
        access::require_channel_admin(&state.db, cid, user.id).await?;
    }

    let secret = generate_secret();
    let webhook: Webhook = sqlx::query_as::<_, Webhook>(
        r#"
        INSERT INTO webhooks (id, owner_id, channel_id, target_url, events, secret)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, owner_id, channel_id, target_url, events, secret, is_active, created_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(req.channel_id)
    .bind(&req.target_url)
    .bind(&req.events)
    .bind(&secret)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(CreateWebhookResponse {
        id: webhook.id,
        target_url: webhook.target_url,
        events: webhook.events,
        secret,
    }))
}

/// `GET /api/v1/webhooks` — list webhooks owned by the current user.
pub async fn list_webhooks(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<Webhook>>> {
    let hooks: Vec<Webhook> = sqlx::query_as::<_, Webhook>(
        r#"
        SELECT id, owner_id, channel_id, target_url, events, secret, is_active, created_at
        FROM webhooks WHERE owner_id = $1 ORDER BY created_at DESC
        "#,
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(hooks))
}

/// `DELETE /api/v1/webhooks/:id`
pub async fn delete_webhook(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let res = sqlx::query("DELETE FROM webhooks WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("webhook".into()));
    }
    Ok(super::ok())
}
