//! Message domain models, including threading and pagination helpers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Message {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub user_id: Uuid,
    pub content: String,
    /// When set, this message is a reply within a thread rooted at the parent.
    pub parent_id: Option<Uuid>,
    /// Denormalized count of replies for thread-root messages.
    pub reply_count: i32,
    pub edited_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Aggregated reactions for one emoji on a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionGroup {
    pub emoji: String,
    pub count: i64,
    pub user_ids: Vec<Uuid>,
}

/// Message enriched with author info, attachments, and reactions for API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageView {
    #[serde(flatten)]
    pub message: Message,
    pub author: super::user::UserPublic,
    #[serde(default)]
    pub attachments: Vec<super::file::Attachment>,
    #[serde(default)]
    pub reactions: Vec<ReactionGroup>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct ReactionRequest {
    #[validate(length(min = 1, max = 32))]
    pub emoji: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct SendMessageRequest {
    /// Message body. May be empty when the message carries attachments (an
    /// image-only message needs no caption); the handler enforces that a
    /// message has either text or at least one attachment.
    #[validate(length(max = 8000))]
    #[serde(default)]
    pub content: String,
    /// Reply within an existing thread.
    pub parent_id: Option<Uuid>,
    /// IDs of previously-uploaded files to attach.
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct EditMessageRequest {
    #[validate(length(min = 1, max = 8000))]
    pub content: String,
}

/// Keyset-pagination query for message history. Results are returned newest
/// first; `before` fetches older messages than the given cursor.
#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    /// Return messages created strictly before this message id.
    pub before: Option<Uuid>,
    /// Return messages created strictly after this message id.
    pub after: Option<Uuid>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

impl HistoryQuery {
    /// Clamp the limit to a safe range to protect the database.
    pub fn clamped_limit(&self) -> i64 {
        self.limit.clamp(1, 200)
    }
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub messages: Vec<MessageView>,
    /// Cursor to pass as `before` to fetch the next (older) page.
    pub next_cursor: Option<Uuid>,
}
