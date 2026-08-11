//! Audit logging.
//!
//! Records security-relevant admin actions to the append-only `audit_events`
//! table. Recording is best-effort: a failure to write an audit row must never
//! fail or roll back the action it describes, so `record` swallows errors after
//! logging them. Callers pass the actor, an action string, and optional target
//! and metadata.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// A target of an audited action (the thing acted upon).
pub struct Target {
    pub kind: &'static str,
    pub id: String,
    pub label: String,
}

impl Target {
    pub fn new(kind: &'static str, id: impl ToString, label: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.to_string(),
            label: label.into(),
        }
    }
}

/// Record an audit event. Best-effort: never returns an error to the caller.
///
/// * `action`      stable machine string, e.g. "role.changed".
/// * `actor_id`    who performed it (None for system actions). The human label
///                 is resolved from the users table at write time and frozen
///                 into the row, so it stays readable if the actor is later
///                 deleted or renamed.
/// * `target`      optional thing acted upon.
/// * `metadata`    structured context (old/new values, reason, etc.).
pub async fn record(
    db: &PgPool,
    action: &str,
    actor_id: Option<Uuid>,
    target: Option<Target>,
    metadata: Value,
) {
    let (tk, ti, tl) = match target {
        Some(t) => (Some(t.kind.to_string()), Some(t.id), Some(t.label)),
        None => (None, None, None),
    };

    // Resolve a readable actor label now and freeze it into the row. COALESCE
    // to 'system' when there is no actor or the lookup finds nothing.
    let res = sqlx::query(
        r#"
        INSERT INTO audit_events
            (id, action, actor_id, actor_label, target_kind, target_id, target_label, metadata)
        VALUES (
            $1, $2, $3,
            COALESCE((SELECT username FROM users WHERE id = $3), 'system'),
            $4, $5, $6, $7
        )
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(action)
    .bind(actor_id)
    .bind(tk)
    .bind(ti)
    .bind(tl)
    .bind(metadata)
    .execute(db)
    .await;

    if let Err(e) = res {
        // Log and move on, never propagate.
        tracing::warn!(error = %e, action, "failed to record audit event");
    }
}
