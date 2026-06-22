-- ============================================================================
-- RSMC Engine — Initial Schema
-- ============================================================================
-- This migration creates the full relational schema for the collaboration
-- engine. It is written to be PostgreSQL-native (enums, UUIDs, JSONB, partial
-- indexes) while remaining generic enough to map onto any team-chat product.
--
-- Conventions:
--   * Primary keys are UUIDs generated application-side (uuid v4).
--   * All timestamps are `timestamptz` stored in UTC.
--   * Soft-deletes use a nullable `deleted_at` rather than row removal where
--     history must be preserved (messages).
-- ============================================================================

-- Required for gen_random_uuid() fallback if the app ever omits an id.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- ---- Enums ----------------------------------------------------------------

CREATE TYPE user_role AS ENUM ('admin', 'member', 'guest');
CREATE TYPE member_role AS ENUM ('owner', 'admin', 'member');
CREATE TYPE channel_type AS ENUM ('public', 'private', 'direct', 'group');
CREATE TYPE notification_kind AS ENUM ('mention', 'direct_message', 'channel_invite', 'thread_reply');

-- ---- Users ----------------------------------------------------------------

CREATE TABLE users (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email         TEXT NOT NULL,
    username      TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    role          user_role NOT NULL DEFAULT 'member',
    avatar_url    TEXT,
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Case-insensitive uniqueness for email and username.
CREATE UNIQUE INDEX users_email_lower_idx    ON users (LOWER(email));
CREATE UNIQUE INDEX users_username_lower_idx ON users (LOWER(username));

-- ---- Refresh tokens -------------------------------------------------------
-- Stores hashes of issued refresh tokens so they can be revoked / rotated.

CREATE TABLE refresh_tokens (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash  TEXT NOT NULL UNIQUE,
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX refresh_tokens_user_idx ON refresh_tokens (user_id);

-- ---- Channels -------------------------------------------------------------

CREATE TABLE channels (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name          TEXT,                       -- NULL for direct messages
    topic         TEXT,
    channel_type  channel_type NOT NULL,
    created_by    UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Public/private channel names are unique (case-insensitive). DMs have no name.
CREATE UNIQUE INDEX channels_name_lower_idx
    ON channels (LOWER(name))
    WHERE name IS NOT NULL;

-- ---- Channel membership ---------------------------------------------------

CREATE TABLE channel_members (
    channel_id    UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role          member_role NOT NULL DEFAULT 'member',
    last_read_at  TIMESTAMPTZ,
    joined_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, user_id)
);

CREATE INDEX channel_members_user_idx ON channel_members (user_id);

-- ---- Messages -------------------------------------------------------------

CREATE TABLE messages (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    channel_id   UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    content      TEXT NOT NULL,
    parent_id    UUID REFERENCES messages(id) ON DELETE CASCADE,  -- thread root
    reply_count  INTEGER NOT NULL DEFAULT 0,
    edited_at    TIMESTAMPTZ,
    deleted_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Primary access pattern: newest messages in a channel (keyset pagination).
CREATE INDEX messages_channel_created_idx
    ON messages (channel_id, created_at DESC, id DESC);

-- Thread replies lookup.
CREATE INDEX messages_parent_idx
    ON messages (parent_id, created_at)
    WHERE parent_id IS NOT NULL;

-- ---- Attachments ----------------------------------------------------------

CREATE TABLE attachments (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    uploader_id  UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    channel_id   UUID REFERENCES channels(id) ON DELETE SET NULL,
    filename     TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes   BIGINT NOT NULL,
    storage_path TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX attachments_channel_idx ON attachments (channel_id);

-- Join table linking attachments to the message that embeds them.
CREATE TABLE message_attachments (
    message_id    UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    attachment_id UUID NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, attachment_id)
);

-- ---- Notifications --------------------------------------------------------

CREATE TABLE notifications (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind        notification_kind NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}'::jsonb,
    read_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fetch a user's notifications newest-first; partial index for unread.
CREATE INDEX notifications_user_created_idx ON notifications (user_id, created_at DESC);
CREATE INDEX notifications_unread_idx ON notifications (user_id) WHERE read_at IS NULL;

-- ---- Presence -------------------------------------------------------------

CREATE TABLE presence (
    user_id    UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    online     BOOLEAN NOT NULL DEFAULT FALSE,
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---- Webhooks -------------------------------------------------------------

CREATE TABLE webhooks (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id  UUID REFERENCES channels(id) ON DELETE CASCADE,
    target_url  TEXT NOT NULL,
    events      TEXT[] NOT NULL DEFAULT '{}',
    secret      TEXT NOT NULL,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX webhooks_channel_idx ON webhooks (channel_id);
CREATE INDEX webhooks_active_idx ON webhooks (is_active) WHERE is_active;

-- ---- updated_at trigger ---------------------------------------------------
-- Keep updated_at fresh automatically on row modification.

CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER users_set_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TRIGGER channels_set_updated_at
    BEFORE UPDATE ON channels
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
