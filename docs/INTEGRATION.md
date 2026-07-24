# Building a Frontend for rsmc-engine

A practical, framework-agnostic guide to designing and coding your own UI on top
of the rsmc-engine backend (Realtime Sync, Messaging & Collaboration).

rsmc-engine is headless. It exposes a REST API under `/api/v1` plus a WebSocket
channel, and you bring whatever frontend you like (React, Vue, Svelte, SwiftUI,
Flutter, a CLI, anything that speaks HTTP and WebSocket). This guide covers the
mental model, auth, the data you render, realtime wiring, files, and the design
decisions that separate a demo from a real app.

## 1. The mental model

The engine owns identity and auth (JWT), users and roles, channels (public,
private, direct, group), messages with threads, channel lifecycle
(archive/browse/join), scheduled messages, drafts, custom status, file storage
with access control, presence, notifications, and outbound webhooks. It enforces
all permissions server-side.

Your frontend owns everything visual and interactive: layout, navigation,
styling, optimistic updates, local caching, how you present errors, and how you
reflect the permission model to users.

Two transports work together:

- **REST** (`/api/v1/...`) for request/response actions: log in, list channels, send a message, upload a file, page history.
- **WebSocket** (`/api/v1/ws`) for realtime pushes: new messages, edits, deletes, typing, presence, membership, status/profile changes, and notifications.

The golden rule: **REST is the source of truth; WebSocket is a live notifier.**
Fetch state over REST, then keep it fresh via WebSocket events. Events are
best-effort and are **not** replayed on reconnect, so after any reconnect you
re-subscribe and backfill from REST. Never treat the socket as your only data
path.

## 2. Prerequisites & connection basics

In development the engine typically runs on `http://localhost:8080`:

- REST base: `http://localhost:8080/api/v1`
- WebSocket: `ws://localhost:8080/api/v1/ws` (`wss://` in production)

Bodies are JSON unless noted (file/avatar upload is multipart; download is
binary). Timestamps are ISO-8601 UTC strings. IDs are UUID strings.

**CORS.** The engine sends permissive CORS by default, or an explicit allowlist
via `CORS_ORIGINS`. If your frontend is on a different origin and you hit CORS
errors, set the allowed origins on the backend.

**Dev proxy (recommended).** Rather than hardcoding the host and fighting CORS,
proxy `/api` from your dev server to the engine. The WebSocket entry needs its
own `ws: true` flag; mixing it with the REST entry breaks multipart uploads:

```js
// vite.config.js
export default {
  server: {
    proxy: {
      "/api/v1/ws": { target: "ws://localhost:8080", ws: true },
      "/api": { target: "http://localhost:8080", changeOrigin: true },
    },
  },
};
```

With that, use relative paths (`/api/v1/...`) and a relative WebSocket URL, and
CORS never comes up in dev.

## 3. Authentication

Everything except `/healthz`, `/readyz`, signup, login, refresh, and the public
avatar serve requires a bearer token. Get auth right first.

### 3.1 The token model

Signup or login returns an **AuthResponse**:

```json
{
  "access_token": "eyJ...", "refresh_token": "eyJ...",
  "token_type": "Bearer", "expires_in": 3600,
  "user": { /* UserPublic */ }
}
```

- **access_token**: short-lived (default 1h). Send as `Authorization: Bearer <access_token>` on every protected request.
- **refresh_token**: long-lived (default 30d), single-use / rotated. When the access token expires, `POST /auth/refresh` for a new pair; the old refresh token is revoked immediately.
- The first account on a fresh database becomes `admin`.

### 3.2 Storing tokens

For a web app, keep tokens in memory and mirror them to `localStorage` so a
reload restores the session. The trade-off: `localStorage` is readable by any
script on your origin, so it's XSS-exposed. If your threat model demands more,
have your own tiny backend hold the refresh token in an `HttpOnly` cookie and
proxy auth. For most self-hosted deployments, `localStorage` with a strong CSP is
the common, acceptable choice. Pick deliberately.

### 3.3 The auth flow to implement

1. **Boot:** if a stored access token exists, call `GET /users/me` to validate it and load the user. On `401`, try a refresh; if that also fails, clear tokens and show login.
2. **Login/Signup:** POST credentials, store the pair, set the user, connect the WebSocket.
3. **Transparent refresh:** on any `401`, attempt a single refresh, then retry the original request once. If refresh fails, log out cleanly.
4. **Logout:** `POST /auth/logout` (revokes all your refresh tokens), then clear local state and close the socket.

### 3.4 A reference HTTP client

A thin wrapper centralizes auth, refresh-on-401, timeouts, and error shaping.
Build it once and route everything through it.

```js
const BASE = "/api/v1";

let access = null, refresh = null;
let refreshing = null; // de-dupes concurrent refreshes

export function setTokens(t) {
  access = t?.access_token ?? null;
  refresh = t?.refresh_token ?? null;
  if (t) localStorage.setItem("auth", JSON.stringify({ access, refresh }));
  else localStorage.removeItem("auth");
}
export function loadTokens() {
  try {
    const t = JSON.parse(localStorage.getItem("auth"));
    access = t?.access; refresh = t?.refresh;
  } catch { /* ignore */ }
  return access;
}

class ApiError extends Error {
  constructor(status, code, message) { super(message); this.status = status; this.code = code; }
  get isAuth() { return this.status === 401; }
}

async function refreshTokens() {
  const res = await fetch(`${BASE}/auth/refresh`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ refresh_token: refresh }),
  });
  if (!res.ok) throw new ApiError(res.status, "refresh_failed", "session expired");
  setTokens(await res.json());
}

async function request(method, path, { body, raw, retry = true, timeoutMs } = {}) {
  const headers = {};
  if (access) headers.authorization = `Bearer ${access}`;
  const init = { method, headers };
  if (raw) init.body = raw;                 // FormData for uploads
  else if (body !== undefined) {
    headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }

  // Always bound the request so a hung backend can't freeze the UI.
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeoutMs ?? (raw ? 120000 : 20000));
  init.signal = ctrl.signal;

  let res;
  try { res = await fetch(`${BASE}${path}`, init); }
  catch (e) {
    clearTimeout(t);
    throw new ApiError(0, "network", "Couldn't reach the server.");
  }
  clearTimeout(t);

  if (res.status === 401 && retry && refresh) {
    refreshing = refreshing || refreshTokens().finally(() => (refreshing = null));
    try { await refreshing; } catch { setTokens(null); throw new ApiError(401, "auth", "Please sign in again."); }
    return request(method, path, { body, raw, retry: false, timeoutMs });
  }
  if (!res.ok) {
    let code = "error", message = res.statusText;
    try { const j = await res.json(); code = j.error ?? code; message = j.message ?? message; } catch {}
    throw new ApiError(res.status, code, message);
  }
  if (res.status === 204) return null;
  return res.json();
}

export const api = {
  // auth
  signup: (b) => request("POST", "/auth/signup", { body: b, retry: false }),
  login:  (b) => request("POST", "/auth/login",  { body: b, retry: false }),
  logout: () => request("POST", "/auth/logout"),
  // users
  me:       () => request("GET", "/users/me"),
  updateMe: (b) => request("PATCH", "/users/me", { body: b }),
  setStatus:(b) => request("PUT", "/users/me/status", { body: b }),
  uploadAvatar: (fd) => request("POST", "/users/me/avatar", { raw: fd }),
  deleteAvatar: () => request("DELETE", "/users/me/avatar"),
  searchUsers: (q, limit = 50) => request("GET", `/users?q=${encodeURIComponent(q)}&limit=${limit}`),
  // channels
  listChannels: (includeArchived = false) =>
    request("GET", `/channels${includeArchived ? "?include_archived=true" : ""}`),
  browseChannels: () => request("GET", "/channels/browse"),
  joinChannel:  (id) => request("POST", `/channels/${id}/join`),
  archiveChannel:   (id) => request("POST", `/channels/${id}/archive`),
  unarchiveChannel: (id) => request("POST", `/channels/${id}/unarchive`),
  createChannel:(b) => request("POST", "/channels", { body: b }),
  openDirect:   (userId) => request("POST", "/channels/direct", { body: { user_id: userId } }),
  channelMembers:(id) => request("GET", `/channels/${id}/members`),
  // messages
  history: (id, { limit = 50, before } = {}) =>
    request("GET", `/channels/${id}/messages?limit=${limit}${before ? `&before=${before}` : ""}`),
  send: (id, b) => request("POST", `/channels/${id}/messages`, { body: b }),
  thread: (msgId) => request("GET", `/messages/${msgId}/thread`),
  editMessage: (id, content) => request("PATCH", `/messages/${id}`, { body: { content } }),
  deleteMessage:(id) => request("DELETE", `/messages/${id}`),
  // scheduled + drafts
  scheduleMessage: (id, b) => request("POST", `/channels/${id}/scheduled`, { body: b }),
  listScheduled:   (id) => request("GET", `/channels/${id}/scheduled`),
  cancelScheduled: (id) => request("DELETE", `/scheduled/${id}`),
  getDraft:  (id) => request("GET", `/channels/${id}/draft`),
  saveDraft: (id, content) => request("PUT", `/channels/${id}/draft`, { body: { content } }),
  // files
  upload: (formData) => request("POST", "/files", { raw: formData }),
  // notifications & presence
  notifications: (unreadOnly = false) => request("GET", `/notifications?unread_only=${unreadOnly}`),
  unreadCount: () => request("GET", "/notifications/unread_count"),
  bulkPresence: (ids) => request("POST", "/presence/bulk", { body: { user_ids: ids } }),
};
```

The error envelope is always `{ "error": "<code>", "message": "<human text>" }`,
so switch on `err.code` for behavior and show `err.message` to users.

## 4. The data model you'll render

**UserPublic**: `id`, `email`, `username`, `display_name`, `role`
(`admin`|`member`|`guest`), `avatar_url`, `is_active`, `created_at`, and status
fields `status_emoji`, `status_text`, `status_expires_at` (nullable; an expired
status comes back null).

**Channel**: `id`, `name` (null for DMs), `topic`, `channel_type`
(`public`|`private`|`direct`|`group`), `created_by`, `created_at`, `updated_at`,
`archived_at`, `archived_by` (null unless archived). `GET /channels` items also
carry `unread_count`, `last_read_at`, `muted`.

**BrowsableChannel** (from `/channels/browse`): a public channel plus
`member_count` and `is_member`.

**ChannelMember role** (from membership): `owner` | `admin` | `member`.
`GET /channels/:id/members` returns `UserPublic[]`; the channel's creator is
`channel.created_by`.

**MessageView**: `id`, `channel_id`, `user_id`, `content`, `parent_id` (null =
root, set = reply), `reply_count`, `edited_at`, `deleted_at`, `created_at`, plus
`author` (UserPublic) and `attachments` (Attachment[]). The author and
attachments are embedded, so a message renders with no extra lookups.

**Attachment**: `id`, `uploader_id`, `channel_id` (null until bound),
`filename`, `content_type`, `size_bytes`, `created_at`.

**ScheduledMessage**: `id`, `channel_id`, `user_id`, `content`, `parent_id`,
`scheduled_for`, `sent_at`, `created_at`.

**Draft**: `user_id`, `channel_id`, `content`, `updated_at`.

**Notification**: `id`, `user_id`, `kind`
(`mention`|`direct_message`|`channel_invite`|`thread_reply`), `payload` (JSON),
`read_at`, `created_at`.

## 5. Loading and rendering the core views

A typical client has three regions: a channel list (sidebar), the active
channel's message stream (main), and contextual panels (members, threads, files,
scheduled).

### 5.1 Channel list

`GET /channels` returns only channels the caller belongs to, most-recently-active
first. Archived channels are hidden unless you pass `?include_archived=true`.
Direct messages come back with `channel_type: "direct"` and a null `name`; derive
the display name from the other member.

### 5.2 Browse & join

`GET /channels/browse` lists public channels with `member_count` and `is_member`,
so you can offer a directory. For a channel where `is_member` is false, call
`POST /channels/:id/join` (public channels only), then open it. Private channels,
DMs, and groups are not joinable this way; those still rely on invites.

### 5.3 Message history with pagination

`GET /channels/:id/messages?limit=50` returns root messages only (no thread
replies), newest-first, with a `next_cursor`:

```json
{ "messages": [ /* newest first */ ], "next_cursor": "uuid-or-null" }
```

For a transcript you usually want oldest-at-top, so reverse for display. To page
older, pass `next_cursor` back as `before`; a null cursor means you've reached the
start. Fetching history also advances your `last_read_at` server-side.

```js
const { messages, next_cursor } = await api.history(channelId, { limit: 50 });
render(messages.slice().reverse());     // oldest at top
let cursor = next_cursor;

async function loadOlder() {
  if (!cursor) return;                   // reached the beginning
  const page = await api.history(channelId, { limit: 50, before: cursor });
  prepend(page.messages.slice().reverse());
  cursor = page.next_cursor;
}
```

### 5.4 Threads

A root message with `reply_count > 0` has replies. Fetch with
`GET /messages/:id/thread` (oldest-first). To post into a thread, send a normal
message with `parent_id` set to the root message's id.

### 5.5 Members & presence

`GET /channels/:id/members` returns `UserPublic[]`. For who's online, call
`POST /presence/bulk` with the member ids; it returns `{ user_id, online,
last_seen }` for each. Seed presence on load, then keep it live via WebSocket
`presence` events (optionally re-poll every 60s as a safety net).

## 6. Realtime with WebSocket

Connect after login, subscribe to channels as the user opens them, and translate
events into UI updates.

### 6.1 Connecting & authenticating

Browsers can't set an `Authorization` header on a WebSocket, so the access token
rides in the query string:

```
wss://your-host/api/v1/ws?token=<access_token>
```

An invalid/expired token is rejected with `401` before the upgrade. A user can
hold multiple sessions; events fan out to all. Online is set when the first
session connects, offline when the last disconnects.

### 6.2 The frames

All frames are JSON with a `type` discriminator (snake_case).

**Client to server:**

| `type` | Fields | Meaning |
|---|---|---|
| `subscribe` | `channel_id` | Start receiving events for a channel you belong to |
| `unsubscribe` | `channel_id` | Stop receiving events for a channel |
| `typing` | `channel_id` | Broadcast that you're typing |
| `ping` | (none) | Heartbeat; server replies `pong` |

**Server to client:**

| `type` | Fields | When |
|---|---|---|
| `message_created` | `channel_id`, `message` | New message/reply posted |
| `message_updated` | `channel_id`, `message` | Message edited |
| `message_deleted` | `channel_id`, `message_id` | Message soft-deleted |
| `typing` | `channel_id`, `user_id` | Another member is typing |
| `presence` | `user_id`, `online`, `last_seen` | A user came online / went offline |
| `member_joined` | `channel_id`, `user` | Someone joined the channel |
| `member_left` | `channel_id`, `user_id` | Someone left or was removed |
| `channel_archived` | `channel_id`, `archived` | A channel was archived/unarchived |
| `status_changed` | `user_id`, `emoji`, `text` | A user set or cleared their status |
| `profile_updated` | `user_id`, `display_name`, `avatar_url` | A user changed name or avatar |
| `notification` | `notification` | You received a notification |
| `pong` | (none) | Reply to `ping` |
| `error` | `message` | A client frame was rejected |

You must `subscribe` to a channel to receive its channel-scoped events
(`message_*`, `typing`, `member_*`, `channel_archived`). Workspace-wide events
(`presence`, `status_changed`, `profile_updated`) and your own `notification`s
always reach all your sessions, so you can patch cached copies of a user wherever
they appear (message authors, DM peers, member lists).

### 6.3 A resilient realtime client

The two things people get wrong: not reconnecting, and not re-subscribing +
backfilling after a reconnect. Handle both.

```js
class Realtime {
  constructor(onEvent) { this.onEvent = onEvent; this.subs = new Set(); this.backoff = 1000; }

  connect(token) {
    this.token = token;
    this.ws = new WebSocket(`/api/v1/ws?token=${encodeURIComponent(token)}`);
    this.ws.onopen = () => {
      this.backoff = 1000;                       // reset backoff
      for (const id of this.subs) this.raw({ type: "subscribe", channel_id: id });
      this.ping = setInterval(() => this.raw({ type: "ping" }), 30000);
      this.onEvent({ type: "_open" });           // tell the app to backfill
    };
    this.ws.onmessage = (e) => this.onEvent(JSON.parse(e.data));
    this.ws.onclose = () => {
      clearInterval(this.ping);
      this.onEvent({ type: "_closed" });
      setTimeout(() => this.connect(this.token), this.backoff);
      this.backoff = Math.min(this.backoff * 2, 30000);  // capped exponential backoff
    };
  }
  raw(obj) { if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(obj)); }
  subscribe(id) { this.subs.add(id); this.raw({ type: "subscribe", channel_id: id }); }
  unsubscribe(id) { this.subs.delete(id); this.raw({ type: "unsubscribe", channel_id: id }); }
  typing(id) { this.raw({ type: "typing", channel_id: id }); }
  close() { this.subs.clear(); this.ws?.close(); }
}
```

On `_open`, refetch history for the active channel (events aren't replayed). Then
apply live events:

```js
const rt = new Realtime((ev) => {
  switch (ev.type) {
    case "_open":            backfillActiveChannel(); break;
    case "message_created":  if (ev.channel_id === activeId) appendMessage(ev.message); bumpChannelOrder(ev.channel_id); break;
    case "message_updated":  replaceMessage(ev.message); break;
    case "message_deleted":  markDeleted(ev.message_id); break;
    case "typing":           showTyping(ev.channel_id, ev.user_id); break;   // auto-expire after ~4s
    case "presence":         setPresence(ev.user_id, ev.online, ev.last_seen); break;
    case "member_joined":    addMember(ev.channel_id, ev.user); break;
    case "member_left":      removeMember(ev.channel_id, ev.user_id); break;
    case "channel_archived": setArchived(ev.channel_id, ev.archived); break;
    case "status_changed":   patchUserStatus(ev.user_id, ev.emoji, ev.text); break;
    case "profile_updated":  patchUser(ev.user_id, ev.display_name, ev.avatar_url); break;
    case "notification":     addNotification(ev.notification); break;
    case "error":            console.warn("ws error:", ev.message); break;
  }
});
rt.connect(accessToken);
```

### 6.4 Optimistic sending (optional but recommended)

Render the user's message immediately with a temporary id, then reconcile when
either the REST response or the `message_created` event arrives. De-dupe by
message id so the optimistic copy and the echoed event don't both show. Start
simple (render on REST response) and add optimism once the basics work.

## 7. Files & avatars

**Message attachments** are a two-step flow, and access-controlled.

1. **Upload:** `POST /files` as `multipart/form-data` with a single **`file`** field. You get back `{ id, filename, content_type, size_bytes, url }`. The file is stored unbound. Respect the size cap (default 25 MiB).
2. **Attach:** include that id in `attachment_ids` when you `POST` the message.

```js
const fd = new FormData();
fd.append("file", fileInput.files[0]);   // field MUST be named "file"
const uploaded = await api.upload(fd);
await api.send(channelId, { content: caption, attachment_ids: [uploaded.id] });
```

`GET /files/:id` requires the `Authorization` header and enforces access control,
so you **cannot** point an `<img src>` straight at it (the browser won't send your
token). Fetch the bytes with auth and make an object URL:

```js
async function fileObjectUrl(fileId) {
  const res = await fetch(`/api/v1/files/${fileId}`, {
    headers: { authorization: `Bearer ${access}` },
  });
  if (!res.ok) throw new Error("file fetch failed");
  return URL.createObjectURL(await res.blob());  // use as <img src> or <a href>
}
```

Call `URL.revokeObjectURL()` when the element unmounts.

**Avatars are different: they're public.** Upload with
`POST /users/me/avatar` (multipart `file`, image only), which sets the user's
`avatar_url` to `/api/v1/users/:id/avatar?v=N`. That serve endpoint needs no auth,
so you can use `avatar_url` directly in `<img src>`. The `?v=N` busts the cache
when the avatar changes; `profile_updated` tells you to refresh cached copies.
`DELETE /users/me/avatar` removes it.

## 8. Status, scheduled messages & drafts

**Custom status.** `PUT /users/me/status` with `{ emoji, text, expires_in_secs }`
sets it; an empty body clears it. Other clients learn of the change via
`status_changed`; render the emoji/text next to the user's name. An expired
status reads back as null, so trust the server's value.

**Scheduled messages.** `POST /channels/:id/scheduled` with `content` and a future
`scheduled_for` queues a send; `GET .../scheduled` lists your pending ones;
`DELETE /scheduled/:id` cancels. When a scheduled message is delivered it arrives
as a normal `message_created` event, so no special handling is needed to display
it. A side panel listing pending items with a cancel button is a natural fit.

**Drafts.** `GET /channels/:id/draft` returns your saved draft (or
`{ content: "" }`); `PUT /channels/:id/draft` saves it, and a blank body deletes
it. Debounce saves as the user types, restore on channel change, and clear on
send.

## 9. Notifications & presence

**Notifications.** On load, `GET /notifications/unread_count` for a badge and
`GET /notifications` for the list. New ones arrive live as `notification` events;
increment the badge and prepend. Mark read with `POST /notifications/:id/read`
(or `read_all`). The `kind` routes the user on click; `payload` carries
kind-specific context.

**Presence.** Seed with `POST /presence/bulk`, then keep live via `presence`
events. Optionally re-poll visible users every ~60s, since events fire only on
change.

## 10. Respecting the permission model

The engine enforces permissions server-side, so a forbidden action returns `403`.
A good UI doesn't let users walk into walls: reflect permissions by disabling or
hiding actions the user can't perform, and explaining why.

The rules to mirror:

- **System roles:** `admin` can do everything (including manage users/roles); `member` can create channels, send messages, upload files, read; `guest` can send and read but **cannot** create channels or upload files.
- **Channel roles:** only a channel's `owner` (creator) or a channel `admin` can edit the channel, add/remove members, archive/unarchive, or create channel-scoped webhooks.
- **Last-admin guard:** the final owner/admin can't leave a channel while other members remain (the backend returns `400`); disable "leave" in that state.
- **Self-guards:** an admin can't deactivate themselves or change their own role (`400` / `400`); disable those controls on your own row.
- **Archived channels are read-only:** posting, scheduling, and joining are rejected; disable the composer and show why.
- **Messages:** the author can edit/delete their own; channel owner/admin can also delete others'.

A small helper keeps this centralized:

```js
const can = {
  createChannel: (u) => u.role === "admin" || u.role === "member",
  uploadFiles:   (u) => u.role === "admin" || u.role === "member",
  manageUsers:   (u) => u.role === "admin",
  adminChannel:  (u, ch) => u.role === "admin" || ch.created_by === u.id,
  post:          (u, ch) => !ch.archived_at,
  editMessage:   (u, m) => m.user_id === u.id,
  deleteMessage: (u, m, ch) => m.user_id === u.id || u.role === "admin" || ch.created_by === u.id,
};
```

Members come back as `UserPublic[]` without a per-member channel role, but the
creator is `channel.created_by`. Treat the creator as an owner/member even before
the member list loads, so they never see a misleading "join this channel" prompt.

## 11. Error handling & resilience

- **Always time out requests.** A hung backend should surface a clear "couldn't reach the server," never an infinite spinner (the reference client aborts after 20s, 120s for uploads).
- **Refresh exactly once per 401**, then retry once. De-dupe concurrent refreshes so ten parallel calls don't fire ten refreshes.
- **Boot must always resolve.** If the startup `GET /users/me` fails, fall through to login rather than hanging; keep tokens for a later retry.
- **Map status codes to UX:** `401` re-auth; `403` "no permission" (ideally already disabled); `404` gone; `409` conflict (duplicate email/username, taken channel name, surface inline); `413` file too large; `429` back off; `500` generic retry.
- **WebSocket reconnect with capped exponential backoff**, then re-subscribe and backfill.
- **Surface the server's `message`** where it helps, instead of a generic string.

## 12. Suggested build order

Each step leaves you something working:

1. **Auth shell:** signup/login, the HTTP client with refresh-on-401, boot via `GET /users/me`, logout.
2. **Channel list + read-only history:** `GET /channels`, click one, render the transcript.
3. **Sending:** the composer, `POST` messages, render on response.
4. **Realtime:** connect the WebSocket, subscribe on open, apply `message_*` events.
5. **Threads, members, presence:** side panels, `presence/bulk` + live presence, typing.
6. **Files & avatars:** attachment upload + object-URL display; avatar upload (public serve).
7. **Lifecycle:** archive/unarchive, browse + join, live `member_*` / `channel_archived`.
8. **Status, scheduled, drafts:** the profile status editor, schedule popover + panel, per-channel drafts.
9. **Notifications:** badge, list, live events, mark-read.
10. **Permissions polish:** disable/hide per role, self-guards, archived read-only, friendly explanations.
11. **Admin** (optional): user list, role changes, deactivate/reactivate, backup/restore.

## 13. Framework notes

- **React/Vue/Svelte:** put the HTTP client and a realtime singleton at the app root; expose the current user, channels, and active channel via context/store. Drive re-renders from a normalized message store keyed by id (edits/deletes/de-dup become trivial). The same store makes `profile_updated` / `status_changed` patches a one-liner.
- **Mobile (Swift/Kotlin/Flutter):** the same REST + WebSocket contract applies with native clients. The browser-only object-URL caveat for attachments doesn't apply; fetch bytes with the auth header and render directly.
- **Sandboxed preview environments:** avoid browser storage there; use in-memory state and real `localStorage` in your actual app.

## 14. Quick reference: endpoints

```
Auth      POST   /auth/signup | /auth/login | /auth/refresh | /auth/logout
Users     GET    /users/me  PATCH /users/me  GET /users/:id  GET /users?q=&limit=
          PUT    /users/me/status
          POST   /users/me/avatar   DELETE /users/me/avatar   GET /users/:id/avatar (public)
          PUT    /users/:id/role (admin)  POST /users/:id/deactivate|activate (admin)
Channels  POST   /channels   GET /channels?include_archived=   POST /channels/direct
          GET    /channels/browse   POST /channels/:id/join
          POST   /channels/:id/archive | /channels/:id/unarchive
          GET    /channels/:id   PATCH /channels/:id   POST /channels/:id/read
          PATCH  /channels/:id/mute
          GET    /channels/:id/members  POST /channels/:id/members  DELETE /channels/:id/members/:uid
Messages  POST   /channels/:id/messages  GET /channels/:id/messages?limit=&before=&after=
          GET    /messages/:id/thread  PATCH /messages/:id  DELETE /messages/:id
Scheduled POST   /channels/:id/scheduled  GET /channels/:id/scheduled  DELETE /scheduled/:id
Drafts    GET    /channels/:id/draft  PUT /channels/:id/draft
Files     POST   /files (multipart field "file")  GET /files/:id
Notifs    GET    /notifications?unread_only=&limit=  GET /notifications/unread_count
          POST   /notifications/:id/read  POST /notifications/read_all
Presence  GET    /presence/:user_id  POST /presence/bulk
Webhooks  POST   /webhooks  GET /webhooks  DELETE /webhooks/:id
System    GET    /system/info | /system/stats   POST /system/backup | /system/restore   GET /system/backups   (admin)
Health    GET    /healthz  /readyz
Realtime  GET    /api/v1/ws?token=<access_token>     (WebSocket)
```

Base URL for all REST: `/api/v1`. Auth header:
`Authorization: Bearer <access_token>`. For full request/response shapes see
`docs/API.md`; for the realtime protocol see `docs/WEBSOCKET.md`.
