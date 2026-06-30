# rsmc-engine

**RSMC** — *Realtime Sync, Messaging & Collaboration.*

A plug-and-play, production-ready **team-collaboration backend** written in Rust.
It provides the server-side core of a Slack/Mattermost-style product —
authentication, real-time messaging, history, files, presence, notifications,
and webhooks — with **no UI**, so you can put any frontend (web, mobile,
desktop, bots) in front of it.

Built on **Axum**, **SQLx/PostgreSQL**, **Tokio**, and optional **Redis** pub/sub
for horizontal scaling.

> **Want a ready-made frontend?** [**rsmc-ui**](https://github.com/p0nu/rsmc-ui)
> is a companion React client for this engine — channels, DMs, threads, files,
> reactions, presence, and an admin console, out of the box.
>
> **Building your own?** See the **[Frontend Integration Guide](docs/INTEGRATION.md)**
> for a framework-agnostic walkthrough of auth, realtime, files, and the
> permission model.

---

## Features

- **Auth & users** — JWT access + refresh tokens with rotation and revocation, Argon2id hashing, roles (`admin`/`member`/`guest`) and per-channel roles. First user becomes admin.
- **Messaging** — public/private channels, DMs, group channels, threaded replies; edits and soft-deletes with timestamps.
- **Reactions** — emoji reactions on messages, with realtime updates.
- **History** — durable persistence with keyset pagination and per-channel read cursors.
- **Files** — multipart uploads with size limits and access-controlled download.
- **Presence** — live online/offline tracking over WebSocket.
- **Notifications** — mentions, thread replies, direct messages, and channel invites, delivered in real time and persisted.
- **Webhooks** — outbound HTTP subscriptions, signed with HMAC-SHA256.
- **Realtime** — a single WebSocket endpoint streaming typed events; event bus with optional Redis fan-out across instances.
- **Admin** — database backup & restore and user management (roles, deactivate/reactivate).

---

## Tech stack

| Concern        | Choice                                            |
|----------------|---------------------------------------------------|
| HTTP framework | Axum 0.7 (Tower middleware, WebSockets)           |
| Async runtime  | Tokio                                             |
| Database       | PostgreSQL via SQLx (pooled, embedded migrations) |
| Auth           | `jsonwebtoken` (JWT), `argon2` (Argon2id)         |
| Realtime scale | Optional Redis pub/sub (`redis-pubsub` feature)   |
| Config         | `config` + `dotenvy` (TOML + env vars)            |
| Observability  | `tracing` (text or JSON)                          |

---

## Quick start (Docker Compose)

**1. Configure secrets.**

```bash
cp .env.example .env
```

Set the two required secrets (the stack will not start without them — there are
no insecure defaults):

```bash
JWT_SECRET=<paste output of: openssl rand -hex 32>
POSTGRES_PASSWORD=<a strong password>
```

**2. (Optional) Choose where uploads and backups live.**

By default they land in `./uploads` and `./backups` next to the compose file. To
relocate, set `UPLOAD_HOST_DIR` / `BACKUP_HOST_DIR` in `.env`. The container runs
as UID 10001, so create and chown the directories once before starting:

```bash
mkdir -p ./uploads ./backups
sudo chown 10001:10001 ./uploads ./backups   # or: chmod 777 ./uploads ./backups
```

**3. Bring everything up.**

```bash
docker compose up --build
```

This starts PostgreSQL, Redis, and the engine, with migrations applied on boot.
The API is available at `http://localhost:8080`.

```bash
curl localhost:8080/healthz      # smoke test
```

Then create the first account (the first user automatically becomes admin):

```bash
curl -s localhost:8080/api/v1/auth/signup \
  -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","username":"ada","display_name":"Ada","password":"correct horse battery"}'
```

To use the engine with a ready-made interface, point the
[**rsmc-ui**](https://github.com/p0nu/rsmc-ui) client at it (see that repo's
README). Or build your own against the [API](docs/API.md) and
[integration guide](docs/INTEGRATION.md).

**Prefer no Docker?** See **[docs/NO_DOCKER.md](docs/NO_DOCKER.md)** for a
complete, tested Rust + PostgreSQL setup (Redis optional).

---

## Documentation

- **[rsmc-ui](https://github.com/p0nu/rsmc-ui)** — the companion React frontend; the quickest way to use the engine with a UI.
- **[docs/INTEGRATION.md](docs/INTEGRATION.md)** — build a frontend against the engine (auth, realtime, files, permissions).
- **[docs/API.md](docs/API.md)** — full REST request/response reference.
- **[docs/WEBSOCKET.md](docs/WEBSOCKET.md)** — realtime protocol and event types.
- **[docs/SCHEMA.md](docs/SCHEMA.md)** — database design.
- **[docs/CONFIGURATION.md](docs/CONFIGURATION.md)** — all settings, backup tooling, upload limits.
- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — internals, module layout, embedding the crate.

---

## Testing

```bash
# Unit tests (password hashing, mention parsing, HMAC/SHA-256 vectors)
cargo test --lib

# Integration tests — require a throwaway Postgres; auto-skip without one
docker run --rm -d --name rsmc-test-db -p 5433:5432 \
  -e POSTGRES_USER=rsmc -e POSTGRES_PASSWORD=rsmc -e POSTGRES_DB=rsmc_test \
  postgres:16-alpine
export TEST_DATABASE_URL=postgres://rsmc:rsmc@localhost:5433/rsmc_test
cargo test --test integration
```

---

## Security notes

- Passwords are hashed with **Argon2id**; plaintext is never stored or logged.
- Refresh tokens are stored only as fingerprints and can be revoked (logout
  revokes all; refresh rotates them).
- Set a strong `JWT_SECRET` and serve behind TLS. Both Docker Compose and the
  engine refuse to start on a missing or too-short secret.
- Keep secrets out of version control: real values live in `.env` (git-ignored);
  only `.env.example` is committed. If a secret lands in git history, rotate it.
- Restrict `APP__SERVER__CORS_ORIGINS` to your real frontends in production.
- Webhook payloads are signed; verify the `X-Signature` header using the
  per-webhook secret returned once at creation time.

---

## License

MIT. Provided as-is as a foundation for your own collaboration backend.
