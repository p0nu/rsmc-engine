# Changelog

## [0.2.0] — unread & thread-count fixes

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
