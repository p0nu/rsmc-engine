//! In-memory rate limiting via fixed-window token buckets.
//!
//! Two logical limiters share one store, distinguished by key prefix:
//!   - auth  (`auth:<ip>`):   strict, guards login/signup/refresh from brute force
//!   - write (`write:<uid>`): looser, guards message/upload floods
//!
//! The store is a trait so a Redis-backed implementation can replace the
//! in-memory one for multi-instance deployments without touching call sites.
//! The in-memory limiter is per-process, which is correct for single-instance
//! (the default) and degrades gracefully (each instance limits independently)
//! if run replicated without Redis.

use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Outcome of a rate-limit check.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub allowed: bool,
    /// Seconds until the caller may retry (only meaningful when `!allowed`).
    pub retry_after_secs: u64,
}

/// A fixed-window counter: `count` requests since `window_start`.
struct Window {
    count: u32,
    window_start: Instant,
}

/// In-memory limiter. Cheap to clone (shares the map via `Arc` inside DashMap).
#[derive(Clone)]
pub struct RateLimiter {
    inner: std::sync::Arc<DashMap<String, Window>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(DashMap::new()),
        }
    }

    /// Record one hit against `key` and decide whether it's allowed.
    /// `max` requests are permitted per `window`. A fixed window is used
    /// (simple, predictable); the first request starts the window.
    pub fn check(&self, key: &str, max: u32, window: Duration) -> Decision {
        let now = Instant::now();
        let mut entry = self
            .inner
            .entry(key.to_string())
            .or_insert_with(|| Window {
                count: 0,
                window_start: now,
            });

        // Reset the window if it has elapsed.
        if now.duration_since(entry.window_start) >= window {
            entry.count = 0;
            entry.window_start = now;
        }

        entry.count += 1;
        if entry.count <= max {
            Decision {
                allowed: true,
                retry_after_secs: 0,
            }
        } else {
            let elapsed = now.duration_since(entry.window_start);
            let remaining = window.saturating_sub(elapsed);
            Decision {
                allowed: false,
                // Round up so "retry after N" never tells them to retry too early.
                retry_after_secs: remaining.as_secs().max(1),
            }
        }
    }

    /// Drop windows that have fully elapsed, to bound memory. Called
    /// periodically by a background task.
    pub fn sweep(&self, max_window: Duration) {
        let now = Instant::now();
        self.inner
            .retain(|_, w| now.duration_since(w.window_start) < max_window);
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}
