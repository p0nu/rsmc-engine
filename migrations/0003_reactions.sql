-- Message reactions (emoji).
--
-- A user may react to a message with a given emoji at most once. Reactions are
-- aggregated per message for display.

CREATE TABLE IF NOT EXISTS reactions (
    id          UUID PRIMARY KEY,
    message_id  UUID NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    emoji       TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (message_id, user_id, emoji)
);

CREATE INDEX IF NOT EXISTS idx_reactions_message ON reactions (message_id);
