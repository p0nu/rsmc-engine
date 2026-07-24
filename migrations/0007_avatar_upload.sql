-- ============================================================================
-- 0007: uploaded avatars
-- ============================================================================
-- Store avatars uploaded through the API on the local volume (like message
-- attachments) rather than only referencing an external avatar_url. The public
-- serve endpoint reads avatar_path; avatar_version is bumped on every change so
-- clients can cache-bust with ?v=N. All columns are nullable/defaulted, so this
-- is safe on an existing database.
ALTER TABLE users
    ADD COLUMN avatar_path         TEXT NULL,
    ADD COLUMN avatar_content_type TEXT NULL,
    ADD COLUMN avatar_version      INT NOT NULL DEFAULT 0;
