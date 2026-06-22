# rsmc-engine — API Reference

Base URL for all REST endpoints: **`/api/v1`**
Health probes live at the root: `/healthz`, `/readyz`.

All request and response bodies are JSON unless noted (file upload/download are
binary/multipart). Timestamps are RFC 3339 / ISO 8601 UTC strings. IDs are
UUIDv4 strings.

---

## Authentication

The API uses JWT bearer tokens. After signup or login you receive an
`access_token` (short-lived, default 1h) and a `refresh_token` (long-lived,
default 30d, rotated on use).

Send the access token on every protected request:

```
Authorization: Bearer <access_token>
```

When the access token expires, exchange the refresh token at
`POST /auth/refresh` for a new pair. Refresh tokens are single-use: each refresh
revokes the old token and issues a new one (rotation). `POST /auth/logout`
revokes all of the caller's refresh tokens.

### Roles & permissions

System roles (`role` on the user): `admin`, `member`, `guest`. The **first user
to sign up automatically becomes `admin`.** Admins can change roles and
deactivate users.

Per-channel roles (`role` on channel membership): `owner`, `admin`, `member`.
Channel admins/owners manage channel settings and membership.

### Error format

Errors return the appropriate HTTP status and a JSON body:

```json
{ "error": "forbidden", "message": "not a member of this channel" }
```

Common statuses: `400` validation/bad request, `401` missing/invalid token,
`403` insufficient permission, `404` not found, `409` conflict (e.g. duplicate
email/username), `413` body too large, `429` rate limited, `500` internal.

---

## Auth endpoints

### `POST /auth/signup`
Create an account. First account becomes admin.

Request:
```json
{
  "email": "ada@example.com",
  "username": "ada",
  "display_name": "Ada Lovelace",
  "password": "correct horse battery staple"
}
```
Response `200`: an **AuthResponse**:
```json
{
  "access_token": "eyJ...",
  "refresh_token": "eyJ...",
  "token_type": "Bearer",
  "expires_in": 3600,
  "user": {
    "id": "…", "email": "ada@example.com", "username": "ada",
    "display_name": "Ada Lovelace", "role": "admin",
    "avatar_url": null, "is_active": true, "created_at": "…"
  }
}
```
Errors: `409` if email or username already taken.

### `POST /auth/login`
Request:
```json
{ "email": "ada@example.com", "password": "…" }
```
Response `200`: an **AuthResponse** (same shape as signup).
Errors: `401` on bad credentials or deactivated account.

### `POST /auth/refresh`
Request:
```json
{ "refresh_token": "eyJ…" }
```
Response `200`: a fresh **AuthResponse**. The supplied refresh token is revoked.
Errors: `401` if the token is invalid, expired, revoked, or not a refresh token.

### `POST /auth/logout`
Auth required. Revokes all refresh tokens for the caller.
Response `200`: `{ "status": "ok" }`

---

## Users

### `GET /users/me`
Auth required. Returns the caller's **UserPublic**.

### `PATCH /users/me`
Auth required. Update own profile. Fields are optional; omitted fields are left
unchanged.
```json
{ "display_name": "Ada L.", "avatar_url": "https://…/a.png" }
```
Response `200`: updated **UserPublic**.

### `GET /users/:id`
Auth required. Returns a user's **UserPublic**. `404` if not found.

### `GET /users?q=<search>&limit=<n>`
Auth required. List/search users. `q` matches username/display_name
(case-insensitive, partial). `limit` clamped to 1..200 (default 50).
Response `200`: `UserPublic[]`.

### `PUT /users/:id/role`  *(admin only)*
```json
{ "role": "member" }
```
Response `200`: updated **UserPublic**. `403` if caller isn't admin.

### `POST /users/:id/deactivate`  *(admin only)*
Deactivates an account (blocks login; preserves history).
Response `200`: `{ "status": "ok" }`.

---

## Channels

A **Channel** has: `id`, `name` (null for DMs), `topic`, `channel_type`
(`public` | `private` | `direct` | `group`), `created_by`, `created_at`,
`updated_at`.

### `POST /channels`
Auth required (permission: create channel). Creates a public or private channel;
caller becomes `owner`.
```json
{
  "name": "engineering",
  "topic": "eng chatter",
  "private": false,
  "member_ids": ["uuid-of-user-a", "uuid-of-user-b"]
}
```
Response `200`: the created **Channel**. `409` if the name is taken.

### `POST /channels/direct`
Auth required. Open (or fetch existing) 1:1 direct channel with another user.
```json
{ "user_id": "uuid-of-other-user" }
```
Response `200`: the **Channel** (`channel_type: "direct"`). Idempotent — returns
the existing DM if one already exists between the two users.

### `GET /channels`
Auth required. Lists channels the caller belongs to, most-recently-active first.
Response `200`: `Channel[]`.

### `GET /channels/:id`
Auth required (must be a member). Response `200`: the **Channel**.

### `PATCH /channels/:id`  *(channel owner/admin)*
```json
{ "name": "eng", "topic": "new topic" }
```
Response `200`: updated **Channel**.

### `GET /channels/:id/members`
Auth required (member). Response `200`: `ChannelMember[]`
(`channel_id`, `user_id`, `role`, `last_read_at`, `joined_at`).

### `POST /channels/:id/members`  *(channel owner/admin)*
```json
{ "user_id": "uuid" }
```
Response `200`: `{ "status": "ok" }`.

### `DELETE /channels/:id/members/:user_id`
Remove a member. Allowed if removing yourself, or if you're a channel
owner/admin. Response `200`: `{ "status": "ok" }`.

---

## Messages

A **Message** has: `id`, `channel_id`, `user_id`, `content`, `parent_id`
(null for root messages; set for thread replies), `reply_count`, `edited_at`,
`deleted_at`, `created_at`.

A **MessageView** is a Message enriched with `author` (**UserPublic**) and
`attachments` (**Attachment[]**).

### `POST /channels/:id/messages`
Auth required (member; permission: send message). Send a message or thread
reply.
```json
{
  "content": "hello @bob, see attached",
  "parent_id": null,
  "attachment_ids": ["uuid-of-uploaded-file"]
}
```
- `parent_id` (optional): the root message this reply belongs to; it must live
  in the same channel. Posting a reply bumps the parent's `reply_count`.
- `attachment_ids` (optional): IDs returned by `POST /files`, owned by the
  caller. They get bound to this channel/message.
- `@username` mentions in `content` notify mentioned channel members.

Response `200`: the created **MessageView**. Broadcasts a `message_created`
event to channel subscribers over WebSocket.

### `GET /channels/:id/messages?limit=<n>&before=<cursor>&after=<cursor>`
Auth required (member). Returns **root** messages (no thread replies),
newest-first, with keyset pagination. `before`/`after` take a message-id cursor;
`limit` clamps to 1..200. Fetching also advances the caller's `last_read_at`.

Response `200`: a **HistoryResponse**:
```json
{ "messages": [ /* MessageView, newest first */ ], "next_cursor": "uuid-or-null" }
```
Pass `next_cursor` back as `before` to page older.

### `GET /messages/:id/thread`
Auth required (member of the parent's channel). Returns the replies under a root
message, oldest-first.
Response `200`: `MessageView[]`.

### `PATCH /messages/:id`  *(author only)*
```json
{ "content": "edited text" }
```
Response `200`: updated **MessageView** (`edited_at` set). Emits
`message_updated`.

### `DELETE /messages/:id`
Author or channel owner/admin. Soft-deletes (content cleared, `deleted_at` set;
the row remains for thread integrity). Response `200`: `{ "status": "ok" }`.
Emits `message_deleted`.

---

## Files

An **Attachment** has: `id`, `uploader_id`, `channel_id` (null until bound to a
message), `filename`, `content_type`, `size_bytes`, `created_at`.

### `POST /files`
Auth required. `multipart/form-data` with a single field named **`file`**.
Subject to the configured size cap (default 25 MiB). The file is stored, unbound,
until referenced by a message via `attachment_ids`.

Response `200`: an **UploadResponse**:
```json
{
  "id": "uuid",
  "filename": "diagram.png",
  "content_type": "image/png",
  "size_bytes": 12345,
  "url": "/api/v1/files/uuid"
}
```

### `GET /files/:id`
Auth required. Streams the file with `Content-Disposition: attachment`. Access
control: the uploader can always fetch; otherwise the caller must be a member of
the channel the attachment is bound to. An unbound file is uploader-only.

---

## Notifications

A **Notification** has: `id`, `user_id`, `kind`
(`mention` | `direct_message` | `channel_invite` | `thread_reply`), `payload`
(arbitrary JSON), `read_at`, `created_at`.

### `GET /notifications?unread_only=<bool>&limit=<n>`
Auth required. Lists the caller's notifications, newest-first. `limit` clamps to
1..200. Response `200`: `Notification[]`.

### `GET /notifications/unread_count`
Auth required. Response `200`: `{ "unread_count": 3 }`.

### `POST /notifications/:id/read`
Mark one as read. Response `200`: `{ "status": "ok" }`.

### `POST /notifications/read_all`
Mark all of the caller's notifications read. Response `200`:
`{ "status": "ok" }`.

---

## Presence

### `GET /presence/:user_id`
Auth required. Combines the persisted "last seen" row with live hub state.
Response `200`:
```json
{ "user_id": "uuid", "online": true, "last_seen": "…" }
```

### `POST /presence/bulk`
Auth required. Batch presence lookup.
```json
{ "user_ids": ["uuid-a", "uuid-b"] }
```
Response `200`: an array of presence objects (same shape as above).

---

## Webhooks

Outbound webhooks let external systems receive events over HTTP. A **Webhook**
has: `id`, `owner_id`, `channel_id` (null = all channels the owner can see),
`target_url`, `events` (string[]), `is_active`, `created_at`. The signing
`secret` is returned **once** at creation.

Supported event names: `message.created`, `message.updated`, `message.deleted`.

### `POST /webhooks`
Auth required. Either the global "manage webhooks" permission, or channel
owner/admin when `channel_id` is set.
```json
{
  "target_url": "https://example.com/hook",
  "events": ["message.created", "message.deleted"],
  "channel_id": null
}
```
Response `200`: a **CreateWebhookResponse** including the `secret` (store it; it
isn't retrievable later):
```json
{ "id": "uuid", "target_url": "…", "events": ["…"], "secret": "base64…" }
```

### `GET /webhooks`
Auth required. Lists webhooks owned by the caller (without secrets).
Response `200`: `Webhook[]`.

### `DELETE /webhooks/:id`  *(owner only)*
Response `200`: `{ "status": "ok" }`.

### Delivery & signatures
Each delivery is an HTTP `POST` to `target_url` with body:
```json
{ "event": "message.created", "timestamp": "…", "data": { /* MessageView */ } }
```
Headers:
- `Content-Type: application/json`
- `X-RSMC-Event: message.created`
- `X-Signature: sha256=<hex>` — HMAC-SHA256 of the raw request body using the
  webhook's `secret`.

Verify by recomputing `HMAC_SHA256(secret, raw_body)` and comparing in constant
time against the hex digest after the `sha256=` prefix.

---

## Health

### `GET /healthz`
Liveness. Always `200`: `{ "status": "ok" }` while the process is up.

### `GET /readyz`
Readiness. Checks the database (`SELECT 1`) and reports live connections.
`200` when ready:
```json
{ "status": "ready", "online_users": 12 }
```
`503` with an error body when the database is unreachable.

---

See **`docs/WEBSOCKET.md`** for the realtime protocol and **`docs/SCHEMA.md`**
for the database schema.
