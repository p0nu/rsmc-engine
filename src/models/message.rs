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
    /// When set, this message is pinned in its channel. Populated only by
    /// queries that select it (history/search/pins); `#[sqlx(default)]` keeps
    /// the many existing column-list queries that omit it working unchanged.
    #[serde(default)]
    #[sqlx(default)]
    pub pinned_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[sqlx(default)]
    pub pinned_by: Option<Uuid>,
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

/// Query for message search. `channel_id` scopes to one channel; when omitted,
/// search spans every channel the caller belongs to.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub channel_id: Option<Uuid>,
    /// Filter to messages authored by this username (resolves to a user id).
    /// Also expressible inline as `from:username` in `q`.
    pub from: Option<String>,
    /// Filter to a channel by name. Also expressible inline as `in:channel`.
    pub in_channel: Option<String>,
    /// Attachment filter; currently only `file` (messages with attachments).
    /// Also expressible inline as `has:file`.
    pub has: Option<String>,
    /// Only messages at/after this instant. Inline: `after:YYYY-MM-DD`.
    pub after: Option<DateTime<Utc>>,
    /// Only messages strictly before this instant. Inline: `before:YYYY-MM-DD`.
    pub before: Option<DateTime<Utc>>,
    #[serde(default = "default_search_limit")]
    pub limit: i64,
}

fn default_search_limit() -> i64 {
    30
}

/// Operators parsed out of a raw search string, plus the remaining free text.
#[derive(Debug, Default, Clone)]
pub struct ParsedSearch {
    pub text: String,
    pub from: Option<String>,
    pub in_channel: Option<String>,
    pub has: Option<String>,
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
}

impl SearchQuery {
    /// Merge inline operators from `q` with any explicit query-param fields.
    /// Explicit params win over inline operators when both are present.
    pub fn parse(&self) -> ParsedSearch {
        let mut p = parse_search_operators(&self.q);
        if self.from.is_some() {
            p.from = self.from.clone();
        }
        if self.in_channel.is_some() {
            p.in_channel = self.in_channel.clone();
        }
        if self.has.is_some() {
            p.has = self.has.clone();
        }
        if self.after.is_some() {
            p.after = self.after;
        }
        if self.before.is_some() {
            p.before = self.before;
        }
        p
    }
}

/// Parse `from:`, `in:`, `has:`, `before:`, `after:` operators out of a search
/// string, returning them plus the leftover free text. Unknown tokens and plain
/// words are kept as text. A leading `#`/`@` on `in:`/`from:` values is stripped.
/// Dates accept `YYYY-MM-DD` (interpreted as UTC midnight) or full RFC3339.
pub fn parse_search_operators(raw: &str) -> ParsedSearch {
    let mut out = ParsedSearch::default();
    let mut text_parts: Vec<&str> = Vec::new();

    for tok in raw.split_whitespace() {
        if let Some(v) = tok.strip_prefix("from:") {
            let v = v.trim_start_matches('@');
            if !v.is_empty() {
                out.from = Some(v.to_string());
                continue;
            }
        } else if let Some(v) = tok.strip_prefix("in:") {
            let v = v.trim_start_matches('#');
            if !v.is_empty() {
                out.in_channel = Some(v.to_string());
                continue;
            }
        } else if let Some(v) = tok.strip_prefix("has:") {
            if !v.is_empty() {
                out.has = Some(v.to_lowercase());
                continue;
            }
        } else if let Some(v) = tok.strip_prefix("after:") {
            if let Some(dt) = parse_search_date(v) {
                out.after = Some(dt);
                continue;
            }
        } else if let Some(v) = tok.strip_prefix("before:") {
            if let Some(dt) = parse_search_date(v) {
                out.before = Some(dt);
                continue;
            }
        }
        text_parts.push(tok);
    }

    out.text = text_parts.join(" ");
    out
}

/// Accept `YYYY-MM-DD` (UTC midnight) or a full RFC3339 timestamp.
fn parse_search_date(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(
            d.and_hms_opt(0, 0, 0)?,
            Utc,
        ));
    }
    None
}

impl SearchQuery {
    pub fn clamped_limit(&self) -> i64 {
        self.limit.clamp(1, 50)
    }
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<MessageView>,
}
