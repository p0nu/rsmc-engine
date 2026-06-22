//! Channel-level access control helpers shared by handlers.

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::models::user::MemberRole;
use uuid::Uuid;

/// Ensure a user is a member of a channel. Returns their membership role.
pub async fn require_member(db: &Db, channel_id: Uuid, user_id: Uuid) -> AppResult<MemberRole> {
    let role: Option<MemberRole> = sqlx::query_scalar(
        "SELECT role FROM channel_members WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    role.ok_or_else(|| AppError::Forbidden("not a member of this channel".into()))
}

/// Ensure a user can administer a channel (owner or admin role).
pub async fn require_channel_admin(db: &Db, channel_id: Uuid, user_id: Uuid) -> AppResult<()> {
    let role = require_member(db, channel_id, user_id).await?;
    match role {
        MemberRole::Owner | MemberRole::Admin => Ok(()),
        MemberRole::Member => Err(AppError::Forbidden(
            "requires channel admin privileges".into(),
        )),
    }
}
