-- ============================================================================
-- 0004: message search + pinned messages
-- ============================================================================

-- ---- Full-text search over message content --------------------------------
-- A GIN index over to_tsvector(content) makes websearch_to_tsquery lookups fast
-- without needing a stored tsvector column or any extension.
CREATE INDEX messages_content_fts_idx
    ON messages
    USING GIN (to_tsvector('english', content));

-- ---- Pinned messages ------------------------------------------------------
-- A message is pinned within its channel by recording who pinned it and when.
-- Nullable columns keep this additive; a pinned message has pinned_at set.
ALTER TABLE messages
    ADD COLUMN pinned_at TIMESTAMPTZ,
    ADD COLUMN pinned_by UUID REFERENCES users(id) ON DELETE SET NULL;

-- Fast "list pins for a channel" lookups, newest pin first.
CREATE INDEX messages_channel_pinned_idx
    ON messages (channel_id, pinned_at DESC)
    WHERE pinned_at IS NOT NULL;
