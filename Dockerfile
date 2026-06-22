# syntax=docker/dockerfile:1

# ---------- Build stage ----------
# Pin a toolchain new enough for the dependency tree. (The committed Cargo.lock
# also pins transitive crates for older toolchains; on >= 1.85 you may delete
# those pins and re-resolve.)
FROM rust:1.85-slim-bookworm AS builder

# System deps needed to compile some crates (TLS via rustls needs none, but
# pkg-config/openssl headers are handy if you swap to native-tls).
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies: copy manifests first, build only the dependency graph
# with a stub, then remove the stub's OWN compiled artifacts (keeping the cached
# deps) so the real source must be recompiled. Without this cleanup a caching
# hiccup could leave the do-nothing stub binary in place — it exits 0 instantly,
# which under a restart policy becomes an "exited with code 0 (restarting)" loop.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && cargo build --release --quiet || true \
    && rm -rf src \
         target/release/rsmc-engine \
         target/release/deps/rsmc_engine* \
         target/release/.fingerprint/rsmc-engine*

# Now copy the actual sources and migrations and build for real. The size guard
# fails the image build loudly if the stub somehow slipped through, rather than
# shipping a binary that would restart-loop at runtime.
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked \
    && test "$(stat -c%s target/release/rsmc-engine)" -gt 1000000 \
    || (echo "ERROR: built binary too small; stub may have shipped" && exit 1)

# ---------- Runtime stage ----------
FROM debian:bookworm-slim AS runtime

# ca-certificates is required for outbound HTTPS (webhook delivery, etc.).
# The admin backup/restore feature shells out to pg_dump / pg_restore, and
# pg_dump REFUSES to run against a newer server (it aborts on version
# mismatch). The database is PostgreSQL 16, and Debian Bookworm's default
# postgresql-client is 15 — so we add the official PostgreSQL APT repo (PGDG)
# and install the matching postgresql-client-16.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates wget gnupg lsb-release \
    && install -d /usr/share/postgresql-common/pgdg \
    && wget -qO /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc \
        https://www.postgresql.org/media/keys/ACCC4CF8.asc \
    && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-client-16 \
    && apt-get purge -y --auto-remove gnupg lsb-release \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --user-group --no-create-home --uid 10001 appuser

WORKDIR /app

# Binary + migrations (the app runs sqlx migrations at startup when enabled).
COPY --from=builder /app/target/release/rsmc-engine /usr/local/bin/rsmc-engine
COPY --from=builder /app/migrations ./migrations

# Uploads + backups directories (mount volumes here in production).
RUN mkdir -p /app/uploads /app/backups && chown -R appuser:appuser /app

USER appuser

# Note: PORT is bridged into server.port by the config loader; the default is
# 8080 regardless. We deliberately don't set APP__SERVER__PORT here because
# typed (numeric) fields are best provided via the parsed bare `PORT` var.
ENV APP__SERVER__HOST=0.0.0.0 \
    APP__STORAGE__UPLOAD_DIR=/app/uploads \
    APP__STORAGE__BACKUP_DIR=/app/backups \
    PORT=8080 \
    RUST_LOG=info

EXPOSE 8080

# Simple TCP/HTTP healthcheck against the readiness probe.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["/bin/sh", "-c", "wget -qO- http://127.0.0.1:8080/healthz || exit 1"]

ENTRYPOINT ["/usr/local/bin/rsmc-engine"]
