# Changelog

## [0.2.0] — unread & thread-count fixes

### Fixed
- **Doubled DM unread badge** — a single direct message no longer counts as 2.
  The Chat-button total is now *derived* from the sum of per-channel unread
  badges (a single source of truth) instead of being tracked as a separate
  counter fed by both `notification` and `message_created` events.
- **Chat-button count stuck after reading** — opening a channel now clears its
  badge locally and the derived total updates in the same render, so it can no
  longer get stuck at 1 until a manual refresh.
- **Unread badge doubling on hot-reload** — per-message idempotent counting
  means a re-delivered event or a duplicate handler registration (e.g. after
  editing a component in dev) can no longer inflate a badge.
- **Thread reply-count inflation** — posting one reply showed as "4 replies".
  The parent's `reply_count` bump is now idempotent per reply id, immune to
  StrictMode double-invocation, WebSocket re-delivery, and the sender also
  receiving its own event.

### Added
- **`POST /channels/:id/read`** — advances the caller's read cursor and clears
  that channel's unread notifications atomically. The active channel calls it
  (debounced) on incoming messages so a later channel-list refresh can't
  resurrect already-seen messages as unread.
- Integration tests covering the single-DM unread count, `mark_read` clearing
  both unread surfaces, and once-per-reply thread counting.

### Added (earlier in 0.2.0)
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
