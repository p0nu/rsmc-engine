//! Presence handlers — online/offline + last-seen lookups.

use crate::error::AppResult;
use crate::middleware::AuthUser;
use crate::models::ws_protocol::Presence;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

/// `GET /api/v1/presence/:user_id`
///
/// Live online status is sourced from the in-memory hub (authoritative for
/// "right now"); `last_seen` comes from the persisted table.
pub async fn get_presence(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(user_id): Path<Uuid>,
) -> AppResult<Json<Presence>> {
    let stored: Option<Presence> = sqlx::query_as::<_, Presence>(
        "SELECT user_id, online, last_seen FROM presence WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;

    let online = state.events.hub().is_online(user_id);
    let presence = match stored {
        Some(mut p) => {
            p.online = online;
            p
        }
        None => Presence {
            user_id,
            online,
            last_seen: chrono::Utc::now(),
        },
    };
    Ok(Json(presence))
}

#[derive(Debug, Deserialize)]
pub struct BulkPresenceRequest {
    pub user_ids: Vec<Uuid>,
}

/// `POST /api/v1/presence/bulk` — resolve presence for many users at once.
pub async fn bulk_presence(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<BulkPresenceRequest>,
) -> AppResult<Json<Vec<Presence>>> {
    let stored: Vec<Presence> = sqlx::query_as::<_, Presence>(
        "SELECT user_id, online, last_seen FROM presence WHERE user_id = ANY($1)",
    )
    .bind(&req.user_ids)
    .fetch_all(&state.db)
    .await?;
    let mut map: std::collections::HashMap<Uuid, Presence> =
        stored.into_iter().map(|p| (p.user_id, p)).collect();

    let out = req
        .user_ids
        .into_iter()
        .map(|uid| {
            let online = state.events.hub().is_online(uid);
            match map.remove(&uid) {
                Some(mut p) => {
                    p.online = online;
                    p
                }
                None => Presence {
                    user_id: uid,
                    online,
                    last_seen: chrono::Utc::now(),
                },
            }
        })
        .collect();
    Ok(Json(out))
}
