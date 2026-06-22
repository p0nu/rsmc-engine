# rsmc-engine — WebSocket Protocol

The realtime channel delivers message, typing, presence, and notification
events, and lets clients subscribe to channels and broadcast typing indicators.

## Connecting

```
GET /api/v1/ws?token=<access_token>
```

Authentication is via the **`token` query parameter** carrying a valid access
JWT — browsers' `WebSocket` API can't set an `Authorization` header, so the
token rides in the URL. An invalid/expired token yields `401` before the
upgrade. On success the connection is upgraded and a hub session is registered;
the user is marked **online** when their first session connects and **offline**
when their last session disconnects.

A single user may hold multiple concurrent sessions (tabs/devices); events fan
out to all of them.

Browser example:
```js
const ws = new WebSocket(`wss://your-host/api/v1/ws?token=${accessToken}`);
ws.onmessage = (e) => {
  const event = JSON.parse(e.data);
  switch (event.type) {
    case "message_created": /* render event.message */ break;
    case "presence":        /* update event.user_id */ break;
    // …
  }
};
```

## Message framing

All frames are JSON text. Both directions use a tagged union with a `type`
discriminator (snake_case).

## Client → Server events

| `type`        | Fields           | Meaning                                              |
|---------------|------------------|------------------------------------------------------|
| `subscribe`   | `channel_id`     | Start receiving events for a channel you belong to.  |
| `unsubscribe` | `channel_id`     | Stop receiving events for a channel.                 |
| `typing`      | `channel_id`     | Broadcast that you're typing (to channel members).   |
| `ping`        | —                | Heartbeat; server replies with `pong`.               |

`subscribe`/`typing` require membership of the target channel; non-members are
ignored (and may receive an `error` frame).

Examples:
```json
{ "type": "subscribe", "channel_id": "8f3a…" }
{ "type": "typing", "channel_id": "8f3a…" }
{ "type": "ping" }
```

## Server → Client events

| `type`            | Fields                                   | When                                          |
|-------------------|------------------------------------------|-----------------------------------------------|
| `message_created` | `channel_id`, `message` (MessageView)    | A new message/reply is posted in the channel. |
| `message_updated` | `channel_id`, `message` (MessageView)    | A message is edited.                          |
| `message_deleted` | `channel_id`, `message_id`               | A message is (soft-)deleted.                  |
| `typing`          | `channel_id`, `user_id`                  | Another member is typing.                     |
| `presence`        | `user_id`, `online`, `last_seen`         | A user came online / went offline.            |
| `notification`    | `notification` (Notification)            | The caller received a notification.           |
| `pong`            | —                                        | Reply to `ping`.                              |
| `error`           | `message`                                | A client frame was rejected.                  |

Examples:
```json
{ "type": "message_created", "channel_id": "8f3a…", "message": { /* MessageView */ } }
{ "type": "typing", "channel_id": "8f3a…", "user_id": "1b2c…" }
{ "type": "presence", "user_id": "1b2c…", "online": false, "last_seen": "2026-06-16T09:30:00Z" }
{ "type": "pong" }
{ "type": "error", "message": "not a member of this channel" }
```

`MessageView` and `Notification` have the same shape as in the REST API
(see `docs/API.md`).

## Delivery semantics

- **Channel events** (`message_*`, `typing`) reach sessions that have
  `subscribe`d to that channel and whose user is a member.
- **User events** (`presence`, `notification`) reach all of that user's
  sessions (presence is also broadcast to observers tracking the user).
- Events are best-effort and **not** replayed on reconnect. After reconnecting,
  re-subscribe and backfill missed history via
  `GET /api/v1/channels/:id/messages`.

## Multi-instance fan-out

When `REDIS_URL` is configured (and the `redis-pubsub` feature is enabled,
which it is by default), events emitted on one server instance are published to
Redis and re-dispatched to local sessions on every other instance. This lets you
run the engine horizontally behind a load balancer with no sticky-session
requirement for correctness of event delivery. Without Redis the engine runs
fine as a single instance.

## Heartbeats & disconnects

Send `ping` periodically (e.g. every 30s) to keep intermediaries from idling the
socket; expect a `pong`. On disconnect the session is unregistered; when a
user's last session drops they are marked offline and a `presence` event is
emitted.
