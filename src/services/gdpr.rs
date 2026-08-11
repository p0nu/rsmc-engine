//! GDPR: personal-data export and account deletion.
//!
//! Deletion honors the instance policy (config.gdpr.deletion_mode):
//!   - Anonymize (default): scrub the user row to a tombstone; keep messages
//!     but detach identity. Threads/history stay coherent.
//!   - HardDelete: remove the user's messages and uploaded files, then delete
//!     the user row.
//!
//! IMPORTANT (schema coupling): several tables reference users with ON DELETE
//! RESTRICT (messages, attachments/uploads, channels.created_by). A bare
//! DELETE FROM users therefore fails while those rows exist. Hard delete must
//! clear them first; anonymize leaves them but removes identifying data.
//!
//! REVIEW NOTE: hard delete removes the user's messages, which can leave gaps
//! in other users' threads (replies to a now-deleted root). That is inherent to
//! honoring an erasure request; anonymize is the default precisely because it
//! preserves conversation integrity. Flagged for product sign-off.

use crate::config::DeletionMode;
use crate::state::AppState;
use serde_json::json;
use uuid::Uuid;

/// Assemble a user's personal data as a JSON document for export.
pub async fn export_user_data(state: &AppState, user_id: Uuid) -> anyhow::Result<serde_json::Value> {
    let profile = sqlx::query_as::<_, ExportProfile>(
        "SELECT id, email, username, display_name, role::text AS role, avatar_url, \
         created_at, mfa_enabled FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;

    let messages = sqlx::query_as::<_, ExportMessage>(
        "SELECT id, channel_id, content, created_at, edited_at, deleted_at \
         FROM messages WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;

    let memberships = sqlx::query_as::<_, ExportMembership>(
        "SELECT channel_id, joined_at, muted FROM channel_members WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;

    let sessions = sqlx::query_as::<_, ExportSession>(
        "SELECT user_agent, last_used_at, created_at FROM refresh_tokens \
         WHERE user_id = $1 AND NOT revoked",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;

    Ok(json!({
        "exported_at": chrono::Utc::now(),
        "profile": profile,
        "messages": messages,
        "channel_memberships": memberships,
        "active_sessions": sessions,
    }))
}

/// Delete (or anonymize) a user's account per the instance policy. Runs inside
/// a transaction so a partial failure leaves the account intact.
pub async fn delete_account(
    state: &AppState,
    user_id: Uuid,
    mode: DeletionMode,
) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;

    match mode {
        DeletionMode::HardDelete => {
            // Clear RESTRICT-referencing rows first, then the user.
            //
            // Attachments the user uploaded (attachments.uploader_id RESTRICT).
            // Their message_attachments links cascade from the message or the
            // attachment, so removing attachments and messages is sufficient.
            // Detach attachments from messages first via message_attachments,
            // then delete the user's messages, then their uploaded attachments.
            sqlx::query(
                "DELETE FROM message_attachments WHERE message_id IN \
                 (SELECT id FROM messages WHERE user_id = $1)",
            )
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .ok();

            sqlx::query("DELETE FROM messages WHERE user_id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

            // Attachments uploaded by this user (uploader_id RESTRICT). Their
            // message_attachments links cascade on attachment delete.
            sqlx::query("DELETE FROM attachments WHERE uploader_id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

            // Channels created by this user use created_by RESTRICT. Deleting
            // channels a user made is too destructive to do silently, so block
            // hard-delete with a clear error if any exist; an admin can
            // reassign or delete them first.
            let owned: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM channels WHERE created_by = $1",
            )
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
            if owned > 0 {
                anyhow::bail!(
                    "user owns {owned} channel(s); reassign or delete them before hard-deleting the account"
                );
            }

            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
        DeletionMode::Anonymize => {
            // Scrub identifying fields; keep the row so messages stay attached.
            // Randomize email/username to free them for reuse and remove PII.
            let tomb = format!("deleted-{}", &user_id.simple().to_string()[..12]);
            sqlx::query(
                "UPDATE users SET \
                   email = $2 || '@deleted.invalid', \
                   username = $2, \
                   display_name = 'Deleted User', \
                   password_hash = '', \
                   avatar_url = NULL, \
                   is_active = FALSE, \
                   status_emoji = NULL, status_text = NULL, status_expires_at = NULL, \
                   mfa_enabled = FALSE, mfa_secret_enc = NULL \
                 WHERE id = $1",
            )
            .bind(user_id)
            .bind(&tomb)
            .execute(&mut *tx)
            .await?;

            // Revoke all sessions.
            sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE user_id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
    }

    tx.commit().await?;
    Ok(())
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct ExportProfile {
    id: Uuid,
    email: String,
    username: String,
    display_name: String,
    role: String,
    avatar_url: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    mfa_enabled: bool,
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct ExportMessage {
    id: Uuid,
    channel_id: Uuid,
    content: String,
    created_at: chrono::DateTime<chrono::Utc>,
    edited_at: Option<chrono::DateTime<chrono::Utc>>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct ExportMembership {
    channel_id: Uuid,
    joined_at: chrono::DateTime<chrono::Utc>,
    muted: bool,
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct ExportSession {
    user_agent: Option<String>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
}
