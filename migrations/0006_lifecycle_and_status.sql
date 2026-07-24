-- ============================================================================
-- 0006: channel lifecycle (archival), custom user status, scheduled messages,
--       and per-channel drafts
-- ============================================================================
-- All changes here are additive and nullable, so applying this migration to an
-- existing 0.6.0 database is safe and requires no backfill.

-- ---- Channel archival -----------------------------------------------------
-- An archived channel becomes read-only: no new messages may be posted, and it
-- is hidden from the default channel list (opt back in with
-- ?include_archived=true). Archival is reversible (unarchive clears both).
ALTER TABLE channels
    ADD COLUMN archived_at TIMESTAMPTZ NULL,
    ADD COLUMN archived_by UUID NULL REFERENCES users(id) ON DELETE SET NULL;

-- ---- Custom user status ---------------------------------------------------
-- A lightweight, Slack-style status: an optional emoji + short text, with an
-- optional auto-expiry. Expired statuses are treated as absent by the API.
ALTER TABLE users
    ADD COLUMN status_emoji      TEXT NULL,
    ADD COLUMN status_text       TEXT NULL,
    ADD COLUMN status_expires_at TIMESTAMPTZ NULL;

-- ---- Scheduled messages ---------------------------------------------------
-- Messages composed now but delivered later. A background sweeper claims due
-- rows, inserts the real message, and stamps sent_at. Rows are retained after
-- sending for a short audit trail; the sweeper only ever claims unsent rows.
CREATE TABLE scheduled_messages (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id    UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    content       TEXT NOT NULL,
    parent_id     UUID NULL REFERENCES messages(id) ON DELETE CASCADE,
    scheduled_for TIMESTAMPTZ NOT NULL,
    sent_at       TIMESTAMPTZ NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The sweeper scans for unsent rows whose time has come, oldest first.
CREATE INDEX scheduled_messages_due_idx
    ON scheduled_messages (scheduled_for)
    WHERE sent_at IS NULL;

-- Listing a user's own pending scheduled messages within a channel.
CREATE INDEX scheduled_messages_channel_user_idx
    ON scheduled_messages (channel_id, user_id)
    WHERE sent_at IS NULL;

-- ---- Drafts ---------------------------------------------------------------
-- One retained, unsent draft per user per channel. Overwritten on save and
-- removed when cleared or sent. Primary key on (user_id, channel_id) makes save
-- an upsert.
CREATE TABLE drafts (
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    content    TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, channel_id)
);
