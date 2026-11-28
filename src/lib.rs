//! # rsmc-engine
//!
//! A plug-and-play, production-ready backend engine for team collaboration:
//! authentication, real-time messaging (channels, DMs, threads), message
//! history, file sharing with access control, presence, notifications, and
//! webhooks - built on Axum, SQLx/PostgreSQL, Tokio, and optional Redis pub/sub.
//!
//! ## Embedding
//!
//! The crate exposes a single [`build_router`] factory that returns a fully
//! wired [`axum::Router`]. Host applications can mount it directly or nest it
//! under a path. [`bootstrap`] wires up the database, JWT keys, hub, event bus,
//! and (optionally) Redis, returning ready-to-serve state and router.

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod middleware;
pub mod models;
pub mod services;
pub mod state;
pub mod ws;

pub use error::{AppError, AppResult};
pub use state::AppState;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Build the full router: REST + WebSocket under `/api/v1`, health probes at root.
pub fn build_router(state: AppState) -> Router {
    let cors = build_cors(&state);
    let max_body = state.config.server.max_body_bytes;

    // Auth endpoints get a strict per-IP limiter (brute-force protection).
    let auth_routes = Router::new()
        .route("/auth/signup", post(handlers::auth::signup))
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/mfa/verify", post(handlers::auth::mfa_verify))
        .route("/auth/refresh", post(handlers::auth::refresh))
        .route("/auth/logout", post(handlers::auth::logout))
        .route("/auth/verify-email", post(handlers::email::verify_email))
        .route(
            "/auth/password-reset",
            post(handlers::email::request_password_reset),
        )
        .route(
            "/auth/password-reset/confirm",
            post(handlers::email::confirm_password_reset),
        )
        .route(
            "/auth/confirm-email-change",
            post(handlers::email::confirm_email_change),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rate_limit::auth_limit,
        ));

    let api = Router::new()
        .merge(auth_routes)
        // ---- users ----
        .route(
            "/users/me",
            get(handlers::users::me)
                .patch(handlers::users::update_me)
                .delete(handlers::users::delete_my_account),
        )
        .route("/users/me/status", put(handlers::users::set_status))
        .route(
            "/users/me/notification-prefs",
            patch(handlers::users::update_notification_prefs),
        )
        .route("/users/me/sessions", get(handlers::users::list_sessions))
        .route("/users/me/export", get(handlers::users::export_my_data))
        .route(
            "/users/me/mfa/enroll",
            post(handlers::users::mfa_enroll),
        )
        .route(
            "/users/me/mfa/confirm",
            post(handlers::users::mfa_confirm),
        )
        .route(
            "/users/me/mfa/disable",
            post(handlers::users::mfa_disable),
        )
        .route(
            "/users/me/sessions/revoke-all",
            post(handlers::users::revoke_all_sessions),
        )
        .route(
            "/users/me/sessions/:id",
            delete(handlers::users::revoke_session),
        )
        .route(
            "/users/me/verify-email",
            post(handlers::email::send_verification),
        )
        .route(
            "/users/me/email",
            post(handlers::email::request_email_change),
        )
        .route(
            "/users/me/avatar",
            post(handlers::avatars::upload_avatar).delete(handlers::avatars::delete_avatar),
        )
        .route("/users/:id/avatar", get(handlers::avatars::serve_avatar))
        .route("/users", get(handlers::users::list_users))
        .route("/users/:id", get(handlers::users::get_user))
        .route("/users/:id/role", put(handlers::users::set_role))
        .route(
            "/users/:id/deactivate",
            post(handlers::users::deactivate),
        )
        .route(
            "/users/:id/activate",
            post(handlers::users::activate),
        )
        // ---- channels ----
        .route(
            "/channels",
            post(handlers::channels::create_channel).get(handlers::channels::list_channels),
        )
        .route("/channels/direct", post(handlers::channels::create_direct))
        .route("/channels/browse", get(handlers::channels::browse_channels))
        .route(
            "/channels/:id/archive",
            post(handlers::channels::archive_channel),
        )
        .route(
            "/channels/:id/unarchive",
            post(handlers::channels::unarchive_channel),
        )
        .route("/channels/:id/join", post(handlers::channels::join_channel))
        .route(
            "/channels/:id/read",
            post(handlers::channels::mark_read),
        )
        .route(
            "/channels/:id/receipts",
            get(handlers::channels::list_receipts),
        )
        .route("/channels/:id/mute", patch(handlers::channels::set_mute))
        .route(
            "/channels/:id",
            get(handlers::channels::get_channel).patch(handlers::channels::update_channel),
        )
        .route(
            "/channels/:id/members",
            get(handlers::channels::list_members).post(handlers::channels::add_member),
        )
        .route(
            "/channels/:id/members/:user_id",
            delete(handlers::channels::remove_member),
        )
        // ---- messages ----
        .route(
            "/channels/:id/messages",
            post(handlers::messages::send_message).get(handlers::messages::history),
        )
        .route("/messages/:id/thread", get(handlers::messages::thread))
        .route(
            "/messages/:id/pin",
            post(handlers::messages::pin_message).delete(handlers::messages::unpin_message),
        )
        .route("/channels/:id/pins", get(handlers::messages::list_pins))
        .route("/search", get(handlers::messages::search))
        .route(
            "/messages/:id",
            patch(handlers::messages::edit_message).delete(handlers::messages::delete_message),
        )
        // ---- scheduled messages + drafts ----
        .route(
            "/channels/:id/scheduled",
            post(handlers::compose::create_scheduled).get(handlers::compose::list_scheduled),
        )
        .route(
            "/scheduled/:id",
            delete(handlers::compose::cancel_scheduled),
        )
        .route(
            "/channels/:id/draft",
            get(handlers::compose::get_draft).put(handlers::compose::save_draft),
        )
        .route(
            "/messages/:id/reactions",
            post(handlers::messages::add_reaction).delete(handlers::messages::remove_reaction),
        )
        // ---- files ----
        .route("/files", post(handlers::files::upload))
        .route("/files/:id", get(handlers::files::download))
        // ---- notifications ----
        .route("/notifications", get(handlers::notifications::list))
        .route(
            "/notifications/unread_count",
            get(handlers::notifications::unread_count),
        )
        .route(
            "/notifications/:id/read",
            post(handlers::notifications::mark_read),
        )
        .route(
            "/notifications/read_all",
            post(handlers::notifications::mark_all_read),
        )
        // ---- presence ----
        .route(
            "/presence/:user_id",
            get(handlers::presence::get_presence),
        )
        .route("/presence/bulk", post(handlers::presence::bulk_presence))
        // ---- webhooks ----
        .route(
            "/webhooks",
            post(handlers::webhooks::create_webhook).get(handlers::webhooks::list_webhooks),
        )
        .route(
            "/webhooks/:id",
            delete(handlers::webhooks::delete_webhook),
        )
        // ---- instance settings (GET any user, PUT admin only) ----
        .route(
            "/settings",
            get(handlers::settings::get_settings).put(handlers::settings::update_settings),
        )
        // ---- system (admin) ----
        .route("/system/info", get(handlers::system::info))
        .route("/system/stats", get(handlers::system::stats))
        .route("/system/audit", get(handlers::system::audit_log))
        .route("/system/backup", post(handlers::system::backup))
        .route("/system/backups", get(handlers::system::list_backups))
        .route("/system/restore", post(handlers::system::restore))
        // ---- app links (workspace bookmarks) ----
        .route(
            "/app-links",
            get(handlers::app_links::list_app_links).post(handlers::app_links::create_app_link),
        )
        .route(
            "/app-links/:id",
            axum::routing::delete(handlers::app_links::delete_app_link),
        )
        // ---- websocket ----
        .route("/ws", get(ws::ws_handler))
        // Per-user write/flood limiter across the API (keyed by token subject,
        // falls back to IP when unauthenticated). Generous by default.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::rate_limit::write_limit,
        ));

    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::readyz))
        .route("/metrics", get(handlers::metrics::metrics))
        .nest("/api/v1", api)
        // Cap how long a single HTTP request may take, so a slow or stuck client
        // cannot hold a handler open indefinitely (slowloris-style resource
        // exhaustion). This times out the HTTP request/response only; a
        // WebSocket upgrade returns quickly and the upgraded, long-lived socket
        // lives outside this layer, so realtime is unaffected. The window is
        // generous enough for large uploads over slow links.
        .layer(TimeoutLayer::new(std::time::Duration::from_secs(120)))
        .layer(TraceLayer::new_for_http())
        // Raise both body limits: RequestBodyLimitLayer caps the raw body,
        // DefaultBodyLimit caps what extractors (incl. Multipart) buffer
        // (its 2 MiB default silently breaks larger uploads).
        .layer(DefaultBodyLimit::max(max_body))
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(cors)
        .with_state(state)
}

/// CORS from configured origins: `*` or empty = any-origin (no credentials);
/// else an explicit allowlist that permits credentials.
///
/// The two branches are not symmetric, and deliberately so. The CORS spec
/// forbids combining `Access-Control-Allow-Credentials: true` with a wildcard
/// (`Any`) for origin OR for headers: a credentialed response must name its
/// allowed origins and headers explicitly. So the wildcard branch does NOT set
/// credentials, and the allowlist branch names concrete methods and headers
/// instead of `Any`. Sending `Any` headers together with credentials makes
/// `tower-http` panic at startup, which is how a production deployment that set
/// a real origin would otherwise crash.
fn build_cors(state: &AppState) -> CorsLayer {
    use axum::http::{header, Method};

    let origins = &state.config.server.cors_origins;
    if origins.is_empty() || origins.iter().any(|o| o == "*") {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let parsed: Vec<_> = origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(parsed)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .allow_credentials(true)
    }
}

/// Initialize the engine from config: DB (+migrations), JWT keys, hub, event bus,
/// and Redis pub/sub when the `redis-pubsub` feature and a URL are set.
pub async fn bootstrap(settings: config::Settings) -> anyhow::Result<AppState> {
    handlers::system::mark_process_start();
    let pool = db::connect(&settings.database).await?;
    if settings.database.auto_migrate {
        db::migrate(&pool).await?;
        tracing::info!("database migrations applied");
    }

    // Log the effective, resolved configuration once at startup. This makes
    // first-run behavior transparent: where uploads/backups are written, whether
    // rate limiting is on, and whether the process is single- or multi-instance.
    // It's the first thing to check when something like a backup "goes missing".
    tracing::info!(
        upload_dir = %settings.storage.upload_dir,
        backup_dir = %settings.storage.backup_dir,
        rate_limiting = settings.rate_limit.enabled,
        auto_migrate = settings.database.auto_migrate,
        "effective configuration"
    );

    let jwt = auth::JwtKeys::new(
        &settings.auth.jwt_secret,
        settings.auth.access_token_ttl_secs,
        settings.auth.refresh_token_ttl_secs,
    );

    let hub = ws::Hub::new();

    #[cfg(feature = "redis-pubsub")]
    let events = {
        match &settings.redis.url {
            Some(url) => {
                let (pubsub, client) = services::pubsub::PubSub::connect(url).await?;
                let bus = services::events::EventBus::new(hub.clone(), pool.clone(), Some(pubsub));
                services::pubsub::spawn_subscriber(client, bus.clone());
                tracing::info!("redis pub/sub enabled for multi-instance fan-out");
                bus
            }
            None => {
                tracing::info!("redis url not set; running in single-instance mode");
                services::events::EventBus::new(hub.clone(), pool.clone(), None)
            }
        }
    };

    #[cfg(not(feature = "redis-pubsub"))]
    let events = services::events::EventBus::new(hub.clone(), pool.clone());

    let state = AppState::new(pool, settings, jwt, hub, events);

    // Start the background scheduler that delivers due scheduled messages.
    services::scheduler::spawn(state.clone());

    // Periodically evict stale rate-limit windows to bound memory.
    {
        let limiter = state.rate_limiter.clone();
        let max_window = std::time::Duration::from_secs(
            state
                .config
                .rate_limit
                .auth_window_secs
                .max(state.config.rate_limit.write_window_secs)
                .max(1)
                * 2,
        );
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                limiter.sweep(max_window);
            }
        });
    }

    Ok(state)
}
