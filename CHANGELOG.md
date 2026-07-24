# Changelog

## [0.7.0] - lifecycle, status, scheduling, drafts, avatars, and realtime fixes

### Added
- **Channel archival** - `POST /channels/:id/archive` and
  `POST /channels/:id/unarchive` (channel-admin only) mark a channel read-only.
  Posting to an archived channel is rejected (`400`). `GET /channels` hides
  archived channels by default; pass `?include_archived=true` to include them.
  Direct messages and groups can't be archived. Emits `channel_archived`.
- **Self-service join + browse directory** - `GET /channels/browse` returns
  public channels with `member_count` and `is_member`; `POST /channels/:id/join`
  lets a user join a public channel (private/DM/group/archived are rejected).
- **Membership realtime events** - `add_member` and self-join emit
  `member_joined`; `remove_member` emits `member_left`. A **last-admin guard**
  stops the final owner/admin from leaving while other members remain.
- **Custom status** - `PUT /users/me/status` sets or clears an emoji + text with
  an optional auto-expiry (`expires_in_secs`). Expired statuses read as absent.
  `UserPublic` now carries `status_emoji`, `status_text`, `status_expires_at`.
  Emits `status_changed`.
- **Scheduled messages** - `POST /channels/:id/scheduled` queues a message for
  future delivery; `GET /channels/:id/scheduled` lists the caller's pending
  ones; `DELETE /scheduled/:id` cancels. A background sweeper (15s interval)
  atomically claims due rows (`FOR UPDATE SKIP LOCKED`), inserts real messages,
  and fans out `message_created`, skipping archived/deleted channels.
- **Drafts** - `GET /channels/:id/draft` (returns `content:""` when none) and
  `PUT /channels/:id/draft` (empty body deletes) store one draft per channel
  per user.
- **Uploaded avatars** - `POST /users/me/avatar` (multipart, image-only,
  size-capped) and `DELETE /users/me/avatar`. `GET /users/:id/avatar` serves the
  image publicly (no auth) so `<img>` works; `avatar_url` becomes
  `/api/v1/users/:id/avatar?v=N`, the version bumping for cache-busting.
- Integration tests for archival, browse/join, the last-admin guard, custom
  status (including expiry), scheduled delivery via the sweeper, drafts, and the
  avatar upload/serve/delete cycle.

### Fixed
- **Presence propagation** - `mark_presence` now broadcasts via a new
  workspace-wide `emit_all` instead of `emit_user`, which only reached the
  user's own sessions. Presence dots now update for everyone (the core presence
  bug).
- **Profile propagation** - updating a display name or avatar (and avatar
  upload/delete) now emits `profile_updated` workspace-wide so other clients can
  patch cached copies of the user.

### Added (internals)
- `Hub::dispatch_all`, `EventBus::emit_all`, and `RoutedEvent::All` for
  workspace-wide broadcasts (Redis multi-instance aware).
- New WS events: `member_joined`, `member_left`, `channel_archived`,
  `status_changed`, `profile_updated`.

### Notes
- Migrations `0006` (archival columns, status columns, `scheduled_messages` and
  `drafts` tables) and `0007` (avatar columns) are additive and nullable, so
  they apply safely to an existing database on startup.

## [0.6.0] - admin overview

### Added
- **`GET /system/stats`** - admin-only workspace, server, and database overview
  for the admin console. Reports member counts (active/deactivated), channels by
  type, message totals plus 24-hour and 7-day activity, file count and storage
  used, database size, schema version, connection-pool usage, PostgreSQL
  version, server version and uptime, and the count of currently online members.
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

## [0.4.0] - search, pins, broadcast mentions

### Added
- **Message search** - `GET /search?q=...` runs full-text search over message
  content. Scoped to one channel with `&channel_id=...`, otherwise across every
  channel the caller belongs to.
- **Pinned messages** - `POST /messages/:id/pin` and `DELETE /messages/:id/pin`
  pin/unpin a message in its channel; `GET /channels/:id/pins` lists a channel's
  pins newest-first.
- **Broadcast mentions** - `@channel`, `@here`, and `@everyone` in a message
  now notify every other member of the channel (a `mention` notification with
  `"broadcast": true` in the payload).

### Notes
- Migration `0004` adds the FTS index and the two nullable pin columns; it
  applies automatically on startup and is backward compatible.

## [0.3.0] - read-receipt API

### Added
- **Read-receipt endpoint** - `GET /api/v1/channels/:id/receipts` returns each
  member's read cursor (`user_id`, `last_read_at`) for members of the channel.
  Non-members receive `403`.
- **`read` WebSocket event** - `{ channel_id, user_id, last_read_at }`, emitted
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

## [0.2.0] - unread & thread-count, reaction, mention autocomplete

### Added
- **Message reactions** - emoji reactions on messages (migration `0003`),
  `POST`/`DELETE /messages/:id/reactions`, aggregated reaction groups in message
  views, and `reaction_added` / `reaction_removed` WebSocket events.
- **Direct-message notifications** - sending a message in a direct channel
  notifies the other participant(s).
- **Channel-invite notifications** - adding a member to a channel notifies the
  invited user.
- **Unread tracking** - `GET /channels` returns `unread_count` and
  `last_read_at` per channel (powers sidebar unread badges and the "new
  messages" divider in the UI).
- **@mention autocomplete** (UI) - typeahead over `GET /users?q=` while typing
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
