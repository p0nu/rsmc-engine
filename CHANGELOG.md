# Changelog

## [0.6.0] - admin overview

### Added
- **`GET /system/stats`** - admin-only workspace, server, and database overview
  for the admin console. Reports member counts (active/deactivated), channels by
  type, message totals plus 24-hour and 7-day activity, file count and storage
  used, database size, schema version, connection-pool usage, PostgreSQL
  version, server version and uptime, and the count of currently online members.
  All figures are cheap aggregates; nothing exposes secrets or message content.
- Integration test covering the reported figures and admin-only access.

## [0.5.0] - channel mute

### Added
- **Mute channel** - `PATCH /channels/:id/mute` takes `{ "muted": true|false }`
  and sets the mute state for the calling member only. Members-only; other
  members are unaffected.
- `GET /channels` now returns a `muted` flag per channel alongside
  `unread_count` and `last_read_at`.
- Muted members receive **no notifications** from that channel: direct
  messages, thread replies, `@mentions`, and `@channel` broadcasts are all
  suppressed. Unread counts continue to accrue so clients can still show
  quiet activity.
- Integration test covering mute suppression, unread accrual while muted, and
  restoration on unmute.

### Notes
- Migration `0005` adds a `muted` boolean to `channel_members`, defaulting to
  false. Additive and backward compatible; applies automatically on startup.

## [0.4.0] — search, pins, broadcast mentions

### Added
- **Message search** — `GET /search?q=...` runs full-text search over message
  content. Scoped to one channel with `&channel_id=...`, otherwise across every
  channel the caller belongs to.
- **Pinned messages** — `POST /messages/:id/pin` and `DELETE /messages/:id/pin`
  pin/unpin a message in its channel; `GET /channels/:id/pins` lists a channel's
  pins newest-first.
- **Broadcast mentions** — `@channel`, `@here`, and `@everyone` in a message
  now notify every other member of the channel (a `mention` notification with
  `"broadcast": true` in the payload).

### Notes
- Migration `0004` adds the FTS index and the two nullable pin columns; it
  applies automatically on startup and is backward compatible.

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
