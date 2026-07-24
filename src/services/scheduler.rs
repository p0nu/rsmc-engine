//! Background scheduler: delivers due scheduled messages.
//!
//! A single sweeper task runs on an interval. Each tick atomically claims due,
//! unsent rows (`FOR UPDATE SKIP LOCKED` so multiple instances never deliver
//! the same row twice), inserts a real message for each, stamps `sent_at`, and
//! fans out `MessageCreated` over the event bus. Rows whose channel has been
//! archived or deleted are skipped (and marked sent so they don't linger).

use crate::handlers::messages;
use crate::models::compose::ScheduledMessage;
use crate::models::message::Message;
use crate::models::ws_protocol::ServerEvent;
use crate::state::AppState;
use std::time::Duration;
use uuid::Uuid;

/// How often the sweeper wakes to check for due messages.
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// Spawn the background sweeper. Called once from bootstrap.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        // Skip missed ticks rather than bursting after a pause.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if let Err(e) = sweep_once(&state).await {
                tracing::warn!(error = %e, "scheduled-message sweep failed");
            }
        }
    });
}

/// Deliver all currently-due scheduled messages. Returns the number delivered.
/// Public so integration tests can trigger a deterministic sweep.
pub async fn sweep_once(state: &AppState) -> crate::error::AppResult<usize> {
    let mut delivered = 0usize;

    loop {
        // Claim a batch of due rows atomically. SKIP LOCKED lets concurrent
        // instances make progress without contending on the same rows.
        let mut tx = state.db.begin().await?;
        let due: Vec<ScheduledMessage> = sqlx::query_as::<_, ScheduledMessage>(
            r#"
            SELECT id, channel_id, user_id, content, parent_id, scheduled_for, sent_at, created_at
            FROM scheduled_messages
            WHERE sent_at IS NULL AND scheduled_for <= now()
            ORDER BY scheduled_for ASC
            LIMIT 50
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .fetch_all(&mut *tx)
        .await?;

        if due.is_empty() {
            tx.commit().await?;
            break;
        }

        // Collect messages to fan out after the transaction commits.
        let mut to_emit: Vec<(Uuid, Message)> = Vec::new();

        for sched in &due {
            // Skip channels that have been deleted or archived since scheduling.
            let channel_state: Option<(Option<chrono::DateTime<chrono::Utc>>,)> = sqlx::query_as(
                "SELECT archived_at FROM channels WHERE id = $1",
            )
            .bind(sched.channel_id)
            .fetch_optional(&mut *tx)
            .await?;

            let skip = match channel_state {
                None => true,                        // channel deleted
                Some((archived_at,)) => archived_at.is_some(), // channel archived
            };

            if skip {
                // Mark sent so we don't re-examine it forever.
                sqlx::query("UPDATE scheduled_messages SET sent_at = now() WHERE id = $1")
                    .bind(sched.id)
                    .execute(&mut *tx)
                    .await?;
                continue;
            }

            // Insert the real message.
            let message: Message = sqlx::query_as::<_, Message>(
                r#"
                INSERT INTO messages (id, channel_id, user_id, content, parent_id)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING id, channel_id, user_id, content, parent_id, reply_count,
                          edited_at, deleted_at, created_at
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(sched.channel_id)
            .bind(sched.user_id)
            .bind(&sched.content)
            .bind(sched.parent_id)
            .fetch_one(&mut *tx)
            .await?;

            // Maintain thread reply counts + channel ordering, mirroring send.
            if let Some(parent) = sched.parent_id {
                sqlx::query("UPDATE messages SET reply_count = reply_count + 1 WHERE id = $1")
                    .bind(parent)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("UPDATE channels SET updated_at = now() WHERE id = $1")
                .bind(sched.channel_id)
                .execute(&mut *tx)
                .await?;

            sqlx::query("UPDATE scheduled_messages SET sent_at = now() WHERE id = $1")
                .bind(sched.id)
                .execute(&mut *tx)
                .await?;

            to_emit.push((sched.channel_id, message));
        }

        tx.commit().await?;

        // Fan out realtime events after commit so subscribers never observe a
        // message that a rollback later erased.
        for (channel_id, message) in to_emit {
            match messages::enrich(state, vec![message]).await {
                Ok(mut views) => {
                    if let Some(view) = views.pop() {
                        state.events.emit_channel(
                            channel_id,
                            ServerEvent::MessageCreated {
                                channel_id,
                                message: Box::new(view),
                            },
                        );
                        delivered += 1;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to enrich scheduled message"),
            }
        }
    }

    Ok(delivered)
}
