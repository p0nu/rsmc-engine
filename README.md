# rsmc-engine

**RSMC** — *Realtime Sync, Messaging & Collaboration.*

A plug-and-play, production-ready **team-collaboration backend** written in Rust.
It provides the server-side core of a Slack/Mattermost-style product —
authentication, real-time messaging, history, files, presence, notifications,
and webhooks — with **no UI**, so you can put any frontend (web, mobile,
desktop, bots) in front of it.

Built on **Axum**, **SQLx/PostgreSQL**, **Tokio**, and optional **Redis** pub/sub
for horizontal scaling.

> Building a client against this engine? See the **[Frontend Integration
> Guide](docs/INTEGRATION.md)** for a framework-agnostic walkthrough of auth,
> realtime, files, and the permission model.

---

## Features

- **Auth & users** — signup, login, JWT access + refresh tokens with rotation
  and server-side revocation, Argon2id password hashing, roles
  (`admin` / `member` / `guest`) and per-channel member roles
  (`owner` / `admin` / `member`). The first registered user becomes an admin.
- **Messaging** — public/private channels, direct messages, group channels, and
  threaded replies. Soft-deletes and edits with audit timestamps.
- **History** — durable persistence with efficient **keyset pagination** and
  per-channel read cursors.
- **Files** — multipart uploads with size limits and access-controlled download
  (only channel members, or the uploader for unattached files).
- **Presence** — live online/offline tracking over WebSocket, with a stored
  `last_seen` fallback.
- **Notifications** — mentions, thread replies, DMs, and channel invites,
  delivered in real time and persisted for later retrieval.
- **Webhooks** — outbound HTTP subscriptions to message events, signed with
  **HMAC-SHA256** (`X-Signature: sha256=…`).
- **Realtime** — a single WebSocket endpoint streams typed events; an internal
  event bus unifies local dispatch, optional Redis fan-out across instances, and
  webhook delivery.
- **Admin** — database backup & restore via the API (admin-only), plus user
  management (roles, deactivate/reactivate).

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

## Quick start

There are two supported ways to run the engine: **Docker Compose** (fastest, all
dependencies wired) or a **manual/no-Docker setup** (full control, see the
dedicated section below).

### Option A — Docker Compose (everything wired)

**1. Configure secrets.**

```bash
cp .env.example .env
```

Edit `.env` and set the two required secrets (the stack will not start without
them — there are no insecure defaults):

```bash
JWT_SECRET=<paste output of: openssl rand -hex 32>
POSTGRES_PASSWORD=<a strong password>
```

**2. Choose where uploads and backups live on your host (optional but
recommended).**

By default, uploaded files and database backups land in `./uploads` and
`./backups` next to the compose file — browsable folders, no extra setup. To put
them somewhere specific, set these in `.env`:

```bash
UPLOAD_HOST_DIR=/home/you/rsmc-data/uploads
BACKUP_HOST_DIR=/home/you/rsmc-data/backups
```

Because the container runs as UID 10001, create the directories once and make
them writable by that UID **before** starting (this is the one host-side step
Docker can't do for you):

```bash
mkdir -p ./uploads ./backups          # or your custom paths
sudo chown 10001:10001 ./uploads ./backups
# (if you can't use chown: `chmod 777 ./uploads ./backups`)
```

Doing this now means backups "just work" later — you won't have to reconfigure
anything when you first trigger one from the admin UI.

**3. Bring everything up.**

```bash
docker compose up --build
```

This starts PostgreSQL, Redis, and the engine. Compose injects your secrets from
`.env`, composing the database URL from the same credentials so they can't drift,
and bind-mounts your chosen host directories. The API is then available at
`http://localhost:8080`. Migrations run automatically on boot.

```bash
# smoke test
curl localhost:8080/healthz
```

> If you see an error like `POSTGRES_PASSWORD: set POSTGRES_PASSWORD in .env`
> when starting, that's the intended fail-fast guard — create your `.env` with
> the two secrets above.

Backups created from the admin **System** tab will then appear directly in your
`BACKUP_HOST_DIR` (or `./backups`) as `rsmc-<timestamp>.dump` files. In the UI,
leave the path field blank or use a bare filename — the engine writes to
`/app/backups` inside the container, which maps to your host folder.

### Option B — Manual setup without Docker

See **[Running without Docker](#running-without-docker)** below for a complete,
tested, step-by-step guide (Rust + PostgreSQL only; Redis is optional).

---

## Running without Docker

This is a complete, verified setup that uses only **Rust** and **PostgreSQL** —
no Docker, no Redis required. Every command below has been tested end to end.

> **Redis is optional.** The engine compiles with Redis support by default, but
> if `REDIS_URL` is not set it runs in single-instance mode using in-process
> event dispatch. You only need Redis to fan out realtime events across multiple
> engine instances behind a load balancer. For a single instance, skip it.

### 1. Install prerequisites

**Rust** (stable toolchain, 1.85+ recommended):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version          # confirm it's installed
```

**PostgreSQL** (16 recommended, to match the backup tooling — see the note in
[Configuration](#configuration)):

```bash
# Debian / Ubuntu
sudo apt update && sudo apt install -y postgresql postgresql-client

# macOS (Homebrew)
brew install postgresql@16 && brew services start postgresql@16

# Fedora
sudo dnf install -y postgresql-server postgresql && sudo postgresql-setup --initdb && sudo systemctl enable --now postgresql
```

Make sure the server is running before continuing:

```bash
pg_isready          # should report "accepting connections"
```

### 2. Create the database and role

Create a dedicated role and database for the engine. Run `psql` as the postgres
superuser (on most Linux installs that's `sudo -u postgres psql`; on Homebrew
macOS just `psql postgres`):

```bash
sudo -u postgres psql <<'SQL'
CREATE ROLE rsmc LOGIN PASSWORD 'rsmc';
CREATE DATABASE rsmc OWNER rsmc;
SQL
```

> Use a strong password in anything beyond local development, and update
> `DATABASE_URL` accordingly.

Verify you can connect as the new role:

```bash
PGPASSWORD=rsmc psql -h localhost -U rsmc -d rsmc -c '\conninfo'
```

If this fails with a peer-authentication error on Linux, ensure your
`pg_hba.conf` allows password (`md5`/`scram-sha-256`) auth for TCP connections
from `127.0.0.1`, then reload PostgreSQL.

### 3. Configure the engine

```bash
cp .env.example .env
```

Edit `.env` so it has at least these two values:

```bash
# Must match the role/database/password you created above
DATABASE_URL=postgres://rsmc:<your-password>@localhost:5432/rsmc

# Generate a strong secret (must be >= 16 chars)
JWT_SECRET=$(openssl rand -hex 32)
```

Leave `REDIS_URL` commented out (single-instance mode). The `.env` file is loaded
automatically at startup.

> For this manual path you can ignore the `POSTGRES_PASSWORD` field in
> `.env.example` — that one is only used by the Docker stack. Here, the password
> lives directly inside `DATABASE_URL`.

### 4. Build and run

```bash
# Compile a release binary (first build downloads and compiles dependencies)
cargo build --release

# Run it — database migrations apply automatically on first boot
cargo run --release
```

You should see log lines similar to:

```
INFO rsmc_engine::db: database migrations applied
INFO rsmc_engine: redis url not set; running in single-instance mode
INFO rsmc_engine: listening addr=0.0.0.0:8080
```

The API is now live at `http://localhost:8080`.

> To build a smaller binary **without** Redis support compiled in at all:
> `cargo build --release --no-default-features` (then `REDIS_URL` is ignored
> entirely).

### 5. Verify it works

```bash
# Liveness + readiness (readiness also checks the database)
curl localhost:8080/healthz      # -> {"status":"ok"}
curl localhost:8080/readyz       # -> {"status":"ready","online_users":0}

# Create the first account (the first user automatically becomes admin)
curl -s localhost:8080/api/v1/auth/signup \
  -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","username":"ada","display_name":"Ada","password":"correct horse battery"}'
# -> {"access_token":"...","refresh_token":"...","user":{...,"role":"admin"}}
```

If signup returns an `access_token` and a user with `"role":"admin"`, your
no-Docker setup is fully working. The schema (users, channels, messages,
attachments, notifications, …) was created automatically by the migration step.

### 6. Run as a background service (optional)

For a persistent deployment without Docker, run the release binary under a
process manager. A minimal **systemd** unit:

```ini
# /etc/systemd/system/rsmc-engine.service
[Unit]
Description=rsmc-engine
After=network.target postgresql.service

[Service]
WorkingDirectory=/opt/rsmc-engine
EnvironmentFile=/opt/rsmc-engine/.env
ExecStart=/opt/rsmc-engine/target/release/rsmc-engine
Restart=on-failure
User=rsmc

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rsmc-engine
sudo journalctl -u rsmc-engine -f      # follow logs
```

Put your release binary, `.env`, and (if used) an `uploads/` directory under the
`WorkingDirectory`. The `Restart=on-failure` policy is correct here — the engine
exits non-zero only on real errors and otherwise runs until signalled.

---

## First requests

```bash
# Sign up (first user becomes admin)
curl -s localhost:8080/api/v1/auth/signup \
  -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","username":"ada","display_name":"Ada","password":"correct horse battery"}'

# -> { "access_token": "...", "refresh_token": "...", "user": { ... } }

TOKEN=...   # access_token from above

# Create a channel
curl -s localhost:8080/api/v1/channels \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"name":"general","private":false}'

# Send a message
curl -s localhost:8080/api/v1/channels/<channel_id>/messages \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"content":"hello @world"}'
```

Connect to the realtime stream (token goes in the query string, since browsers
can't set headers on WebSocket handshakes):

```
ws://localhost:8080/api/v1/ws?token=<access_token>
```

Full request/response details are in **[docs/API.md](docs/API.md)**, the realtime
protocol in **[docs/WEBSOCKET.md](docs/WEBSOCKET.md)**, the database design in
**[docs/SCHEMA.md](docs/SCHEMA.md)**, and a complete client-building walkthrough
in **[docs/INTEGRATION.md](docs/INTEGRATION.md)**.

---

## Configuration

Configuration is layered, later sources overriding earlier ones:

1. `config.toml` (optional — see `config.toml.example`)
2. `APP__SECTION__KEY` environment variables (double underscore separator)
3. Well-known bare env vars: `DATABASE_URL`, `JWT_SECRET`, `REDIS_URL`, `PORT`

Key settings (see `.env.example` for the full list):

| Variable | Default | Meaning |
|----------|---------|---------|
| `DATABASE_URL` | — (required) | PostgreSQL connection string |
| `JWT_SECRET` | — (required, ≥16 chars) | HMAC secret for signing JWTs |
| `REDIS_URL` | unset | Enables multi-instance realtime fan-out (optional) |
| `PORT` / `APP__SERVER__PORT` | `8080` | HTTP listen port |
| `APP__SERVER__CORS_ORIGINS` | `*` | Comma-separated allowlist (`*` = any) |
| `APP__SERVER__MAX_BODY_BYTES` | `26214400` | Request body cap (25 MiB) |
| `APP__AUTH__ACCESS_TOKEN_TTL_SECS` | `3600` | Access token lifetime |
| `APP__AUTH__REFRESH_TOKEN_TTL_SECS` | `2592000` | Refresh token lifetime |
| `APP__STORAGE__UPLOAD_DIR` | `./uploads` | Upload directory |
| `APP__STORAGE__BACKUP_DIR` | `/app/backups` | Directory for admin DB backups |
| `APP__DATABASE__AUTO_MIGRATE` | `true` | Run migrations on startup |
| `APP__LOGGING__FORMAT` | `text` | `text` or `json` |

> **Backup/restore & PostgreSQL client version.** The admin backup feature
> shells out to `pg_dump`/`pg_restore`. `pg_dump` refuses to run against a
> *newer* server, so the client major version must be **≥ the database server's**.
> The provided Docker image installs `postgresql-client-16` from the official
> PGDG repository to match the bundled PostgreSQL 16 server. For a no-Docker
> setup, install a `postgresql-client` whose major version matches your server.

> **Upload size.** Both the tower-http request-body limit and Axum's extractor
> body limit are raised to `APP__SERVER__MAX_BODY_BYTES` (25 MiB by default), so
> the single effective cap is enforced in the upload handler. If you front the
> service with a reverse proxy (nginx, etc.), raise its body-size limit to match,
> or uploads will fail at the proxy before reaching the app.

> **Where backups land.** The engine always *writes* backups to
> `APP__STORAGE__BACKUP_DIR` (default `/app/backups`). On the **Docker** path,
> that container directory is bind-mounted to your host via `BACKUP_HOST_DIR` in
> `.env` (default `./backups`), so dumps appear on your host automatically — set
> it once during install (see Option A). On the **no-Docker** path there is no
> mapping: the engine writes straight to `APP__STORAGE__BACKUP_DIR` on the real
> filesystem (set it to wherever you want backups, e.g. `./backups`), owned by
> whoever runs the process.

---

## Architecture

```
                       ┌─────────────────────────────┐
   HTTP / WebSocket    │          Axum router         │
   ───────────────────►│   handlers/  middleware/     │
                       └───────────────┬──────────────┘
                                       │ AppState (db, jwt, hub, events)
              ┌────────────────────────┼────────────────────────┐
              ▼                        ▼                         ▼
        ┌───────────┐          ┌──────────────┐          ┌──────────────┐
        │  SQLx /   │          │   ws::Hub    │          │  EventBus    │
        │ Postgres  │          │ (live socket │◄────────►│ local + redis│
        │           │          │  sessions)   │          │ + webhooks   │
        └───────────┘          └──────────────┘          └──────┬───────┘
                                                                │
                                              outbound HTTP ────┘ (HMAC-signed)
```

- **`handlers/`** — one module per resource; thin, returning typed `AppResult`.
- **`middleware/`** — extractors for authenticated users (`AuthUser`,
  `AdminUser`) and request validation (`ValidatedJson`).
- **`services/`** — cross-cutting logic: access checks, the event bus,
  notifications, webhook signing/delivery, and the Redis pub/sub bridge.
- **`ws/`** — the WebSocket handler and the in-process `Hub` that holds live
  sessions and fans events to subscribers.
- **`models/`** — domain types, request/response DTOs, and the WS protocol enums.
- **`error/`** — a single `AppError` mapping cleanly to HTTP status + JSON.

When `REDIS_URL` is set, the event bus publishes every realtime event to Redis
and each instance re-dispatches to its own local sockets, so you can run many
stateless replicas behind a load balancer.

### Project layout

```
src/
  config/      layered settings loader
  db/          pool + migration runner
  auth/        JWT issuing/verification, password hashing
  models/      domain types, DTOs, WS protocol
  handlers/    REST + WS route handlers
  middleware/  auth extractors, request validation
  services/    access control, event bus, notifications, webhooks, pubsub
  ws/          WebSocket handler + session hub
  error/       AppError + IntoResponse
  state.rs     shared AppState
  lib.rs       router factory + bootstrap
  main.rs      executable entrypoint
migrations/    SQL schema (applied via sqlx::migrate!)
docs/          API.md, WEBSOCKET.md, SCHEMA.md, INTEGRATION.md
tests/         in-process integration tests
```

---

## Embedding in your own binary

The crate exposes a router factory and a one-call bootstrap, so you can mount the
engine inside a larger Axum app or customize startup:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = rsmc_engine::config::Settings::load()?;
    let state = rsmc_engine::bootstrap(settings).await?;
    let app = rsmc_engine::build_router(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
```

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

Each integration test runs in its own freshly-migrated schema for isolation.

---

## Security notes

- Passwords are hashed with **Argon2id**; plaintext is never stored or logged.
- Refresh tokens are stored only as fingerprints and can be revoked
  (logout revokes all of a user's refresh tokens; refresh rotates them).
- Set a strong `JWT_SECRET` (e.g. `openssl rand -hex 32`) and serve behind TLS.
  Both Docker Compose and the engine itself refuse to start on a missing or
  too-short secret, so there is no insecure default to forget about.
- Keep secrets out of version control: real values live in `.env`, which is
  git-ignored. Only `.env.example` (with blank secrets) is committed. If a real
  secret ever lands in git history, rotate it and scrub the history.
- Restrict `APP__SERVER__CORS_ORIGINS` to your real frontends in production.
- Webhook payloads are signed; verify the `X-Signature` header on your receiver
  using the per-webhook secret returned once at creation time.

---

## Toolchain note

This project targets a modern Rust toolchain (**1.85+** recommended; the
Dockerfile uses `rust:1.85`). The committed `Cargo.lock` pins a few transitive
dependencies for compatibility with older toolchains; on a current toolchain you
can regenerate it with `cargo update`.

---

## License

Provided as-is for use as a foundation for your own collaboration backend.
