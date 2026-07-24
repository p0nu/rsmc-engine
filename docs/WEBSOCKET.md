# rsmc-engine: WebSocket Protocol

One realtime endpoint delivers message, typing, presence, membership, status,
profile, and notification events, and lets clients subscribe to channels and
broadcast typing.

## Connecting

```
GET /api/v1/ws?token=<access_token>
```

The access JWT rides in the `token` query parameter, since browsers' `WebSocket`
API can't set an `Authorization` header. An invalid or expired token yields `401`
before the upgrade. On success a hub session is registered: the user is marked
**online** when their first session connects and **offline** when their last
disconnects. A user may hold many concurrent sessions (tabs/devices); events fan
out to all of them.

```js
const ws = new WebSocket(`wss://your-host/api/v1/ws?token=${accessToken}`);
ws.onmessage = (e) => {
  const event = JSON.parse(e.data);
  switch (event.type) {
    case "message_created": /* render event.message */ break;
    case "presence":        /* update event.user_id */ break;
    // ...
  }
};
```

## Framing

All frames are JSON text with a snake_case `type` discriminator.

## Client to server

| `type`        | Fields       | Meaning                                          |
|---------------|--------------|--------------------------------------------------|
| `subscribe`   | `channel_id` | Start receiving events for a channel you belong to. |
| `unsubscribe` | `channel_id` | Stop receiving events for a channel.             |
| `typing`      | `channel_id` | Broadcast that you're typing.                    |
| `ping`        | (none)       | Heartbeat; server replies `pong`.                |

`subscribe`/`typing` require membership; non-members are ignored and may get an
`error` frame.

```json
{ "type": "subscribe", "channel_id": "8f3a..." }
{ "type": "typing", "channel_id": "8f3a..." }
{ "type": "ping" }
```

## Server to client

| `type`             | Fields                                   | When                                    |
|--------------------|------------------------------------------|-----------------------------------------|
| `message_created`  | `channel_id`, `message` (MessageView)    | A message/reply is posted.              |
| `message_updated`  | `channel_id`, `message` (MessageView)    | A message is edited.                    |
| `message_deleted`  | `channel_id`, `message_id`               | A message is soft-deleted.              |
| `typing`           | `channel_id`, `user_id`                  | Another member is typing.               |
| `presence`         | `user_id`, `online`, `last_seen`         | A user came online or went offline.     |
| `member_joined`    | `channel_id`, `user` (UserPublic)        | Someone joined the channel.             |
| `member_left`      | `channel_id`, `user_id`                  | Someone left or was removed.            |
| `channel_archived` | `channel_id`, `archived` (bool)          | A channel was archived/unarchived.      |
| `status_changed`   | `user_id`, `emoji`, `text`               | A user set or cleared their status.     |
| `profile_updated`  | `user_id`, `display_name`, `avatar_url`  | A user changed their name or avatar.    |
| `notification`     | `notification` (Notification)            | The caller received a notification.     |
| `pong`             | (none)                                   | Reply to `ping`.                        |
| `error`            | `message`                                | A client frame was rejected.            |

```json
{ "type": "message_created", "channel_id": "8f3a...", "message": { /* MessageView */ } }
{ "type": "presence", "user_id": "1b2c...", "online": false, "last_seen": "2026-06-16T09:30:00Z" }
{ "type": "member_joined", "channel_id": "8f3a...", "user": { /* UserPublic */ } }
{ "type": "profile_updated", "user_id": "1b2c...", "display_name": "Ada L.", "avatar_url": "/api/v1/users/1b2c.../avatar?v=3" }
{ "type": "error", "message": "not a member of this channel" }
```

`MessageView`, `UserPublic`, and `Notification` match the REST shapes (see
`docs/API.md`).

## Delivery semantics

- **Channel-scoped** (`message_*`, `typing`, `member_*`, `channel_archived`) reach sessions that have `subscribe`d to that channel and whose user is a member.
- **Workspace-wide** (`presence`, `status_changed`, `profile_updated`) reach every connected session, so cached copies of a user (message authors, DM peers, member lists) can be patched anywhere they appear.
- **User-scoped** (`notification`) reaches all of that user's sessions.
- Events are best-effort and are **not** replayed on reconnect. After reconnecting, re-subscribe and backfill via `GET /api/v1/channels/:id/messages`.

## Multi-instance fan-out

When `REDIS_URL` is set (the `redis-pubsub` feature is on by default), events
emitted on one instance are published to Redis and re-dispatched to local
sessions on every other instance, so the engine scales horizontally behind a load
balancer with no sticky sessions. Without Redis it runs fine as a single
instance.

## Heartbeats & disconnects

Send `ping` periodically (e.g. every 30s) so intermediaries don't idle the
socket; expect a `pong`. On disconnect the session is unregistered; when a user's
last session drops they go offline and a `presence` event fires.
