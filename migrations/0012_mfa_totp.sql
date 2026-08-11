-- MFA (TOTP, RFC 6238). Authenticator-app only: no backup codes, no email
-- fallback in this iteration.
--
-- The TOTP secret is stored ENCRYPTED at rest (AES-256-GCM; see
-- services/mfa.rs). `mfa_secret_enc` holds nonce+ciphertext (base64). A secret
-- is written at enrollment but MFA is not considered active until the user
-- confirms a valid code, at which point `mfa_enabled` flips true. Storing an
-- unconfirmed secret with mfa_enabled=false lets enrollment be a two-step
-- confirm without a separate table.

ALTER TABLE users ADD COLUMN IF NOT EXISTS mfa_enabled BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS mfa_secret_enc TEXT;
