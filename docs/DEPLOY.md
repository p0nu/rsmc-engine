# Deploying and operating rsmc-engine

This covers the database lifecycle (development vs production), migration safety,
and how to resolve the one migration error that occasionally surprises people.

For install steps see the README (Docker) and `docs/NO_DOCKER.md` (binary /
from source). For settings see `docs/CONFIGURATION.md`.

## Ways to run rsmc-engine

Three supported install styles, pick by audience:

1. **Docker Compose (fastest to try).** Bundles PostgreSQL and Redis, so a fresh
   machine goes from clone to running with two secrets set. See the README quick
   start. Best for evaluation and small self-hosted setups.
2. **Prebuilt binary + your own PostgreSQL (leanest service).** Download the
   release binary for your platform, point `DATABASE_URL` at a PostgreSQL you
   run, and manage it with systemd. Best for a long-running production service.
   See `docs/NO_DOCKER.md`.
3. **Build from source.** `cargo build --release`. Best for development or an
   unsupported platform.

### Prebuilt binaries

Each tagged release attaches binaries built by CI (see
`.github/workflows/release.yml`):

```
rsmc-engine-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz    (static Linux, no deps)
rsmc-engine-vX.Y.Z-aarch64-apple-darwin.tar.gz         (Apple Silicon macOS)
```

The **musl** Linux build is statically linked: it runs on any Linux distribution
with no system libraries to install (this is the recommended download for
servers). The macOS build is for running locally on Apple Silicon. On other
platforms (Intel Mac, ARM Linux, Windows), use Docker or build from source; the
target list in `.github/workflows/release.yml` is easy to extend if you want
additional prebuilt binaries.

```bash
tar -xzf rsmc-engine-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz
cd rsmc-engine-vX.Y.Z-x86_64-unknown-linux-musl
export DATABASE_URL=postgres://rsmc:pw@localhost:5432/rsmc
export JWT_SECRET=$(openssl rand -hex 32)
./rsmc-engine
```

The engine needs a PostgreSQL 16+ database regardless of install style; the
binary does not bundle one (no chat backend does). If `DATABASE_URL` or
`JWT_SECRET` is missing, or the database is unreachable, the engine exits
immediately with an actionable message telling you exactly what to set.

## Database lifecycle: development vs production

The engine uses forward-only migrations. Each `migrations/000N_*.sql` is applied
once; SQLx records a byte-for-byte checksum of the file in the `_sqlx_migrations`
table. On startup the engine re-checksums the files and compares. If an
already-applied migration's file has changed, it refuses to start:

```
Error: migration N was previously applied but has been modified
```

This is a safety feature, not a bug. It protects you from silently running a
schema that no longer matches what the database recorded. The two environments
handle it very differently.

### Development: data is disposable, reset freely

When you switch branches or versions and hit the checksum error, wipe and
reseed the local database:

```bash
make dev-fresh        # docker compose down -v && docker compose up
```

`dev-fresh` deletes the Postgres (and Redis) volume and re-applies every
migration from scratch, so the recorded checksums match the current files. This
is safe **only** because dev data doesn't matter. `make reset-db` does the same
for just the database volume.

### Production: never wipe, forward-only

A production database holds real user data; you can never `down -v` it. The
rules that guarantee the engine always boots cleanly:

1. **Never edit a migration that has shipped.** Not the SQL, not a comment, not
   whitespace. Any change alters the checksum and breaks every existing
   database. All schema changes go in a **new**, higher-numbered file
   (`migrations/000N_your_change.sql`). This is enforced automatically, see
   "Migration immutability guard" below.
2. **Test migrations on a staging copy first.** Restore a production backup to a
   staging instance, deploy the new version there, confirm it migrates cleanly,
   then deploy to production.
3. **Migrations are additive and forward-only.** For example, 0.7.0's `0006` and
   `0007` are additive and nullable, so they apply to a live database with no
   downtime and no manual step.

## Migration immutability guard

`migrations/.checksums` is a committed manifest of the frozen (shipped)
migrations. `scripts/check-migrations.sh` recomputes each file's checksum and
fails if a frozen migration changed or went missing. It runs in CI
(`.github/workflows/migrations.yml`) on any change under `migrations/`, so a
modified migration cannot be merged.

Everyday use:

```bash
make check-migrations     # verify nothing frozen has changed
```

When you add a new migration and it ships (at release), freeze it so it becomes
immutable from then on:

```bash
make freeze-migrations    # re-record migrations/.checksums
git add migrations/.checksums
git commit -m "Freeze migrations for vX.Y.Z"
```

Adding a brand-new migration file is always allowed; the guard only protects
files already listed in the manifest.

## Line endings

`.gitattributes` forces LF on `*.sql` (and other text files) on every platform.
This matters because a line-ending flip (LF <-> CRLF, common on Windows/WSL)
changes a migration's bytes, and therefore its checksum, without changing a
single visible character. Forcing LF removes that entire class of "migration was
modified" errors. If you ever normalize line endings across the repo, run
`git add --renormalize .` once and commit.

## Troubleshooting: "migration N was previously applied but has been modified"

This means the file on disk no longer matches the checksum a database recorded
when it applied that migration. Work through it by environment.

### It's a development database

Reseed it:

```bash
make dev-fresh
```

Done. If it recurs, the migration file genuinely differs from what your volume
applied, find out why (see causes below) and fix the file, not just the volume.

### It's a database you must preserve (staging/production)

Do **not** wipe. First identify what changed. Common causes:

- **A shipped migration was edited** (even a comment). Restore it to its original
  bytes so the checksum matches what the database applied, then redeploy. If it
  is committed, restore from the release tag, e.g.
  `git checkout vX.Y.Z -- migrations/000N_file.sql`.
- **A line-ending flip** (LF/CRLF) changed the bytes. The `.gitattributes` above
  prevents this going forward; restore the file and renormalize.

If you have **intentionally** re-baselined the migrations (you changed a file on
purpose and accept it as the new canonical version) and a preexisting database
needs to accept the new checksum without a wipe, update the stored checksum on
that database to match the current file. This is safe **only** when the change
does not alter the executable SQL (for example, a comment-only edit); if the SQL
itself changed, the database's actual schema differs from the file and you must
add a corrective new migration instead.

```bash
# 1. Get the current file's checksum from a FRESH database that just applied it:
docker compose down -v && docker compose up -d
docker compose exec postgres \
  psql -U rsmc -d rsmc -c \
  "SELECT version, encode(checksum,'hex') FROM _sqlx_migrations ORDER BY version;"

# 2. On the database you are preserving, set the stored checksum for that
#    migration to the value from step 1:
#    UPDATE _sqlx_migrations
#    SET checksum = decode('<hex-from-step-1>', 'hex')
#    WHERE version = <N>;
```

After that the recorded checksum matches the file and the engine boots. Repeat
per preserved database (this does not scale, which is exactly why the golden
rule exists).

### Note on the 0.7.0 -> later dash change

If you maintain a database that first applied the original 0.7.0 migrations and
you later adopt a build where a migration's bytes changed (for instance an em
dash in a comment was replaced with a hyphen), that database will show this
error on the changed migration. It is a **comment-only** change, so the schema is
unaffected; resolve it with the checksum-update procedure above (preserve data)
or `make dev-fresh` (dev). New/fresh databases are unaffected: they apply the
current files and record matching checksums from the start.

## Health and monitoring

The engine exposes stable endpoints for liveness, readiness, and version:

```
GET /healthz         -> 200 {"status":"ok"}                 process is up
GET /readyz          -> 200 {"status":"ready","online_users":N}   DB reachable
                     -> 503 {...}                            DB unreachable
GET /api/v1/system/info   (admin)                            build/version info
```

A minimal health checklist for a deployment:

- `GET /healthz` returns 200 (the process is running).
- `GET /readyz` returns 200 (the database is reachable and migrations applied).
- Logs show `database migrations applied` (or already up to date) on startup.
- For Docker, `docker compose ps` shows `postgres` and `redis` healthy before
  `app` starts (compose waits on their healthchecks).

Point your uptime monitor at `/healthz` for liveness and `/readyz` for
readiness; alert if `/readyz` is non-200, which almost always means the database
is unreachable.
