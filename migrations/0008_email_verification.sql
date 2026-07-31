-- Email verification, password reset, and email change support.
-- Additive and nullable: safe to apply to an existing populated database.

-- Whether the user's email address has been confirmed. Existing rows default
-- to FALSE; the application treats verification as soft (login still allowed)
-- unless configured strict, and auto-verifies the first user.
ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified BOOLEAN NOT NULL DEFAULT FALSE;

-- Single-purpose, single-use tokens for email-related flows. One table with a
-- purpose discriminator keeps the schema small; each row is consumed on use.
CREATE TABLE IF NOT EXISTS email_tokens (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 'verify' | 'reset' | 'change'
    purpose      TEXT NOT NULL,
    -- SHA-256 hash of the opaque token; the raw token is only ever emailed.
    token_hash   TEXT NOT NULL,
    -- For 'change': the new email address awaiting confirmation.
    new_email    TEXT,
    expires_at   TIMESTAMPTZ NOT NULL,
    consumed_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS email_tokens_hash_idx ON email_tokens (token_hash);
CREATE INDEX IF NOT EXISTS email_tokens_user_idx ON email_tokens (user_id);
