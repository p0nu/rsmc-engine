-- Instance settings: admin-editable, runtime configuration that is NOT suited to
-- environment variables because it is changed from the admin UI while the server
-- runs. A simple key/value store so new settings can be added without a schema
-- change each time.
--
-- This is distinct from environment/config-file settings (DATABASE_URL,
-- JWT_SECRET, CORS, retention, GDPR mode, etc.), which stay in config.

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Name-display policy for channel message authors: 'full_name' or 'username'.
-- Seeded to 'full_name' (the common default). ON CONFLICT DO NOTHING so a
-- re-run never clobbers an admin's chosen value.
INSERT INTO settings (key, value)
VALUES ('name_display', 'full_name')
ON CONFLICT (key) DO NOTHING;
