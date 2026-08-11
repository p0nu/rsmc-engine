//! Prometheus metrics endpoint.
//!
//! Exposes a small set of gauges in the Prometheus text exposition format
//! (version 0.0.4) at `GET /metrics`. Kept dependency-free on purpose: the
//! format is simple and stable, and avoiding the prometheus crate keeps the
//! deliverable Cargo.lock lean. Metrics are sampled on scrape (pull model),
//! which is exactly how Prometheus expects to consume them.
//!
//! The endpoint is unauthenticated by design so a scraper can reach it, but it
//! exposes only aggregate counts (no user data). Operators who want it locked
//! down should restrict it at the reverse proxy, the conventional approach.

use crate::state::AppState;
use axum::{extract::State, http::header, response::IntoResponse};

/// `GET /metrics` - Prometheus text exposition.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let mut out = String::with_capacity(1024);

    // Realtime gauge from the in-memory hub (always available, no DB hit).
    let online = state.events.hub().online_count();
    line(
        &mut out,
        "rsmc_online_users",
        "Number of users with at least one active realtime connection.",
        "gauge",
        online as f64,
    );

    // DB-backed gauges. Each is best-effort: on error we emit -1 so a scrape
    // never fails outright, and the operator can alert on the sentinel.
    let users_total = scalar(&state, "SELECT count(*) FROM users").await;
    line(
        &mut out,
        "rsmc_users_total",
        "Total registered users.",
        "gauge",
        users_total,
    );

    let users_active = scalar(&state, "SELECT count(*) FROM users WHERE is_active").await;
    line(
        &mut out,
        "rsmc_users_active",
        "Users that are currently active (not deactivated).",
        "gauge",
        users_active,
    );

    let channels_total =
        scalar(&state, "SELECT count(*) FROM channels WHERE NOT is_archived").await;
    line(
        &mut out,
        "rsmc_channels_total",
        "Non-archived channels.",
        "gauge",
        channels_total,
    );

    let messages_total = scalar(
        &state,
        "SELECT count(*) FROM messages WHERE deleted_at IS NULL",
    )
    .await;
    line(
        &mut out,
        "rsmc_messages_total",
        "Messages not soft-deleted.",
        "gauge",
        messages_total,
    );

    let messages_24h = scalar(
        &state,
        "SELECT count(*) FROM messages WHERE deleted_at IS NULL AND created_at > now() - interval '24 hours'",
    )
    .await;
    line(
        &mut out,
        "rsmc_messages_last_24h",
        "Messages created in the last 24 hours.",
        "gauge",
        messages_24h,
    );

    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        out,
    )
}

/// Append one metric family (HELP + TYPE + a single sample) to the buffer.
fn line(buf: &mut String, name: &str, help: &str, kind: &str, value: f64) {
    buf.push_str("# HELP ");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(help);
    buf.push('\n');
    buf.push_str("# TYPE ");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(kind);
    buf.push('\n');
    buf.push_str(name);
    buf.push(' ');
    // Integers render without a trailing .0 for readability; Prometheus accepts
    // both, but clean output is nicer for humans reading a scrape.
    if value.fract() == 0.0 {
        buf.push_str(&(value as i64).to_string());
    } else {
        buf.push_str(&value.to_string());
    }
    buf.push('\n');
}

/// Run a scalar count query, returning -1.0 as a sentinel on error so the
/// scrape as a whole still succeeds.
async fn scalar(state: &AppState, sql: &str) -> f64 {
    match sqlx::query_scalar::<_, i64>(sql).fetch_one(&state.db).await {
        Ok(v) => v as f64,
        Err(_) => -1.0,
    }
}
