//! Notification creation + realtime push.

use crate::db::Db;
use crate::error::AppResult;
use crate::models::notification::{Notification, NotificationKind};
use crate::models::ws_protocol::ServerEvent;
use crate::services::events::EventBus;
use serde_json::Value;
use uuid::Uuid;

/// Persist a notification and push it to the user in realtime.
pub async fn notify(
    db: &Db,
    events: &EventBus,
    user_id: Uuid,
    kind: NotificationKind,
    payload: Value,
) -> AppResult<()> {
    let notification: Notification = sqlx::query_as::<_, Notification>(
        r#"
        INSERT INTO notifications (id, user_id, kind, payload)
        VALUES ($1, $2, $3, $4)
        RETURNING id, user_id, kind, payload, read_at, created_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(kind)
    .bind(payload)
    .fetch_one(db)
    .await?;

    events.emit_user(user_id, ServerEvent::Notification { notification });
    Ok(())
}

/// Extract `@username` mentions from message content. Returns lowercase
/// usernames without the leading `@`.
pub fn extract_mentions(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in content.split(|c: char| c.is_whitespace() || c == ',') {
        if let Some(name) = token.strip_prefix('@') {
            let cleaned: String = name
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .collect();
            if cleaned.len() >= 3 {
                out.push(cleaned.to_lowercase());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_mentions() {
        let m = extract_mentions("hey @alice and @bob_jones, ping @al");
        assert_eq!(m, vec!["alice".to_string(), "bob_jones".to_string()]);
    }
}
