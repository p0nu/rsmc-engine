//! Channel and membership domain models.

use super::user::MemberRole;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Type;
use uuid::Uuid;
use validator::Validate;

/// The kind of conversation a channel represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[sqlx(type_name = "channel_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ChannelType {
    /// A named, multi-member public channel anyone can join.
    Public,
    /// A named, multi-member private channel; invite only.
    Private,
    /// A 1:1 direct message conversation.
    Direct,
    /// An ad-hoc multi-person direct message group.
    Group,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Channel {
    pub id: Uuid,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub channel_type: ChannelType,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChannelMember {
    pub channel_id: Uuid,
    pub user_id: Uuid,
    pub role: MemberRole,
    /// Last message the user has read; powers unread counts.
    pub last_read_at: Option<DateTime<Utc>>,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateChannelRequest {
    #[validate(length(min = 1, max = 80))]
    pub name: String,
    #[validate(length(max = 255))]
    pub topic: Option<String>,
    #[serde(default)]
    pub private: bool,
    /// Optional initial members (besides the creator).
    #[serde(default)]
    pub member_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateDirectRequest {
    /// The other participant for a 1:1 DM.
    pub user_id: Uuid,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateChannelRequest {
    #[validate(length(min = 1, max = 80))]
    pub name: Option<String>,
    #[validate(length(max = 255))]
    pub topic: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: Uuid,
}

/// A channel plus the caller's read state, for the channel-list view.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ChannelWithMeta {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub channel: Channel,
    pub unread_count: i64,
    pub last_read_at: Option<DateTime<Utc>>,
}
