//! Composition-related models: scheduled messages and per-channel drafts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// A message queued for later delivery. Rows are inserted by the compose
/// handler and drained by the background scheduler, which stamps `sent_at`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ScheduledMessage {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub user_id: Uuid,
    pub content: String,
    pub parent_id: Option<Uuid>,
    pub scheduled_for: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Body for `POST /channels/:id/scheduled`.
#[derive(Debug, Deserialize, Validate)]
pub struct ScheduleMessageRequest {
    #[validate(length(min = 1, max = 8000))]
    pub content: String,
    /// Optional thread parent, mirroring a normal message send.
    pub parent_id: Option<Uuid>,
    /// When to deliver the message. Must be in the future.
    pub scheduled_for: DateTime<Utc>,
}

/// A retained, unsent draft for one channel. Returned by `GET .../draft`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Draft {
    pub user_id: Uuid,
    pub channel_id: Uuid,
    pub content: String,
    pub updated_at: DateTime<Utc>,
}

/// Body for `PUT /channels/:id/draft`. An empty (whitespace-only) `content`
/// deletes the draft.
#[derive(Debug, Deserialize)]
pub struct SaveDraftRequest {
    #[serde(default)]
    pub content: String,
}
