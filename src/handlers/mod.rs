//! HTTP request handlers grouped by resource.

use axum::Json;

pub mod app_links;
pub mod auth;
pub mod channels;
pub mod files;
pub mod health;
pub mod messages;
pub mod notifications;
pub mod presence;
pub mod system;
pub mod users;
pub mod webhooks;

/// The standard `{ "ok": true }` success body used by mutating endpoints that
/// have nothing else to return.
pub fn ok() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}
