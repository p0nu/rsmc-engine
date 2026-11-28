//! Instance settings: a small admin-editable key/value store, read by any
//! authenticated user and written only by an admin.
//!
//! Currently holds the name-display policy (`name_display`) that controls
//! whether channel message authors are shown by full name or by username.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{AppError, AppResult};
use crate::middleware::{AdminUser, AuthUser};
use crate::state::AppState;

/// Allowed values for the name_display setting.
const NAME_DISPLAY_KEY: &str = "name_display";
const NAME_DISPLAY_VALUES: [&str; 2] = ["full_name", "username"];

#[derive(Debug, Serialize)]
pub struct SettingsView {
    pub name_display: String,
}

async fn load_all(state: &AppState) -> AppResult<HashMap<String, String>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings")
            .fetch_all(&state.db)
            .await?;
    Ok(rows.into_iter().collect())
}

/// `GET /settings` — any authenticated user. Returns the current instance
/// settings the client needs, with defaults filled in for any unset key.
pub async fn get_settings(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<SettingsView>> {
    let map = load_all(&state).await?;
    Ok(Json(SettingsView {
        name_display: map
            .get(NAME_DISPLAY_KEY)
            .cloned()
            .unwrap_or_else(|| "full_name".to_string()),
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettings {
    /// Any subset of settings to change. Only recognized keys are applied.
    pub name_display: Option<String>,
}

/// `PUT /settings` — admin only. Updates one or more settings and returns the
/// full current view.
pub async fn update_settings(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    Json(req): Json<UpdateSettings>,
) -> AppResult<Json<SettingsView>> {
    if let Some(v) = req.name_display.as_deref() {
        if !NAME_DISPLAY_VALUES.contains(&v) {
            return Err(AppError::BadRequest(format!(
                "name_display must be one of: {}",
                NAME_DISPLAY_VALUES.join(", ")
            )));
        }
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, now()) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
        )
        .bind(NAME_DISPLAY_KEY)
        .bind(v)
        .execute(&state.db)
        .await?;
    }

    let map = load_all(&state).await?;
    Ok(Json(SettingsView {
        name_display: map
            .get(NAME_DISPLAY_KEY)
            .cloned()
            .unwrap_or_else(|| "full_name".to_string()),
    }))
}
