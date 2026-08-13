# rsmc-engine

**RSMC: Realtime Sync, Messaging & Collaboration.**

The Rust backend for a Slack/Mattermost style product: auth, realtime messaging,
history, files, presence, notifications, two-factor auth, webhooks, and admin
tooling. It pairs with the [rsmc-ui](https://github.com/p0nu/rsmc-ui) React
client for a complete, ready-to-run app, and because it is a clean headless API,
it can equally serve any other frontend (web, mobile, desktop, bots).

Built on Axum, SQLx/PostgreSQL, Tokio, and optional Redis pub/sub for horizontal
scaling.

> **The companion client is [rsmc-ui](https://github.com/p0nu/rsmc-ui)**, a full
> React app that pairs with this engine: channels, DMs, threads, files,
> reactions, presence, notifications, two-factor auth, and an admin console, all
> working out of the box. For most deployments, run the engine and point rsmc-ui
> at it, that is the intended, supported setup.
>
> Because the engine is a clean, headless API, you *can* also build your own
> frontend (web, mobile, desktop, bots) against it; the
> [Frontend Integration Guide](docs/INTEGRATION.md) covers auth, realtime, files,
> and permissions if you go that route.

## Features

The essentials you'd expect are all here: public/private/group channels and DMs,
threaded replies, edits, message history, file sharing, reactions, mentions,
drafts, custom status, avatars, and live presence. Beyond the basics, the parts
worth calling out:

- **Security-first auth.** JWT access + refresh tokens with rotation and revocation, Argon2id hashing, and optional TOTP two-factor (RFC 6238) with encrypted secrets at rest and a login challenge flow.
- **Session control.** Users can see their signed-in devices and revoke any or all (no IP addresses stored).
- **Privacy & compliance.** Self-service GDPR data export and account deletion (`anonymize` or `hard_delete`), plus optional age-based message retention.
- **Admin audit log.** Append-only record of security-relevant actions (role changes, activate/deactivate, MFA, account deletion).
- **Real email.** Verification, password reset, and email change over SMTP with a real STARTTLS upgrade (or logged to the console when SMTP is unset).
- **Realtime at scale.** One WebSocket endpoint streaming typed events, with optional Redis fan-out across multiple instances.
- **Notifications with preferences.** Mentions, thread replies, DMs, and invites, delivered live and persisted; per-user controls, with named mentions always notifying.
- **Webhooks.** Outbound HTTP subscriptions signed with HMAC-SHA256.
- **Operable.** A dependency-free Prometheus `/metrics` endpoint, database backup/restore, scheduled messages, and channel archive/browse.

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

## Quick start

Three ways to run the engine. All of them need a **PostgreSQL** database and two
secrets (`JWT_SECRET` and a database password); there are no insecure defaults,
so the engine will not start until both are set. Generate a secret with
`openssl rand -hex 32`.

- **Prebuilt binary** , simplest; no toolchain, you bring a Postgres.
- **Docker Compose** , batteries included; brings its own Postgres and Redis.
- **From source** , for developers modifying the engine.

### Option A , Prebuilt binary (no toolchain)

Every tagged release attaches ready-to-run binaries (packaged as `.tar.gz`): a
static Linux build (`x86_64-unknown-linux-musl`) that runs on any distro with no
dependencies, and an Apple Silicon macOS build (`aarch64-apple-darwin`). Grab the
latest from the [Releases page](https://github.com/p0nu/rsmc-engine/releases).

> **Prerequisite: a running [PostgreSQL](https://www.postgresql.org/download/)**
> (any recent version). The binary connects to an existing database and creates
> its tables automatically on boot, but it does not create the database itself
> or bundle its own (unlike the Docker option). Create a role and database once:
>
> ```bash
> sudo -u postgres psql <<'SQL'
> CREATE ROLE rsmc LOGIN PASSWORD 'change-me';
> CREATE DATABASE rsmc OWNER rsmc;
> SQL
> ```
>
> Then set `DATABASE_URL` (below) to match, e.g.
> `postgres://rsmc:change-me@localhost:5432/rsmc`.

```bash
# 1. Download and extract (Linux x86_64; v0.10.0)
curl -L -O https://github.com/p0nu/rsmc-engine/releases/download/v0.10.0/rsmc-engine-v0.10.0-x86_64-unknown-linux-musl.tar.gz
tar -xzf rsmc-engine-v0.10.0-x86_64-unknown-linux-musl.tar.gz
cd rsmc-engine-v0.10.0-x86_64-unknown-linux-musl

# 2. Point it at your database and set secrets
export DATABASE_URL="postgres://rsmc:change-me@localhost:5432/rsmc"
export JWT_SECRET="$(openssl rand -hex 32)"

# 3. Run (migrations create the tables on first boot)
./rsmc-engine
```

On Apple Silicon macOS, use the `aarch64-apple-darwin` tarball from the same
release instead. For other versions, swap `v0.10.0` for the tag you want (see
the [Releases page](https://github.com/p0nu/rsmc-engine/releases)).

The API comes up at `http://localhost:8080`. The binary applies its schema
migrations automatically; you only provide the empty database above.

**Common settings.** All config can be set via environment variables using the
`APP__<SECTION>__<FIELD>` pattern (double underscores). The ones you're most
likely to need:

```bash
# Change the port (e.g. if 8080 is already in use) and bind address
export APP__SERVER__PORT=9000            # default 8080
export APP__SERVER__HOST=0.0.0.0         # default 0.0.0.0

# Where uploads and backups are written
export APP__STORAGE__UPLOAD_DIR=./uploads    # default ./uploads
export APP__STORAGE__BACKUP_DIR=./backups    # default ./backups

# Lock CORS to your frontend(s) in production (default: allow any)
export APP__SERVER__CORS_ORIGINS=https://chat.example.com

# Optional: message retention and account-deletion policy
export APP__RETENTION__DAYS=0                 # 0 = keep forever
export APP__GDPR__DELETION_MODE=anonymize     # or hard_delete
```

`DATABASE_URL` and `JWT_SECRET` are also accepted in their bare form (shown
above) for twelve-factor setups. For the complete list, see
[docs/CONFIGURATION.md](docs/CONFIGURATION.md).

### Option B , Docker Compose (batteries included)

**1. Clone the repo.**

```bash
git clone https://github.com/p0nu/rsmc-engine.git
cd rsmc-engine
```

**2. Configure secrets.** Copy the example file, then open `.env` in an editor
and fill in the two required secrets:

```bash
cp .env.example .env
```

```bash
# .env
JWT_SECRET=<paste output of: openssl rand -hex 32>
POSTGRES_PASSWORD=<a strong password>
```

**3. (Optional) Choose where uploads and backups live.** By default they land in
`./uploads` and `./backups` next to the compose file. To relocate, set
`UPLOAD_HOST_DIR` / `BACKUP_HOST_DIR` in `.env`. The container runs as UID 10001,
so create and chown the directories once first:

```bash
mkdir -p ./uploads ./backups
sudo chown 10001:10001 ./uploads ./backups   # or: chmod 777 ./uploads ./backups
```

**4. Bring everything up.**

```bash
docker compose up --build
```

This starts PostgreSQL, Redis, and the engine, applying migrations on boot. The
API is at `http://localhost:8080`.

### Option C , From source (developers)

For hacking on the engine itself. You need the Rust toolchain and a PostgreSQL
you provide.

Install Rust via [rustup](https://rustup.rs) and set a default toolchain:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
```

(On Windows, use the installer from [rustup.rs](https://rustup.rs) instead.) Then
see [docs/NO_DOCKER.md](docs/NO_DOCKER.md) for the full Rust + PostgreSQL walkthrough
(Redis optional), or in short:

```bash
git clone https://github.com/p0nu/rsmc-engine.git
cd rsmc-engine
cp .env.example .env    # set JWT_SECRET + DATABASE_URL
cargo run --release
```

### First account and frontend

Smoke-test the engine:

```bash
curl localhost:8080/healthz
```

Then run [rsmc-ui](https://github.com/p0nu/rsmc-ui) against the engine and **sign
up through the app** , the first account you create becomes the workspace admin.
Use your own email and a password you choose; don't reuse example values.

Building your own frontend instead? See the [API](docs/API.md) and
[integration guide](docs/INTEGRATION.md). Signup is a `POST /api/v1/auth/signup`
with `email`, `username`, `display_name`, and `password`, the same endpoint the
UI calls; the first successful signup is granted admin.

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
- Optional TOTP two-factor auth; the shared secret is encrypted at rest. Note: authenticator apps only, there are no backup codes yet, so a lost device currently needs an admin to reset MFA.
- Refresh tokens are stored only as fingerprints and can be revoked (logout revokes all; refresh rotates them). Users can also list and revoke their own active sessions.
- Auth endpoints are rate-limited per IP; other write endpoints are rate-limited per user. Reads are exempt.
- Email is sent over SMTP with a real STARTTLS upgrade and will not send credentials on an unencrypted connection.
- Security-relevant admin actions are recorded to an append-only audit log.
- Set a strong `JWT_SECRET` and serve behind TLS. Docker Compose and the engine both refuse to start on a missing or too-short secret. Note: the MFA secret encryption key is currently derived from `JWT_SECRET`, so rotating it invalidates enrolled TOTP secrets (users re-enroll).
- Keep secrets out of version control: real values live in `.env` (git-ignored); only `.env.example` is committed. If a secret lands in git history, rotate it.
- Restrict `APP__SERVER__CORS_ORIGINS` to your real frontends in production.
- Webhook payloads are signed; verify the `X-Signature` header with the per-webhook secret returned once at creation.

> **Status:** v0.10.0 is feature-complete and in testing. The core is covered by
> an automated test suite, but it has not yet had an independent security review
> or broad production use. Treat it as ready to evaluate, not yet hardened for
> untrusted production.

## License

MIT. Provided as-is as a foundation for your own collaboration backend.
