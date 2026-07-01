# Changelog

## [0.3.0] — read-receipt API

### Added
- **Read-receipt endpoint** — `GET /api/v1/channels/:id/receipts` returns each
  member's read cursor (`user_id`, `last_read_at`) for members of the channel.
  Non-members receive `403`.
- **`read` WebSocket event** — `{ channel_id, user_id, last_read_at }`, emitted
  to a channel whenever a member advances their read cursor (via
  `POST /channels/:id/read`). Lets connected clients reflect read state live.
- **`ReadReceipt` model** and an integration test covering the endpoint and its
  member-only access.

### Changed
- Internal cleanup: mutating endpoints that return `{ "ok": true }` now use a
  shared `handlers::ok()` helper instead of repeating the literal in 11 places.

### Notes
- Backward compatible. No new database migrations are required; the receipt
  data reuses the existing `channel_members.last_read_at` column.

## [0.2.0] — unread & thread-count, reaction, mention autocomplete

### Added
- **Message reactions** — emoji reactions on messages (migration `0003`),
  `POST`/`DELETE /messages/:id/reactions`, aggregated reaction groups in message
  views, and `reaction_added` / `reaction_removed` WebSocket events.
- **Direct-message notifications** — sending a message in a direct channel
  notifies the other participant(s).
- **Channel-invite notifications** — adding a member to a channel notifies the
  invited user.
- **Unread tracking** — `GET /channels` returns `unread_count` and
  `last_read_at` per channel (powers sidebar unread badges and the "new
  messages" divider in the UI).
- **@mention autocomplete** (UI) — typeahead over `GET /users?q=` while typing
  `@` in the composer.

### Notes
- All four advertised notification kinds (mention, thread_reply, direct_message,
  channel_invite) are now emitted.
- Additive changes; migration `0003` applies automatically on startup.

## [0.1.0]

Initial release: auth, channels (public/private/direct/group), threaded
messaging, history, files, presence, notifications (mentions + thread replies),
webhooks, realtime, admin tools (user management, backup & restore), and the
Apps quick-links feature.
