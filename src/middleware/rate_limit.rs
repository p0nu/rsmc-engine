//! Rate-limiting middleware.
//!
//! Two `from_fn_with_state` middlewares:
//!   - [`auth_limit`]  keys on client IP, applied to `/auth/*`.
//!   - [`write_limit`] keys on authenticated user id, applied to write routes.
//!
//! Both short-circuit with `429 Too Many Requests` (plus a `Retry-After`
//! header) via [`AppError::RateLimited`] when the bucket is exhausted.

use crate::error::AppError;
use crate::state::AppState;
use axum::{
    extract::{ConnectInfo, State},
    http::{header, Request},
    middleware::Next,
    response::Response,
};
use std::net::SocketAddr;
use std::time::Duration;

/// Best-effort client IP: prefer a trusted proxy header, else the socket peer.
/// `X-Forwarded-For`/`X-Real-IP` are only meaningful behind a proxy you control;
/// they're used here just as the rate-limit key, so spoofing them only lets a
/// caller rotate their own key (no privilege escalation).
fn client_ip<B>(req: &Request<B>) -> String {
    if let Some(xff) = req.headers().get("x-forwarded-for") {
        if let Ok(s) = xff.to_str() {
            if let Some(first) = s.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    if let Some(xri) = req.headers().get("x-real-ip") {
        if let Ok(s) = xri.to_str() {
            if !s.trim().is_empty() {
                return s.trim().to_string();
            }
        }
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn too_many(retry_after_secs: u64) -> Response {
    use axum::response::IntoResponse;
    let mut resp = AppError::RateLimited.into_response();
    if let Ok(val) = header::HeaderValue::from_str(&retry_after_secs.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, val);
    }
    resp
}

/// Per-IP limiter for authentication endpoints (brute-force protection).
pub async fn auth_limit(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let cfg = &state.config.rate_limit;
    if !cfg.enabled {
        return next.run(req).await;
    }
    let ip = client_ip(&req);
    let key = format!("auth:{ip}");
    let decision = state.rate_limiter.check(
        &key,
        cfg.auth_max,
        Duration::from_secs(cfg.auth_window_secs),
    );
    if !decision.allowed {
        return too_many(decision.retry_after_secs);
    }
    next.run(req).await
}

/// Per-user limiter for write endpoints (flood protection). It reads the bearer
/// token to key on user id; if the token is absent or invalid it falls back to
/// the client IP, so the limit still applies (and the downstream auth extractor
/// will reject the request properly).
pub async fn write_limit(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let cfg = &state.config.rate_limit;
    if !cfg.enabled {
        return next.run(req).await;
    }

    // Only mutating requests count against the write/flood budget. Read-only
    // methods (GET/HEAD/OPTIONS) are exempt: a single screen load fires many
    // GETs (channels, messages, members, notifications), and throttling those
    // would surface as spurious "could not load" errors during normal use.
    let method = req.method();
    if method == axum::http::Method::GET
        || method == axum::http::Method::HEAD
        || method == axum::http::Method::OPTIONS
    {
        return next.run(req).await;
    }

    // Try to resolve a user id from the bearer token (cheap: JWT verify only).
    let uid = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| {
            h.strip_prefix("Bearer ")
                .or_else(|| h.strip_prefix("bearer "))
        })
        .and_then(|tok| {
            state
                .jwt
                .verify(tok, crate::auth::TokenKind::Access)
                .ok()
                .map(|c| c.sub)
        });

    let key = match uid {
        Some(id) => format!("write:{id}"),
        None => format!("write-ip:{}", client_ip(&req)),
    };

    let decision = state.rate_limiter.check(
        &key,
        cfg.write_max,
        Duration::from_secs(cfg.write_window_secs),
    );
    if !decision.allowed {
        return too_many(decision.retry_after_secs);
    }
    next.run(req).await
}
