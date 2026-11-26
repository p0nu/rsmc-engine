-- System messages: channel-feed lines not authored by a user, e.g.
-- "<username> joined the channel". Additive and backward-compatible:
--   * message_type defaults to 'user', so every existing row is a normal
--     message and all existing queries keep working unchanged.
--   * user_id becomes nullable so a system message can have no author. Normal
--     messages still set it; the app enforces that a 'user' message has an
--     author.

ALTER TABLE messages
    ADD COLUMN message_type TEXT NOT NULL DEFAULT 'user';

ALTER TABLE messages
    ALTER COLUMN user_id DROP NOT NULL;
