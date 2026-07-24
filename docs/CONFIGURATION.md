# Configuration

Settings are layered, later sources overriding earlier ones:

1. `config.toml` (optional; see `config.toml.example`)
2. `APP__SECTION__KEY` environment variables (double-underscore separator)
3. Well-known bare env vars: `DATABASE_URL`, `JWT_SECRET`, `REDIS_URL`, `PORT`

## Settings

| Variable | Default | Meaning |
|----------|---------|---------|
| `DATABASE_URL` | (required) | PostgreSQL connection string |
| `JWT_SECRET` | (required, >=16 chars) | HMAC secret for signing JWTs |
| `REDIS_URL` | unset | Enables multi-instance realtime fan-out (optional) |
| `PORT` / `APP__SERVER__PORT` | `8080` | HTTP listen port |
| `APP__SERVER__CORS_ORIGINS` | `*` | Comma-separated allowlist (`*` = any) |
| `APP__SERVER__MAX_BODY_BYTES` | `26214400` | Request body cap (25 MiB) |
| `APP__AUTH__ACCESS_TOKEN_TTL_SECS` | `3600` | Access token lifetime |
| `APP__AUTH__REFRESH_TOKEN_TTL_SECS` | `2592000` | Refresh token lifetime |
| `APP__STORAGE__UPLOAD_DIR` | `./uploads` | Upload directory (also holds `avatars/`) |
| `APP__STORAGE__BACKUP_DIR` | `/app/backups` | Directory for admin DB backups |
| `APP__DATABASE__AUTO_MIGRATE` | `true` | Run migrations on startup |
| `APP__LOGGING__FORMAT` | `text` | `text` or `json` |

See `.env.example` for the full annotated list.

## Backup/restore & PostgreSQL client version

The admin backup feature shells out to `pg_dump`/`pg_restore`. `pg_dump` refuses
to run against a newer server, so the client major version must be at least the
server's. The Docker image installs `postgresql-client-16` to match the bundled
PostgreSQL 16 server. For a no-Docker setup, install a `postgresql-client` whose
major version matches your server.

## Upload size

Both the tower-http request-body limit and Axum's extractor limit are raised to
`APP__SERVER__MAX_BODY_BYTES` (25 MiB by default), so the single effective cap is
enforced in the upload handler. If you front the service with a reverse proxy,
raise its body-size limit to match or uploads fail at the proxy first. Avatar
uploads have their own smaller cap and must be images.

## Where backups land

The engine writes backups to `APP__STORAGE__BACKUP_DIR` (default `/app/backups`).
On **Docker**, that container directory is bind-mounted to the host via
`BACKUP_HOST_DIR` in `.env` (default `./backups`), so dumps appear on the host
automatically. On the **no-Docker** path there is no mapping: the engine writes
straight to `APP__STORAGE__BACKUP_DIR` on the real filesystem (set it wherever you
want), owned by whoever runs the process.
