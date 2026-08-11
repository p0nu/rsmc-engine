-- Active-session management: enough metadata to let a user recognize and
-- revoke their own sessions, while minimizing stored personal data.
--
-- Deliberately NO IP address is stored (raw IPs are personal data under GDPR
-- and add breach surface). We keep only a coarse device/user-agent string and
-- a last-used timestamp. Both nullable so pre-existing sessions need no
-- backfill; they simply present as "unknown device".

ALTER TABLE refresh_tokens ADD COLUMN IF NOT EXISTS user_agent TEXT;
ALTER TABLE refresh_tokens ADD COLUMN IF NOT EXISTS last_used_at TIMESTAMPTZ;
