# rsmc-engine: API Reference

REST base URL: **`/api/v1`**. Health probes are at the root: `/healthz`,
`/readyz`.

Bodies are JSON unless noted (file/avatar upload is multipart; downloads are
binary). Timestamps are RFC 3339 UTC strings. IDs are UUIDv4 strings.

## Authentication

The API uses JWT bearer tokens. Signup or login returns an `access_token`
(short-lived, default 1h) and a `refresh_token` (long-lived, default 30d, rotated
on use). Send the access token on every protected request:

```
Authorization: Bearer <access_token>
```

When the access token expires, exchange the refresh token at `POST /auth/refresh`
for a new pair. Refresh tokens are single-use: each refresh revokes the old token
and issues a new one. `POST /auth/logout` revokes all of the caller's refresh
tokens.

### Roles & permissions

System roles (`role` on the user): `admin`, `member`, `guest`. The first user to
sign up becomes `admin`. Admins change roles and deactivate users. Guests can
send and read but cannot create channels or upload files.

Per-channel roles (membership `role`): `owner`, `admin`, `member`. Channel
owners/admins manage channel settings, membership, and archival.

### Error format

```json
{ "error": "forbidden", "message": "not a member of this channel" }
```

Common statuses: `400` bad request/validation, `401` missing/invalid token, `403`
insufficient permission, `404` not found, `409` conflict (e.g. duplicate
email/username), `413` body too large, `429` rate limited, `500` internal.

## Auth endpoints

### `POST /auth/signup`
Create an account. First account becomes admin.

Request:
```json
{ "email": "ada@example.com", "username": "ada",
  "display_name": "Ada Lovelace", "password": "correct horse battery staple" }
```
Response `200`: an **AuthResponse**:
```json
{
  "access_token": "eyJ...", "refresh_token": "eyJ...",
  "token_type": "Bearer", "expires_in": 3600,
  "user": { /* UserPublic */ }
}
```
Errors: `409` if email or username is taken.

### `POST /auth/login`
Request: `{ "email": "ada@example.com", "password": "..." }`
Response `200`: an **AuthResponse**. `401` on bad credentials or deactivated
account.

### `POST /auth/refresh`
Request: `{ "refresh_token": "eyJ..." }`
Response `200`: a fresh **AuthResponse**; the supplied token is revoked. `401` if
the token is invalid, expired, revoked, or not a refresh token.

### `POST /auth/logout`
Auth required. Revokes all refresh tokens for the caller. Response `200`:
`{ "status": "ok" }`.

## Users

A **UserPublic** has: `id`, `email`, `username`, `display_name`, `role`,
`avatar_url`, `is_active`, `created_at`, and status fields `status_emoji`,
`status_text`, `status_expires_at` (all nullable; an expired status is reported
as absent).

### `GET /users/me`
Auth required. Returns the caller's **UserPublic**.

### `PATCH /users/me`
Auth required. Update own profile; omitted fields are unchanged.
```json
{ "display_name": "Ada L.", "avatar_url": "https://.../a.png" }
```
Response `200`: updated **UserPublic**. Broadcasts `profile_updated`.

### `PUT /users/me/status`
Auth required. Set or clear the caller's custom status. An empty or all-null body
clears it. `expires_in_secs`, when positive, auto-expires the status that many
seconds from now.
```json
{ "emoji": "🌴", "text": "on vacation", "expires_in_secs": 3600 }
```
Response `200`: updated **UserPublic**. Broadcasts `status_changed`.

### `POST /users/me/avatar`
Auth required. `multipart/form-data` with a single **`file`** field, image only,
size-capped. Stores the avatar, sets `avatar_url` to
`/api/v1/users/:id/avatar?v=N` (the version bumps for cache-busting), and
broadcasts `profile_updated`. Response `200`: updated **UserPublic**.

### `DELETE /users/me/avatar`
Auth required. Removes the uploaded avatar and clears `avatar_url`. Response
`200`: updated **UserPublic**. Broadcasts `profile_updated`.

### `GET /users/:id/avatar`
**Public, no auth**, so `<img src>` works. Streams the user's uploaded avatar, or
`404` if none. Cacheable; the `?v=N` on `avatar_url` busts the cache on change.

### `GET /users/:id`
Auth required. Returns a **UserPublic**. `404` if not found.

### `GET /users?q=<search>&limit=<n>`
Auth required. Search users by username/display_name (case-insensitive, partial).
`limit` clamps to 1..200 (default 50). Response `200`: `UserPublic[]`.

### `PUT /users/:id/role` *(admin only)*
`{ "role": "member" }`. Response `200`: updated **UserPublic**. `400` if you
target your own account.

### `POST /users/:id/deactivate` / `POST /users/:id/activate` *(admin only)*
Deactivate blocks login while preserving history; activate re-enables it.
Response `200`: `{ "status": "ok" }`. Deactivating your own account is `400`.

## Channels

A **Channel** has: `id`, `name` (null for DMs), `topic`, `channel_type`
(`public` | `private` | `direct` | `group`), `created_by`, `created_at`,
`updated_at`, and archival fields `archived_at`, `archived_by` (both null unless
archived).

### `POST /channels`
Auth required (create-channel permission). Creates a public or private channel;
caller becomes `owner`.
```json
{ "name": "engineering", "topic": "eng chatter", "private": false,
  "member_ids": ["uuid-a", "uuid-b"] }
```
Response `200`: the created **Channel**. `409` if the name is taken.

### `POST /channels/direct`
Auth required. Open or fetch a 1:1 DM. `{ "user_id": "uuid" }`. Idempotent:
returns the existing DM if one exists. Response `200`: the **Channel**.

### `GET /channels?include_archived=<bool>`
Auth required. Lists channels the caller belongs to, most-recently-active first.
Archived channels are hidden unless `include_archived=true`. Each item also
carries `unread_count`, `last_read_at`, and `muted`. Response `200`: `Channel[]`.

### `GET /channels/browse`
Auth required. The public-channel directory: every non-archived public channel
with `member_count` and `is_member`. Response `200`: `BrowsableChannel[]`.

### `POST /channels/:id/join`
Auth required. Join a public channel. Rejects private/DM/group (`403`) and
archived channels (`400`). Emits `member_joined`. Response `200`:
`{ "status": "ok" }`.

### `GET /channels/:id`
Auth required (member). Response `200`: the **Channel**.

### `PATCH /channels/:id` *(channel owner/admin)*
`{ "name": "eng", "topic": "new topic" }`. Response `200`: updated **Channel**.

### `POST /channels/:id/archive` / `POST /channels/:id/unarchive` *(channel owner/admin)*
Archive makes a channel read-only and hidden from the default list; unarchive
reverses it. DMs and groups cannot be archived (`400`). Emits `channel_archived`.
Response `200`: the updated **Channel**.

### `POST /channels/:id/read`
Auth required (member). Advances the caller's `last_read_at` to now and clears
unread notifications for the channel. Broadcasts a read receipt. Response `200`:
`{ "status": "ok" }`.

### `PATCH /channels/:id/mute`
Auth required (member). `{ "muted": true|false }`. Muting suppresses this
channel's notifications for the caller while unread counts keep accruing.
Response `200`: `{ "status": "ok" }`.

### `GET /channels/:id/members`
Auth required (member). Response `200`: `UserPublic[]`.

### `POST /channels/:id/members` *(channel owner/admin)*
`{ "user_id": "uuid" }`. Emits `member_joined`. Response `200`:
`{ "status": "ok" }`.

### `DELETE /channels/:id/members/:user_id`
Remove yourself, or (as owner/admin) anyone. The last owner/admin cannot leave
while other members remain (`400`). Emits `member_left`. Response `200`:
`{ "status": "ok" }`.

## Messages

A **Message** has: `id`, `channel_id`, `user_id`, `content`, `parent_id` (null =
root; set = thread reply), `reply_count`, `edited_at`, `deleted_at`,
`created_at`. A **MessageView** adds `author` (**UserPublic**) and `attachments`
(**Attachment[]**).

### `POST /channels/:id/messages`
Auth required (member; send-message permission). Posting to an archived channel is
`400`.
```json
{ "content": "hello @bob, see attached", "parent_id": null,
  "attachment_ids": ["uuid-of-uploaded-file"] }
```
`parent_id` (optional) must live in the same channel; posting a reply bumps the
parent's `reply_count`. `attachment_ids` (optional) are IDs from `POST /files`,
owned by the caller. `@username` mentions notify mentioned members. Response
`200`: the created **MessageView**. Emits `message_created`.

### `GET /channels/:id/messages?limit=<n>&before=<cursor>&after=<cursor>`
Auth required (member). Returns root messages (no thread replies), newest-first,
with keyset pagination. `limit` clamps to 1..200. Fetching advances the caller's
`last_read_at`. Response `200`:
```json
{ "messages": [ /* MessageView, newest first */ ], "next_cursor": "uuid-or-null" }
```
Pass `next_cursor` back as `before` to page older.

### `GET /messages/:id/thread`
Auth required (member of the parent's channel). Returns replies under a root
message, oldest-first. Response `200`: `MessageView[]`.

### `PATCH /messages/:id` *(author only)*
`{ "content": "edited text" }`. Response `200`: updated **MessageView**
(`edited_at` set). Emits `message_updated`.

### `DELETE /messages/:id`
Author or channel owner/admin. Soft-deletes (content cleared, `deleted_at` set;
row kept for thread integrity). Response `200`: `{ "status": "ok" }`. Emits
`message_deleted`.

## Scheduled messages

A **ScheduledMessage** has: `id`, `channel_id`, `user_id`, `content`,
`parent_id`, `scheduled_for`, `sent_at` (null until delivered), `created_at`. A
background sweeper delivers due messages as real messages (emitting
`message_created`), skipping archived or deleted channels.

### `POST /channels/:id/scheduled`
Auth required (member; send-message permission). Queues a message for future
delivery. `scheduled_for` must be in the future (`400` otherwise); archived
channels are `400`.
```json
{ "content": "morning update", "parent_id": null,
  "scheduled_for": "2026-07-25T09:00:00Z" }
```
Response `200`: the created **ScheduledMessage**.

### `GET /channels/:id/scheduled`
Auth required (member). The caller's pending (unsent) scheduled messages in this
channel, soonest first. Response `200`: `ScheduledMessage[]`.

### `DELETE /scheduled/:id`
Auth required. Cancels a pending scheduled message (author only, before it
sends). Response `200`: `{ "status": "ok" }`; `404` if already sent or not yours.

## Drafts

A **Draft** has: `user_id`, `channel_id`, `content`, `updated_at`. One draft per
user per channel.

### `GET /channels/:id/draft`
Auth required (member). Returns the caller's draft, or `{ "content": "" }` when
none. Response `200`: a **Draft**.

### `PUT /channels/:id/draft`
Auth required (member). Upserts the draft; a blank body deletes it.
`{ "content": "half-written thought" }`. Response `200`: `{ "status": "ok" }` (or
`{ "ok": true, "cleared": true }` when cleared).

## Files

An **Attachment** has: `id`, `uploader_id`, `channel_id` (null until bound to a
message), `filename`, `content_type`, `size_bytes`, `created_at`.

### `POST /files`
Auth required. `multipart/form-data` with a single **`file`** field. Subject to
the size cap (default 25 MiB). Stored unbound until referenced by a message via
`attachment_ids`. Response `200`: an **UploadResponse**:
```json
{ "id": "uuid", "filename": "diagram.png", "content_type": "image/png",
  "size_bytes": 12345, "url": "/api/v1/files/uuid" }
```

### `GET /files/:id`
Auth required. Streams the file with `Content-Disposition: attachment`. The
uploader can always fetch; otherwise the caller must be a member of the channel
the file is bound to. An unbound file is uploader-only. (For displaying images,
see the object-URL note in `docs/INTEGRATION.md`; message attachments are not
public, unlike avatars.)

## Notifications

A **Notification** has: `id`, `user_id`, `kind`
(`mention` | `direct_message` | `channel_invite` | `thread_reply`), `payload`
(JSON), `read_at`, `created_at`.

### `GET /notifications?unread_only=<bool>&limit=<n>`
Auth required. Newest-first; `limit` clamps to 1..200. Response `200`:
`Notification[]`.

### `GET /notifications/unread_count`
Auth required. Response `200`: `{ "unread_count": 3 }`.

### `POST /notifications/:id/read` / `POST /notifications/read_all`
Mark one or all read. Response `200`: `{ "status": "ok" }`.

## Presence

### `GET /presence/:user_id`
Auth required. Combines the persisted "last seen" row with live hub state.
Response `200`: `{ "user_id": "uuid", "online": true, "last_seen": "..." }`.

### `POST /presence/bulk`
Auth required. Batch lookup. `{ "user_ids": ["uuid-a", "uuid-b"] }`. Response
`200`: an array of presence objects.

## Webhooks

A **Webhook** has: `id`, `owner_id`, `channel_id` (null = all channels the owner
can see), `target_url`, `events` (string[]), `is_active`, `created_at`. The
signing `secret` is returned once at creation. Supported events:
`message.created`, `message.updated`, `message.deleted`.

### `POST /webhooks`
Auth required (global manage-webhooks permission, or channel owner/admin when
`channel_id` is set).
```json
{ "target_url": "https://example.com/hook",
  "events": ["message.created", "message.deleted"], "channel_id": null }
```
Response `200`: a **CreateWebhookResponse** including the `secret` (not
retrievable later):
```json
{ "id": "uuid", "target_url": "...", "events": ["..."], "secret": "base64..." }
```

### `GET /webhooks`
Auth required. Lists the caller's webhooks (without secrets). Response `200`:
`Webhook[]`.

### `DELETE /webhooks/:id` *(owner only)*
Response `200`: `{ "status": "ok" }`.

### Delivery & signatures
Each delivery is an HTTP `POST` to `target_url`:
```json
{ "event": "message.created", "timestamp": "...", "data": { /* MessageView */ } }
```
Headers: `Content-Type: application/json`, `X-RSMC-Event: <event>`, and
`X-Signature: sha256=<hex>` (HMAC-SHA256 of the raw body using the webhook
`secret`). Verify by recomputing the HMAC and comparing in constant time.

## System *(admin only)*

### `GET /system/info`
Basic server/build info. Response `200`: JSON.

### `GET /system/stats`
Workspace, server, and database overview (member/channel/message counts, activity
windows, storage, DB size, pool usage, online members). Response `200`: JSON.

### `POST /system/backup` / `GET /system/backups` / `POST /system/restore`
Create a database dump, list dumps, and restore one. See
`docs/CONFIGURATION.md` for the `pg_dump`/`pg_restore` client-version
requirement and where dumps land.

## Health

### `GET /healthz`
Liveness. Always `200`: `{ "status": "ok" }` while the process is up.

### `GET /readyz`
Readiness. Checks the database (`SELECT 1`) and reports live connections. `200`
when ready: `{ "status": "ready", "online_users": 12 }`; `503` with an error body
when the database is unreachable.

See `docs/WEBSOCKET.md` for the realtime protocol and `docs/SCHEMA.md` for the
database schema.
