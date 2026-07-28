# rsmc-engine

**RSMC: Realtime Sync, Messaging & Collaboration.**

A headless team-collaboration backend in Rust. It provides the server side of a
Slack/Mattermost style product (auth, realtime messaging, history, files,
presence, notifications, webhooks) with no UI, so any frontend (web, mobile,
desktop, bots) can sit in front of it.

Built on Axum, SQLx/PostgreSQL, Tokio, and optional Redis pub/sub for horizontal
scaling.

> **Want a ready-made frontend?** [rsmc-ui](https://github.com/p0nu/rsmc-ui) is a
> companion React client: channels, DMs, threads, files, reactions, presence,
> and an admin console out of the box.
>
> **Building your own?** See the
> [Frontend Integration Guide](docs/INTEGRATION.md).

## Features

- **Auth & users:** JWT access + refresh tokens with rotation and revocation, Argon2id hashing, system roles (`admin`/`member`/`guest`) and per-channel roles. First user becomes admin.
- **Messaging:** public/private channels, DMs, group channels, threaded replies, edits, and soft-deletes. Markdown is rendered client-side.
- **Channel lifecycle:** archive/unarchive (read-only when archived), a public browse directory, and self-service join.
- **Scheduled messages:** queue a message for later; a background sweeper delivers it.
- **Drafts:** one saved draft per channel per user.
- **Custom status:** per-user emoji + text with optional auto-expiry.
- **History:** durable persistence with keyset pagination and per-channel read cursors.
- **Files & avatars:** access-controlled uploads, plus uploaded avatars served publicly for `<img>`.
- **Presence:** live online/offline tracking over WebSocket.
- **Notifications:** mentions, thread replies, DMs, and channel invites, delivered live and persisted.
- **Webhooks:** outbound HTTP subscriptions signed with HMAC-SHA256.
- **Realtime:** one WebSocket endpoint streaming typed events, with optional Redis fan-out across instances.
- **Admin:** database backup/restore and user management.

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

## Quick start (Docker Compose)

**1. Configure secrets.**

```bash
cp .env.example .env
```

Set the two required secrets (there are no insecure defaults, so the stack will
not start without them):

```bash
JWT_SECRET=<paste output of: openssl rand -hex 32>
POSTGRES_PASSWORD=<a strong password>
```

**2. (Optional) Choose where uploads and backups live.**

By default they land in `./uploads` and `./backups` next to the compose file. To
relocate, set `UPLOAD_HOST_DIR` / `BACKUP_HOST_DIR` in `.env`. The container runs
as UID 10001, so create and chown the directories once first:

```bash
mkdir -p ./uploads ./backups
sudo chown 10001:10001 ./uploads ./backups   # or: chmod 777 ./uploads ./backups
```

**3. Bring everything up.**

```bash
docker compose up --build
```

This starts PostgreSQL, Redis, and the engine, applying migrations on boot. The
API is at `http://localhost:8080`.

```bash
curl localhost:8080/healthz      # smoke test
```

Create the first account (it becomes admin):

```bash
curl -s localhost:8080/api/v1/auth/signup \
  -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","username":"ada","display_name":"Ada","password":"correct horse battery"}'
```

Point [rsmc-ui](https://github.com/p0nu/rsmc-ui) at the engine for a ready-made
interface, or build your own against the [API](docs/API.md) and
[integration guide](docs/INTEGRATION.md).

**Prefer no Docker?** See [docs/NO_DOCKER.md](docs/NO_DOCKER.md) for a Rust +
PostgreSQL setup (Redis optional).

## Documentation

- [docs/INTEGRATION.md](docs/INTEGRATION.md): build a frontend against the engine (auth, realtime, files, permissions).
- [docs/API.md](docs/API.md): REST request/response reference.
- [docs/WEBSOCKET.md](docs/WEBSOCKET.md): realtime protocol and event types.
- [docs/SCHEMA.md](docs/SCHEMA.md): database schema.
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md): settings, backups, upload limits.
- [docs/DEPLOY.md](docs/DEPLOY.md): database lifecycle (dev vs prod), migration safety, health checks, troubleshooting.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): internals, module layout, embedding the crate.

## Testing

```bash
# Unit tests
cargo test --lib

# Integration tests (require a throwaway Postgres; auto-skip without one)
docker run --rm -d --name rsmc-test-db -p 5433:5432 \
  -e POSTGRES_USER=rsmc -e POSTGRES_PASSWORD=rsmc -e POSTGRES_DB=rsmc_test \
  postgres:16-alpine
export TEST_DATABASE_URL=postgres://rsmc:rsmc@localhost:5433/rsmc_test
cargo test --test integration
```

## Security notes

- Passwords are hashed with Argon2id; plaintext is never stored or logged.
- Refresh tokens are stored only as fingerprints and can be revoked (logout revokes all; refresh rotates them).
- Set a strong `JWT_SECRET` and serve behind TLS. Docker Compose and the engine both refuse to start on a missing or too-short secret.
- Keep secrets out of version control: real values live in `.env` (git-ignored); only `.env.example` is committed. If a secret lands in git history, rotate it.
- Restrict `APP__SERVER__CORS_ORIGINS` to your real frontends in production.
- Webhook payloads are signed; verify the `X-Signature` header with the per-webhook secret returned once at creation.

## License

MIT. Provided as-is as a foundation for your own collaboration backend.
