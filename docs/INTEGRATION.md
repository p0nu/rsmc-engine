# Building a Frontend for rsmc-engine

*A practical, framework-agnostic guide to designing and coding your own UI on top of the **rsmc-engine** backend (Realtime Sync, Messaging & Collaboration).*

rsmc-engine is a headless backend. It ships **no UI** — it exposes a REST API under `/api/v1` plus a WebSocket channel, and you bring whatever frontend you like (React, Vue, Svelte, SwiftUI, Flutter, a CLI — anything that speaks HTTP and WebSocket). This guide walks through everything you need to build a solid client: the mental model, auth, the data you'll render, realtime wiring, file handling, and the design decisions that make the difference between a demo and a real app.

---

## 1. The mental model

Before any code, understand what the engine gives you and where the boundaries are.

The engine owns: identity and auth (JWT), users and roles, channels (public, private, direct, group), messages with threads, file storage with access control, presence (who's online), notifications, and outbound webhooks. It enforces all permissions server-side.

Your frontend owns: everything visual and interactive. Layout, navigation, styling, optimistic updates, local caching, how you present errors, and crucially **how you reflect the permission model to users** (more on that below).

Two transports work together:

- **REST** (`/api/v1/...`) for request/response actions: log in, list channels, send a message, upload a file, page through history.
- **WebSocket** (`/api/v1/ws`) for realtime pushes: new messages, edits, deletes, typing indicators, presence changes, and notifications.

The golden rule: **REST is the source of truth; WebSocket is a live notifier.** You fetch state over REST and then *keep it fresh* via WebSocket events. Events are best-effort and are **not** replayed on reconnect — so after any reconnect you re-subscribe and backfill from REST. Never treat the socket as your only data path.

---

## 2. Prerequisites & connection basics

Point your client at the engine's base URL. In development the engine typically runs on `http://localhost:8080`, so:

- REST base: `http://localhost:8080/api/v1`
- WebSocket: `ws://localhost:8080/api/v1/ws` (use `wss://` in production)

All bodies are JSON unless noted (file upload is multipart; file download is binary). Timestamps are ISO-8601 UTC strings. IDs are UUID strings.

**CORS.** The engine sends permissive CORS by default (or an explicit allowlist you configure via `CORS_ORIGINS`). If you serve your frontend from a different origin than the API and hit CORS errors, set the allowed origins on the backend.

**Dev proxy (recommended).** Rather than hardcoding the backend host and fighting CORS, proxy `/api` from your dev server to the engine. With Vite, for example, you forward REST and WebSocket separately — the WebSocket entry needs its own `ws: true` flag, because mixing it with the REST proxy entry breaks multipart uploads:

```js
// vite.config.js
export default {
  server: {
    proxy: {
      // WebSocket: dedicated entry, ws: true
      "/api/v1/ws": { target: "ws://localhost:8080", ws: true },
      // REST: everything else under /api
      "/api": { target: "http://localhost:8080", changeOrigin: true },
    },
  },
};
```

With that proxy, your client can use relative paths (`/api/v1/...`) and a relative WebSocket URL, and you never think about CORS again in dev.

---

## 3. Authentication — the foundation

Everything except `/healthz`, `/readyz`, signup, login, and refresh requires a bearer token. Get auth right first; the rest builds on it.

### 3.1 The token model

Signup or login returns an **AuthResponse**:

```json
{
  "access_token": "eyJ...",
  "refresh_token": "eyJ...",
  "token_type": "Bearer",
  "expires_in": 3600,
  "user": { "id": "...", "email": "...", "username": "...",
            "display_name": "...", "role": "admin",
            "avatar_url": null, "is_active": true, "created_at": "..." }
}
```

- **access_token** — short-lived (default 1h). Send it on every protected request as `Authorization: Bearer <access_token>`.
- **refresh_token** — long-lived (default 30d), **single-use / rotated**. When the access token expires, `POST /auth/refresh` with the refresh token to get a *new pair*; the old refresh token is immediately revoked.
- The first account to sign up on a fresh database becomes `admin`.

### 3.2 Storing tokens

For a web app, the pragmatic choice is to keep tokens in memory and mirror them to `localStorage` so a page reload restores the session. Be aware of the standard trade-off: `localStorage` is readable by any script on your origin, so it's vulnerable to XSS. If your threat model demands it, the more secure pattern is to have your *own* tiny backend store the refresh token in an `HttpOnly` cookie and proxy auth — but for most self-hosted/internal deployments, `localStorage` with a strong CSP is the common, acceptable choice. Pick deliberately.

### 3.3 The auth flow you must implement

1. **Boot:** if you have a stored access token, call `GET /users/me` to validate it and load the current user. If it returns `401`, try a refresh; if that also fails, clear tokens and show the login screen.
2. **Login/Signup:** POST credentials, store the returned pair, set the current user, connect the WebSocket.
3. **Transparent refresh:** when *any* API call returns `401`, attempt a single refresh, then retry the original request once. If the refresh fails, log the user out cleanly.
4. **Logout:** `POST /auth/logout` (revokes all your refresh tokens), then clear local state and close the socket.

### 3.4 A reference HTTP client

A thin wrapper centralizes auth, refresh-on-401, timeouts, and error shaping. This is the single most important piece of client infrastructure — build it once and route everything through it.

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
  me:      () => request("GET", "/users/me"),
  updateMe:(b) => request("PATCH", "/users/me", { body: b }),
  searchUsers: (q, limit = 50) => request("GET", `/users?q=${encodeURIComponent(q)}&limit=${limit}`),
  // channels
  listChannels: () => request("GET", "/channels"),
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
  // files
  upload: (formData) => request("POST", "/files", { raw: formData }),
  // notifications & presence
  notifications: (unreadOnly = false) => request("GET", `/notifications?unread_only=${unreadOnly}`),
  unreadCount: () => request("GET", "/notifications/unread_count"),
  bulkPresence: (ids) => request("POST", "/presence/bulk", { body: { user_ids: ids } }),
};
```

The error envelope is always `{ "error": "<code>", "message": "<human text>" }`, so you can switch on `err.code` for behavior and show `err.message` to users.

---

## 4. The data model you'll render

Knowing the exact shapes lets you design components confidently.

**UserPublic** — `id`, `email`, `username`, `display_name`, `role` (`admin`|`member`|`guest`), `avatar_url`, `is_active`, `created_at`.

**Channel** — `id`, `name` (null for DMs), `topic`, `channel_type` (`public`|`private`|`direct`|`group`), `created_by`, `created_at`, `updated_at`.

**ChannelMember** — `channel_id`, `user_id`, `role` (`owner`|`admin`|`member`), `last_read_at`, `joined_at`.

**MessageView** — a message enriched for display: `id`, `channel_id`, `user_id`, `content`, `parent_id` (null = root, set = thread reply), `reply_count`, `edited_at`, `deleted_at`, `created_at`, plus `author` (UserPublic) and `attachments` (Attachment[]).

**Attachment** — `id`, `uploader_id`, `channel_id` (null until bound to a message), `filename`, `content_type`, `size_bytes`, `created_at`.

**Notification** — `id`, `user_id`, `kind` (`mention`|`direct_message`|`channel_invite`|`thread_reply`), `payload` (arbitrary JSON), `read_at`, `created_at`.

Because `MessageView` already embeds the author and attachments, your message component needs no extra lookups to render — name, avatar, text, and files all arrive together.

---

## 5. Loading and rendering the core views

A typical client has three regions: a list of channels (sidebar), the active channel's message stream (main), and contextual panels (members, threads, files). Here's how to populate them.

### 5.1 Channel list

`GET /channels` returns only the channels the caller belongs to, most-recently-active first — render them in that order. Note there is **no public channel directory endpoint**: you can't list channels the user hasn't joined. If you want a "browse channels" feature, that's a product decision the engine doesn't directly support; you'd build discovery via invites or by having admins add members.

Direct messages come back as channels with `channel_type: "direct"` and a null `name`. For those, derive the display name from the *other* member (fetch members and pick the one that isn't you).

### 5.2 Message history with pagination

`GET /channels/:id/messages?limit=50` returns **root messages only** (thread replies are excluded), **newest-first**, with a `next_cursor`:

```json
{ "messages": [ /* newest first */ ], "next_cursor": "uuid-or-null" }
```

To render a chat transcript you typically want oldest-at-top, so reverse the array for display. To load older messages (infinite scroll upward), pass the `next_cursor` back as `before`. When `next_cursor` is null, you've reached the beginning.

Fetching history also advances your `last_read_at` server-side, which is how unread state stays consistent across devices.

```js
// initial load
const { messages, next_cursor } = await api.history(channelId, { limit: 50 });
render(messages.slice().reverse());     // oldest at top
let cursor = next_cursor;

// load older when the user scrolls to the top
async function loadOlder() {
  if (!cursor) return;                   // reached the beginning
  const page = await api.history(channelId, { limit: 50, before: cursor });
  prepend(page.messages.slice().reverse());
  cursor = page.next_cursor;
}
```

### 5.3 Threads

A root message with `reply_count > 0` has replies. Fetch them with `GET /messages/:id/thread` (returns replies **oldest-first**). Render threads in a side panel or modal. To post into a thread, send a normal message with `parent_id` set to the root message's id.

### 5.4 Members & presence

`GET /channels/:id/members` returns `ChannelMember[]` with per-member `role`. To show who's online, call `POST /presence/bulk` with the member user-ids; it returns `{ user_id, online, last_seen }` for each. Seed presence once on load, then keep it live via WebSocket `presence` events (and optionally re-poll every 60s as a safety net).

---

## 6. Realtime with WebSocket

This is what makes the app feel alive. Connect after login, subscribe to channels as the user opens them, and translate incoming events into UI updates.

### 6.1 Connecting & authenticating

Browsers can't set an `Authorization` header on a WebSocket, so the access token rides in the query string:

```
wss://your-host/api/v1/ws?token=<access_token>
```

An invalid/expired token is rejected with `401` *before* the upgrade. A user can hold multiple sessions (tabs/devices); events fan out to all of them. The user is marked **online** when their first session connects and **offline** when the last disconnects.

### 6.2 The frames

All frames are JSON with a `type` discriminator (snake_case).

**Client → Server:**

| `type` | Fields | Meaning |
|---|---|---|
| `subscribe` | `channel_id` | Start receiving events for a channel you belong to |
| `unsubscribe` | `channel_id` | Stop receiving events for a channel |
| `typing` | `channel_id` | Broadcast that you're typing |
| `ping` | — | Heartbeat; server replies `pong` |

**Server → Client:**

| `type` | Fields | When |
|---|---|---|
| `message_created` | `channel_id`, `message` | New message/reply posted |
| `message_updated` | `channel_id`, `message` | Message edited |
| `message_deleted` | `channel_id`, `message_id` | Message soft-deleted |
| `typing` | `channel_id`, `user_id` | Another member is typing |
| `presence` | `user_id`, `online`, `last_seen` | A user came online / went offline |
| `notification` | `notification` | You received a notification |
| `pong` | — | Reply to `ping` |
| `error` | `message` | A client frame was rejected |

You must `subscribe` to a channel to receive its `message_*` and `typing` events; `presence` and `notification` always reach all your sessions.

### 6.3 A resilient realtime client

The two things people get wrong: not reconnecting, and not re-subscribing + backfilling after a reconnect. Handle both.

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

On `_open`, refetch history for the active channel (events aren't replayed, so you may have missed some while disconnected). Then apply live events:

```js
const rt = new Realtime((ev) => {
  switch (ev.type) {
    case "_open":            backfillActiveChannel(); break;
    case "message_created":  if (ev.channel_id === activeId) appendMessage(ev.message); bumpChannelOrder(ev.channel_id); break;
    case "message_updated":  replaceMessage(ev.message); break;
    case "message_deleted":  markDeleted(ev.message_id); break;
    case "typing":           showTyping(ev.channel_id, ev.user_id); break;   // auto-expire after ~4s
    case "presence":         setPresence(ev.user_id, ev.online, ev.last_seen); break;
    case "notification":     addNotification(ev.notification); break;
    case "error":            console.warn("ws error:", ev.message); break;
  }
});
rt.connect(accessToken);
```

### 6.4 Optimistic sending (optional but recommended)

When the user sends a message, you can render it immediately with a temporary id, then reconcile when either the REST response or the `message_created` event arrives. De-dupe by message id so the optimistic copy and the echoed event don't both show. Keep it simple at first — render on REST response — and add optimism once the basics work.

---

## 7. Files & attachments

Uploading is a two-step flow: upload the bytes, then reference the returned id when you send a message.

1. **Upload** — `POST /files` as `multipart/form-data` with a single field named **`file`**. You get back `{ id, filename, content_type, size_bytes, url }`. The file is stored *unbound* (not yet attached to any message). Respect the size cap (default 25 MiB).
2. **Attach** — include that id in `attachment_ids` when you `POST` the message. The file becomes bound to that channel/message.

```js
const fd = new FormData();
fd.append("file", fileInput.files[0]);   // field MUST be named "file"
const uploaded = await api.upload(fd);
await api.send(channelId, { content: caption, attachment_ids: [uploaded.id] });
```

**Displaying files is the part people get wrong.** The download endpoint `GET /files/:id` requires the `Authorization` header and enforces access control (uploader, or a member of the channel the file is bound to). That means you **cannot** point an `<img src="/api/v1/files/:id">` at it directly — the browser won't send your bearer token, so it gets a `401` and shows a broken image. Instead, fetch the bytes *with* auth and turn them into an object URL:

```js
async function fileObjectUrl(fileId) {
  const res = await fetch(`/api/v1/files/${fileId}`, {
    headers: { authorization: `Bearer ${access}` },
  });
  if (!res.ok) throw new Error("file fetch failed");
  return URL.createObjectURL(await res.blob());  // use as <img src> or <a href>
}
```

Remember to `URL.revokeObjectURL()` when the element unmounts to avoid leaking memory. Show a placeholder while loading and a graceful fallback on error.

---

## 8. Notifications & presence

**Notifications.** On load, call `GET /notifications/unread_count` for a badge and `GET /notifications` for the list. New ones arrive live as `notification` WebSocket events — increment the badge and prepend to the list. Mark items read with `POST /notifications/:id/read` (or `read_all`). The `kind` field (`mention`, `direct_message`, `channel_invite`, `thread_reply`) lets you route the user to the right place on click; `payload` carries kind-specific context.

**Presence.** Seed with `POST /presence/bulk` for the users you're showing, then keep live via `presence` events. A reasonable belt-and-suspenders approach is to also re-poll presence for visible users every ~60s, since events only fire on *change*.

---

## 9. Respecting the permission model (the part that defines quality)

The engine enforces permissions server-side, so a forbidden action simply returns `403`. But a good UI doesn't let users walk into walls — **it reflects permissions in the interface** by disabling or hiding actions the user can't perform, and explaining why.

The rules to mirror:

- **System roles:** `admin` can do everything (including manage users and roles); `member` can create channels, send messages, upload files, read; `guest` can send and read but **cannot create channels or upload files**.
- **Channel roles:** only a channel's `owner` (its creator) or a channel `admin` can edit the channel, add/remove members, or create channel-scoped webhooks.
- **Self-guards:** an admin can't deactivate themselves or change their own role (the backend returns `400`); reflect that by disabling those controls on your own row.
- **Messages:** the author can edit/delete their own; channel owner/admin can also delete others'.

Concretely: if `user.role === "guest"`, disable the "New channel" button and the file-upload control, with a tooltip like "Guests can't create channels." If the user isn't a channel owner/admin, hide the channel-settings gear and the add/remove-member controls. Disabling-with-explanation beats letting an action fail with a raw `403`.

A small permissions helper keeps this honest and centralized:

```js
const can = {
  createChannel: (u) => u.role === "admin" || u.role === "member",
  uploadFiles:   (u) => u.role === "admin" || u.role === "member",
  manageUsers:   (u) => u.role === "admin",
  adminChannel:  (u, ch) => u.role === "admin" || ch.created_by === u.id,
  editMessage:   (u, m) => m.user_id === u.id,
  deleteMessage: (u, m, ch) => m.user_id === u.id || u.role === "admin" || ch.created_by === u.id,
};
```

Note the engine returns channel membership with a per-member `role`, but the channel's *creator* is identified by `channel.created_by` — treat the creator as always an owner/member even before the member list loads, so the creator never sees a misleading "join this channel" prompt.

---

## 10. Error handling & resilience

A few habits separate a robust client from a flaky one:

- **Always time out requests.** A hung backend should surface a clear "couldn't reach the server" state, never an infinite spinner. (The reference client above aborts after 20s, or 120s for uploads.)
- **Refresh exactly once per 401**, then retry the original request once. De-dupe concurrent refreshes (the `refreshing` promise above) so ten parallel calls don't fire ten refreshes.
- **Boot must always resolve.** If the startup `GET /users/me` fails for any reason, fall through to the login screen rather than hanging. Keep the stored tokens so a later retry can succeed.
- **Map status codes to UX:** `401` → re-auth; `403` → "you don't have permission" (and ideally you'd have disabled the control already); `404` → gone/not found; `409` → conflict (duplicate email/username, or channel name taken — surface inline on the form); `413` → file too large; `429` → back off and retry; `500` → generic "something went wrong, try again."
- **WebSocket reconnect with capped exponential backoff**, then re-subscribe and backfill. Never assume the socket stayed up.
- **Surface the server's `message`.** The error envelope gives you human-readable text — show it instead of a generic string where it helps.

---

## 11. Suggested build order

You don't have to build everything at once. A sequence that always leaves you with something working:

1. **Auth shell** — signup/login forms, the HTTP client with refresh-on-401, boot via `GET /users/me`, logout. You now have a session.
2. **Channel list + read-only history** — `GET /channels`, click one, `GET /channels/:id/messages`, render the transcript. The app is now a read-only viewer.
3. **Sending** — the composer, `POST` messages, render on response. It's now a basic chat.
4. **Realtime** — connect the WebSocket, subscribe on channel open, apply `message_*` events. Messages now appear live across clients.
5. **Threads, members, presence** — side panels, `presence/bulk` + live presence, typing indicators.
6. **Files** — upload, attach, and the auth'd object-URL display.
7. **Notifications** — badge, list, live events, mark-read.
8. **Permissions polish** — disable/hide actions per role, self-guards, friendly explanations.
9. **Admin** (if you want it) — user list, role changes, deactivate/reactivate, and the system backup/restore screens.

Each step is independently demoable, which keeps momentum and makes bugs easy to localize.

---

## 12. Framework notes

The guide is framework-agnostic; the patterns map cleanly:

- **React/Vue/Svelte:** put the HTTP client and a realtime singleton at the app root; expose the current user, channels, and the active channel via context/store. Drive re-renders from a normalized message store keyed by id (makes edits/deletes and de-duping trivial).
- **Mobile (Swift/Kotlin/Flutter):** the same REST + WebSocket contract applies. Use the platform's native HTTP and WebSocket clients; the only browser-specific caveat (object URLs for images) doesn't apply — you can fetch file bytes with the auth header and render directly.
- **Avoid browser storage inside sandboxed preview environments** if you're prototyping in one; use in-memory state there and real `localStorage` in your actual app.

---

## 13. Quick reference — endpoints

```
Auth     POST   /auth/signup | /auth/login | /auth/refresh | /auth/logout
Users    GET    /users/me  PATCH /users/me  GET /users/:id  GET /users?q=&limit=
         PUT    /users/:id/role (admin)  POST /users/:id/deactivate|activate (admin)
Channels POST   /channels  GET /channels  POST /channels/direct
         GET    /channels/:id  PATCH /channels/:id
         GET    /channels/:id/members  POST /channels/:id/members  DELETE /channels/:id/members/:uid
Messages POST   /channels/:id/messages  GET /channels/:id/messages?limit=&before=&after=
         GET    /messages/:id/thread  PATCH /messages/:id  DELETE /messages/:id
Files    POST   /files (multipart field "file")  GET /files/:id
Notifs   GET    /notifications?unread_only=&limit=  GET /notifications/unread_count
         POST   /notifications/:id/read  POST /notifications/read_all
Presence GET    /presence/:user_id  POST /presence/bulk
Webhooks POST   /webhooks  GET /webhooks  DELETE /webhooks/:id
System   GET    /system/info  POST /system/backup  GET /system/backups  POST /system/restore   (admin)
Health   GET    /healthz  /readyz
Realtime GET    /api/v1/ws?token=<access_token>     (WebSocket)
```

Base URL for all REST: `/api/v1`. Auth header: `Authorization: Bearer <access_token>`. For the full request/response shapes see the engine's `docs/API.md`; for the realtime protocol see `docs/WEBSOCKET.md`.

---

*That's everything you need to build a first-class client on rsmc-engine. Start with the auth shell, get a read-only message view working, layer in realtime, and polish permissions last. The engine handles correctness and security; your job is to make it feel great.*
