-- Per-user notification preferences.
-- Additive with safe defaults: every column defaults to TRUE so existing rows
-- keep the current behavior (all notification types fire) until a user opts out.
--
-- These gate only the corresponding notification types in dispatch. Named
-- @you mentions are intentionally not included here: direct personal pings are
-- always delivered so a user cannot accidentally silence being paged directly.
-- Per-channel mute (channel_members.muted) still applies on top of these and
-- suppresses every type for that channel.

-- Broadcast mentions: @channel / @here / @everyone.
ALTER TABLE users ADD COLUMN IF NOT EXISTS notify_broadcast BOOLEAN NOT NULL DEFAULT TRUE;

-- Replies to a thread this user started.
ALTER TABLE users ADD COLUMN IF NOT EXISTS notify_thread_reply BOOLEAN NOT NULL DEFAULT TRUE;

-- Direct messages from another user.
ALTER TABLE users ADD COLUMN IF NOT EXISTS notify_dm BOOLEAN NOT NULL DEFAULT TRUE;
