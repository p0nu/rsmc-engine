# Running RSMC without Docker

This guide runs the engine from a prebuilt binary (or from source) against a
PostgreSQL you run directly on the machine. No Docker required.

The engine is a single self-contained binary. The only external thing it needs
is a PostgreSQL server to connect to. Migrations are built into the binary and
run automatically on startup.

Everything below assumes Linux (including WSL on Windows). macOS is similar;
substitute Homebrew for the apt commands.

---

## 1. Install and start PostgreSQL

```bash
sudo apt update
sudo apt install -y postgresql postgresql-client
sudo service postgresql start
```

Confirm it is accepting connections:

```bash
pg_isready
```

Note for WSL users: services do not auto-start after a restart. Re-run
`sudo service postgresql start` whenever you start a new WSL session.

---

## 2. Create the database and user

```bash
sudo -u postgres psql <<'SQL'
CREATE ROLE rsmc LOGIN PASSWORD 'rsmc';
CREATE DATABASE rsmc OWNER rsmc;
SQL
```

Verify you can connect as the new user:

```bash
PGPASSWORD=rsmc psql -h localhost -U rsmc -d rsmc -c '\conninfo'
```

If this fails with a "peer authentication failed" error, PostgreSQL is
rejecting password logins over TCP. Open the host-based auth file
(`/etc/postgresql/*/main/pg_hba.conf`), ensure the local TCP lines use `md5`
or `scram-sha-256` (not `peer`), then `sudo service postgresql restart`.

---

## 3. Get the engine binary

Download the latest release for your platform from GitHub and extract it:

```bash
mkdir -p ~/rsmc && cd ~/rsmc

# Linux (static build, runs on any distro):
curl -LO https://github.com/p0nu/rsmc-engine/releases/latest/download/rsmc-engine-VERSION-x86_64-unknown-linux-musl.tar.gz
tar -xzf rsmc-engine-*-x86_64-unknown-linux-musl.tar.gz
cd rsmc-engine-*-x86_64-unknown-linux-musl
```

Replace `VERSION` with the release you want (for example `v0.9.0`), or grab the
URL directly from the GitHub releases page. Confirm the binary and make it
executable:

```bash
file rsmc-engine        # should mention "statically linked" on the musl build
chmod +x rsmc-engine
```

Prefer building from source instead? See the appendix at the end.

---

## 4. Configure

The engine reads a few environment variables. Two are required; the rest have
sensible defaults.

Required:

```bash
# Where the database lives (user:password@host:port/dbname)
export DATABASE_URL="postgres://rsmc:rsmc@localhost:5432/rsmc"

# Secret used to sign login tokens. Generate a strong random one:
export JWT_SECRET="$(openssl rand -hex 32)"
```

Recommended for a real install (where the binary should read/write its data):

```bash
# Uploaded files (avatars, attachments). Defaults to ./uploads
export APP__STORAGE__UPLOAD_DIR="$HOME/rsmc/uploads"

# Admin database backups. Defaults to ./backups
export APP__STORAGE__BACKUP_DIR="$HOME/rsmc/backups"

mkdir -p "$HOME/rsmc/uploads" "$HOME/rsmc/backups"
```

Both storage directories default to paths relative to wherever you start the
binary (`./uploads`, `./backups`). Setting them explicitly to absolute paths is
recommended so backups and uploads always land in a known place regardless of
the working directory.

Optional common settings:

```bash
export APP__SERVER__PORT=8080          # default 8080
export APP__SERVER__HOST=0.0.0.0       # default 0.0.0.0 (all interfaces)
```

---

## 5. Run it

```bash
./rsmc-engine
```

On first start you should see it connect, apply migrations, and begin listening:

```
starting rsmc-engine version="0.9.0"
database pool established
database migrations applied
listening addr=0.0.0.0:8080
```

If it exits immediately, read the message: the engine prints a clear reason for
the common problems (missing DATABASE_URL or JWT_SECRET, a JWT_SECRET that is
too short, or a database it cannot reach). Fix what it names and re-run.

---

## 6. Verify

In another terminal:

```bash
curl localhost:8080/healthz     # -> {"status":"ok"}
curl localhost:8080/readyz      # -> {"status":"ready","online_users":0}
```

Create the first account. The first account created on an empty database
becomes the admin (this is by creation order, not by the name you choose):

```bash
curl -s localhost:8080/api/v1/auth/signup \
  -H 'content-type: application/json' \
  -d '{"email":"you@example.com","username":"you","display_name":"You","password":"a strong passphrase"}'
```

A token and a user with `"role":"admin"` confirm the engine is fully working.

---

## 7. Connect the web UI

The engine serves the API. To use RSMC in a browser you run the UI (a separate
web app) and point it at the engine.

For local use, the UI's dev server proxies API and WebSocket traffic to the
engine automatically:

```bash
git clone https://github.com/p0nu/rsmc-ui.git
cd rsmc-ui
npm install
npm run dev
```

Open the URL it prints (usually http://localhost:5173) and log in with the
account you created. Keep the engine running while you use the UI.

---

## 8. Run as a service (systemd)

For a machine that should keep RSMC running across reboots, install it as a
systemd service. Create `/etc/systemd/system/rsmc-engine.service`:

```ini
[Unit]
Description=RSMC engine
After=network.target postgresql.service
Wants=postgresql.service

[Service]
Type=simple
User=rsmc
WorkingDirectory=/opt/rsmc
ExecStart=/opt/rsmc/rsmc-engine
Restart=on-failure
Environment=DATABASE_URL=postgres://rsmc:rsmc@localhost:5432/rsmc
Environment=JWT_SECRET=REPLACE_WITH_A_LONG_RANDOM_SECRET
Environment=APP__STORAGE__UPLOAD_DIR=/opt/rsmc/uploads
Environment=APP__STORAGE__BACKUP_DIR=/opt/rsmc/backups

[Install]
WantedBy=multi-user.target
```

Put the binary at `/opt/rsmc/rsmc-engine`, create the directories, then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rsmc-engine
sudo systemctl status rsmc-engine
journalctl -u rsmc-engine -f      # follow logs
```

Storing real secrets directly in the unit file is not ideal; for a production
host, put them in a root-owned `EnvironmentFile=` instead and reference it from
the unit.

---

## Troubleshooting

**"migration N was previously applied but has been modified"**
The database applied a different version of a migration than the binary carries.
For a disposable/development database, reset it:
`sudo -u postgres psql -c 'DROP DATABASE rsmc;' && sudo -u postgres psql -c 'CREATE DATABASE rsmc OWNER rsmc;'`
then start the engine again. For a database with real data, do not wipe it; see
`docs/DEPLOY.md`.

**Backup fails / "No such file or directory"**
The backup directory does not exist or is not writable. Set
`APP__STORAGE__BACKUP_DIR` to a path you own and create it (see step 4). Also
ensure `pg_dump` is installed (`which pg_dump`) and its major version is at
least the server's.

**Presence dot stays grey in the UI**
The dot is green only while the browser's WebSocket to the engine is connected.
Grey means the engine or UI is not reachable; confirm the engine is running and
`curl localhost:8080/healthz` responds.

**Engine exits at startup**
Read the printed message. The usual causes are a missing or short `JWT_SECRET`,
a missing `DATABASE_URL`, or a database that is not running/reachable. Each of
these prints a specific, actionable line.

**"peer authentication failed" connecting to PostgreSQL**
See the note at the end of step 2 (adjust `pg_hba.conf` to use password auth).

---

## Appendix: build from source

If you would rather compile the engine yourself (any platform with Rust):

```bash
git clone https://github.com/p0nu/rsmc-engine.git
cd rsmc-engine
cargo build --release
# binary at target/release/rsmc-engine
```

Then configure and run it exactly as in steps 4 to 6. Building from source needs
a recent stable Rust toolchain (install via https://rustup.rs). All migrations
are compiled into the resulting binary.
