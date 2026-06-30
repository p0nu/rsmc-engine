# Running without Docker

A complete, tested setup using only **Rust** and **PostgreSQL** — no Docker, no
Redis required.

> **Redis is optional.** The engine compiles with Redis support by default, but
> if `REDIS_URL` is not set it runs in single-instance mode using in-process
> event dispatch. You only need Redis to fan out realtime events across multiple
> engine instances behind a load balancer. For a single instance, skip it.

## 1. Install prerequisites

**Rust** (stable toolchain, 1.85+ recommended):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version
```

**PostgreSQL** (16 recommended, to match the backup tooling — see
[CONFIGURATION.md](CONFIGURATION.md)):

```bash
# Debian / Ubuntu
sudo apt update && sudo apt install -y postgresql postgresql-client

# macOS (Homebrew)
brew install postgresql@16 && brew services start postgresql@16

# Fedora
sudo dnf install -y postgresql-server postgresql && sudo postgresql-setup --initdb && sudo systemctl enable --now postgresql
```

Confirm the server is running:

```bash
pg_isready          # should report "accepting connections"
```

## 2. Create the database and role

Run `psql` as the postgres superuser (`sudo -u postgres psql` on most Linux;
`psql postgres` on Homebrew macOS):

```bash
sudo -u postgres psql <<'SQL'
CREATE ROLE rsmc LOGIN PASSWORD 'rsmc';
CREATE DATABASE rsmc OWNER rsmc;
SQL
```

> Use a strong password beyond local development, and update `DATABASE_URL`
> accordingly.

Verify the connection:

```bash
PGPASSWORD=rsmc psql -h localhost -U rsmc -d rsmc -c '\conninfo'
```

If this fails with a peer-authentication error on Linux, ensure `pg_hba.conf`
allows password (`md5`/`scram-sha-256`) auth for TCP connections from
`127.0.0.1`, then reload PostgreSQL.

## 3. Configure the engine

```bash
cp .env.example .env
```

Set at least these two values:

```bash
DATABASE_URL=postgres://rsmc:<your-password>@localhost:5432/rsmc
JWT_SECRET=$(openssl rand -hex 32)
```

Leave `REDIS_URL` commented out for single-instance mode. The `.env` file is
loaded automatically at startup.

> For this manual path you can ignore the `POSTGRES_PASSWORD` field in
> `.env.example` — that one is only used by the Docker stack. Here the password
> lives directly inside `DATABASE_URL`.

## 4. Build and run

```bash
cargo build --release
cargo run --release      # migrations apply automatically on first boot
```

Expected log lines:

```
INFO rsmc_engine::db: database migrations applied
INFO rsmc_engine: redis url not set; running in single-instance mode
INFO rsmc_engine: listening addr=0.0.0.0:8080
```

The API is now live at `http://localhost:8080`.

> To build a smaller binary **without** Redis support compiled in:
> `cargo build --release --no-default-features` (then `REDIS_URL` is ignored).

## 5. Verify it works

```bash
curl localhost:8080/healthz      # -> {"status":"ok"}
curl localhost:8080/readyz       # -> {"status":"ready","online_users":0}

curl -s localhost:8080/api/v1/auth/signup \
  -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","username":"ada","display_name":"Ada","password":"correct horse battery"}'
# -> {"access_token":"...","refresh_token":"...","user":{...,"role":"admin"}}
```

A successful signup returns an `access_token` and a user with `"role":"admin"`.

## 6. Run as a background service (optional)

A minimal **systemd** unit:

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
sudo journalctl -u rsmc-engine -f
```

Put your release binary, `.env`, and (if used) an `uploads/` directory under the
`WorkingDirectory`.
