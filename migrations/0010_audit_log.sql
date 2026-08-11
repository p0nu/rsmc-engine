-- Admin audit log: an append-only record of security-relevant actions
-- (role changes, account activation/deactivation, message deletions by
-- someone other than the author, backup/restore, account deletion).
--
-- Deliberately denormalized and self-contained: actor and target are stored by
-- id AND by a human-readable label captured at the time of the action, so the
-- log stays meaningful even if a user is later deleted or renamed. Rows are
-- never updated or deleted by application code (append-only).

CREATE TABLE IF NOT EXISTS audit_events (
    id           UUID PRIMARY KEY,
    -- What happened, a stable machine string, e.g. "role.changed",
    -- "user.deactivated", "message.deleted", "backup.created".
    action       TEXT NOT NULL,
    -- Who did it. actor_id may be null for system-originated actions.
    actor_id     UUID REFERENCES users(id) ON DELETE SET NULL,
    actor_label  TEXT NOT NULL DEFAULT 'system',
    -- What it was done to (optional): another user, a message, a channel, etc.
    target_kind  TEXT,
    target_id    TEXT,
    target_label TEXT,
    -- Free-form structured context (old/new role, reason, etc.).
    metadata     JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_events_created_idx ON audit_events (created_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_action_idx ON audit_events (action);
CREATE INDEX IF NOT EXISTS audit_events_actor_idx ON audit_events (actor_id);
