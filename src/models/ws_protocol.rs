//! Wire protocol for the WebSocket realtime channel.
//!
//! Clients send [`ClientEvent`] frames; the server pushes [`ServerEvent`]
//! frames. Both are tagged JSON for easy parsing on any frontend.

use super::message::MessageView;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Events sent from client to server over the socket.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// Subscribe to realtime updates for a channel the user belongs to.
    Subscribe { channel_id: Uuid },
    /// Stop receiving updates for a channel.
    Unsubscribe { channel_id: Uuid },
    /// Broadcast a transient "user is typing" indicator.
    Typing { channel_id: Uuid },
    /// Heartbeat; server replies with `Pong`.
    Ping,
}

/// Events pushed from server to client over the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A new message was posted to a subscribed channel.
    MessageCreated { channel_id: Uuid, message: Box<MessageView> },
    /// A message was edited.
    MessageUpdated { channel_id: Uuid, message: Box<MessageView> },
    /// A message was deleted.
    MessageDeleted { channel_id: Uuid, message_id: Uuid },
    /// A reaction was added to a message.
    ReactionAdded { channel_id: Uuid, message_id: Uuid, emoji: String, user_id: Uuid },
    /// A reaction was removed from a message.
    ReactionRemoved { channel_id: Uuid, message_id: Uuid, emoji: String, user_id: Uuid },
    /// Someone is typing in a channel.
    Typing { channel_id: Uuid, user_id: Uuid },
    /// A member advanced their read cursor (read receipt). `last_read_at` is the
    /// new cursor position; other members use it to render "Seen" / "Seen by N".
    Read { channel_id: Uuid, user_id: Uuid, last_read_at: DateTime<Utc> },
    /// A user's presence changed.
    Presence { user_id: Uuid, online: bool, last_seen: DateTime<Utc> },
    /// A member joined a channel (via invite or self-service join).
    MemberJoined { channel_id: Uuid, user: super::user::UserPublic },
    /// A member left or was removed from a channel.
    MemberLeft { channel_id: Uuid, user_id: Uuid },
    /// A channel was archived (`archived: true`) or unarchived (`false`).
    ChannelArchived { channel_id: Uuid, archived: bool },
    /// A user's custom status changed. `emoji`/`text` are null when cleared.
    StatusChanged { user_id: Uuid, emoji: Option<String>, text: Option<String> },
    /// A user's profile (display name and/or avatar) changed. Broadcast
    /// workspace-wide so every client can patch cached copies of the user.
    ProfileUpdated { user_id: Uuid, display_name: String, avatar_url: Option<String> },
    /// A notification was created for this user.
    Notification { notification: super::notification::Notification },
    /// Reply to a `Ping`.
    Pong,
    /// An error occurred processing a client frame.
    Error { message: String },
}

/// Presence record, also persisted for "last seen" queries.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Presence {
    pub user_id: Uuid,
    pub online: bool,
    pub last_seen: DateTime<Utc>,
}
