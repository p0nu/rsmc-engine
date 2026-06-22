//! Health & readiness probes.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};

/// `GET /healthz` — liveness; always 200 if the process is up.
pub async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// `GET /readyz` — readiness; checks DB connectivity.
pub async fn readyz(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) => Ok(Json(serde_json::json!({
            "status": "ready",
            "online_users": state.events.hub().online_count(),
        }))),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}
