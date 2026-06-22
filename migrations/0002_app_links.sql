-- App links (workspace bookmarks).
--
-- Admin-curated shortcuts to external tools (GitHub, GitLab, etc.) shown to all
-- users in the Apps panel. Small by design; the application caps the count.

CREATE TABLE IF NOT EXISTS app_links (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    url         TEXT NOT NULL,
    position    INTEGER NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Ordered for stable display (position, then insertion order).
CREATE INDEX IF NOT EXISTS idx_app_links_position ON app_links (position, created_at);
