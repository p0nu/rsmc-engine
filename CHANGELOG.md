# Changelog

## [0.11.0] - join/leave messages, upload robustness, and request timeouts

### Added
- **Channel join/leave system messages.** A user joining or leaving a channel
  now posts a system message to the feed ("`<name>` joined the channel",
  "`<name>` left the channel", or "`<name>` was removed from the channel" for an
  admin removal). These are authorless `message_type = 'system'` messages,
  broadcast live and persisted in history. Fires on signup auto-join to the
  default channel, on self-join via browse, and on admin add/remove. New
  additive migration (0013) adds a `message_type` column and makes `user_id`
  nullable; existing rows default to `'user'` and are unaffected.
- **Live member-list updates on join.** The signup auto-join now emits a
  `member_joined` event, so already-connected clients (e.g. an admin viewing the
  members list) see a new member appear without a refresh.

### Changed
- **Streaming file uploads.** Uploads are now written to disk chunk by chunk with
  the size cap enforced as they stream, instead of buffering the whole file in
  memory first. This bounds memory use regardless of file size and rejects an
  oversized upload early rather than after fully reading it.
- **HTTP request timeout.** A per-request timeout (120s) is applied so a slow or
  stuck client cannot hold a handler open indefinitely. WebSocket upgrades are
  unaffected (the long-lived socket lives outside the HTTP request).

### Fixed
- **Upload directory is validated at startup.** The engine now checks that the
  upload directory exists and is writable before accepting traffic, failing with
  an actionable message instead of surfacing generic errors on the first upload.
- **Avatar uploads accept images sent as `application/octet-stream`.** Image
  detection now also considers the file extension and magic bytes, so valid
  images that a client labels generically are no longer wrongly rejected.

## [0.10.0] - MFA, GDPR, sessions, audit, retention, metrics, and real SMTP

### Added
- **MFA (TOTP, RFC 6238).** Authenticator-app two-factor. Two-step enrollment
  (`POST /users/me/mfa/enroll` then `/confirm`), a short-lived challenge issued
  at login when MFA is enabled (exchanged at `POST /auth/mfa/verify`), and
  `/disable` requiring a current code. Secrets are stored encrypted at rest
  (AES-256-GCM). New additive migration (0012). Implemented from primitives
  already in the tree (hmac, sha1, data-encoding, ring), no new crates. Note:
  authenticator apps only, no backup codes or email fallback in this release.
- **GDPR data export and account deletion.** `GET /users/me/export` returns the
  caller's profile, messages, memberships, and sessions as JSON. `DELETE
  /users/me` follows an instance policy (`gdpr.deletion_mode`): `anonymize`
  (default) tombstones the user but keeps messages for thread integrity, or
  `hard_delete` removes the user and their messages (blocked with a clear error
  if the user owns channels). Config-only, no migration.
- **Active session management.** `GET /users/me/sessions` lists a user's refresh
  sessions with a device label and last-used time; `DELETE
  /users/me/sessions/:id` and `POST /users/me/sessions/revoke-all` revoke them.
  No IP addresses are stored (privacy-minimizing). New additive migration (0011).
- **Admin audit log.** Append-only recording of security-relevant admin actions
  (role changes, activate/deactivate, MFA enable/disable, account deletion),
  readable at `GET /system/audit`. New additive migration (0010). Writes are
  best-effort and never block the primary operation.
- **Message retention.** Optional config `retention.days` (0 = disabled); the
  scheduler soft-deletes messages older than the threshold. Config-only.
- **Prometheus metrics.** Dependency-free `GET /metrics` exposition of aggregate
  gauges, unauthenticated for scraping.
- **Notification preferences.** Per-user toggles for broadcast mentions, thread
  replies, and DMs (named @mentions always notify). New additive migration
  (0009), surfaced on the user object.

### Changed
- **Real SMTP over STARTTLS.** The SMTP transport now performs an actual TLS
  upgrade (via tokio-rustls, already in the tree) before sending credentials or
  message data, and refuses to send if the server will not STARTTLS. Fixes a bug
  where auth could be sent on a plaintext connection. No new crates.
- **`mfa_enabled` on the self user.** `GET /users/me` now reports the caller's
  MFA state so clients can render enable/disable correctly.

### Fixed
- **Read requests exempt from the write rate limit.** GET/HEAD/OPTIONS no longer
  count against the per-user write budget, fixing spurious "could not load
  messages" errors during normal use. Only mutations are limited.
- **Backup directory from config.** The backup path is read from
  `storage.backup_dir` (default `./backups`) as the single source of truth
  instead of a hardcoded container path.

### Notes
- Twelve migrations frozen (0001-0012); all 0.10.0 additions are additive.
- Review items for sign-off before 1.0: the MFA secret encryption key is derived
  from `JWT_SECRET` (rotating it invalidates enrolled TOTP secrets); GDPR
  hard-delete leaves gaps in others' threads by design (anonymize is the default
  for integrity).

## [0.9.0] - email flows, bare-binary defaults, and startup diagnostics

### Added
- **Email: verification, password reset, and email change.** New endpoints under
  `/auth` and `/users/me` issue single-use, expiring, hash-stored tokens for
  verifying an address, resetting a forgotten password, and changing an email
  (with re-verification of the new address). A new additive migration (0008)
  stores an `email_verified` flag and an `email_tokens` table.
- **Pluggable mail transport.** With no SMTP configured, a console transport logs
  the action link to the server log, so the flows work out of the box in
  development. Setting `smtp_host` switches to SMTP delivery. New `[email]`
  config section covers transport, the public base URL used in links, token
  lifetime, and a verification policy.
- **Soft-by-default verification.** Unverified users can still log in unless
  `require_verification` is set, in which case login is blocked until the address
  is confirmed. The first user is always auto-verified so a fresh instance cannot
  lock itself out before email is configured.

### Changed
- **Bare-binary friendly storage defaults.** Upload and backup directories now
  default to `./uploads` and `./backups` relative to the working directory
  instead of container paths, so the standalone binary works without extra
  configuration. Docker deployments set explicit paths and are unaffected.
- **Clearer first-run diagnostics.** Missing or short secrets, a missing database
  URL, and an unreachable database each produce a specific, actionable message,
  and the effective configuration (active paths, whether rate limiting is on) is
  logged once at startup.

### Notes
- No breaking changes. The 0008 migration is additive and nullable; existing
  databases upgrade with no manual steps.

### Added
- **Rate limiting.** A per-IP limiter on the auth endpoints (login/signup/
  refresh) protects against brute force, and a per-user limiter (keyed by token
  subject, IP fallback) across the API guards against write floods. Exhaustion
  returns `429` with a `Retry-After` header. Configurable via a new
  `[rate_limit]` section (`enabled`, `auth_max`/`auth_window_secs`,
  `write_max`/`write_window_secs`) with sensible defaults; in-memory and
  Redis-swappable in design.
- **Search filters.** `GET /search` now understands `from:username`, `in:#channel`,
  `has:file`, and `after:`/`before:` date operators (dates accept `YYYY-MM-DD` or
  RFC3339), parsed inline from the query or passed as explicit params. Results
  stay scoped to the caller's channels; an unknown `from:`/`in:` yields no rows
  rather than leaking a broader search.
- **Pagination.** `GET /channels/:id/members` and `GET /channels/:id/pins` are
  now keyset-paginated (`?limit=&cursor=`) and return `{ members|pins,
  next_cursor }`, so large channels no longer return unbounded payloads.
- **Prebuilt binaries.** A release workflow builds and attaches engine binaries
  on tagged releases: a statically linked Linux binary (musl) that runs on any
  distro with no system dependencies, and an Apple Silicon macOS binary.
- **Migration guardrails.** `.gitattributes` forces LF on `*.sql` (and other
  text) to stop line-ending checksum drift; `scripts/check-migrations.sh` plus a
  committed `migrations/.checksums` manifest and a CI workflow fail any change to
  an already-frozen migration; a `Makefile` provides `dev`/`dev-fresh`/
  `reset-db`/`check-migrations`/`freeze-migrations`.
- **Deploy documentation.** New `docs/DEPLOY.md` covers install styles (Docker /
  binary / source), the dev-vs-production database lifecycle, the migration
  guard, a health-check contract, and migration troubleshooting.

### Changed
- Startup diagnostics are now actionable: a missing/short `JWT_SECRET`, an unset
  `DATABASE_URL`, or an unreachable database each exit with a clear message
  telling you exactly what to set (with credentials redacted).

### Notes
- No new migrations; the schema is unchanged from 0.7.0.
- If upgrading a database created before the 0.7.0 migration-checksum fix, see
  the troubleshooting section in `docs/DEPLOY.md`.

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
