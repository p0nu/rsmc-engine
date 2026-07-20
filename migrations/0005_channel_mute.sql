-- ============================================================================
-- 0005: per-member channel mute
-- ============================================================================

-- A muted channel still tracks unread counts, but generates no notifications
-- for that member (mentions, DMs, thread replies, and @channel broadcasts are
-- all suppressed). Mute is per-member, so one user muting a channel does not
-- affect anyone else.
ALTER TABLE channel_members
    ADD COLUMN muted BOOLEAN NOT NULL DEFAULT FALSE;
