//! Database connection pool management and migrations.

use crate::config::DatabaseConfig;
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Type alias used throughout the app for the connection pool.
pub type Db = PgPool;

/// Build a connection pool from configuration.
pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<Db> {
    use std::str::FromStr;

    // Disable the prepared-statement cache. Admin restore runs
    // `pg_restore --clean`, dropping/recreating every table; a pooled
    // connection with statements cached against the old tables then fails on
    // reuse ("cached plan must not change result type"), causing 500s right
    // after a restore. Re-planning every query avoids that for a tiny cost.
    let connect_opts =
        sqlx::postgres::PgConnectOptions::from_str(&cfg.url)?.statement_cache_capacity(0);

    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .acquire_timeout(cfg.acquire_timeout())
        // Validate on checkout so a connection left bad by a restore is replaced.
        .test_before_acquire(true)
        .connect_with(connect_opts)
        .await?;

    tracing::info!(
        max = cfg.max_connections,
        min = cfg.min_connections,
        "database pool established"
    );

    Ok(pool)
}

/// Run all pending migrations embedded at compile time from `./migrations`.
pub async fn migrate(pool: &Db) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    tracing::info!("database migrations applied");
    Ok(())
}
