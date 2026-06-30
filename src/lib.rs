//! # rsmc-engine
//!
//! A plug-and-play, production-ready backend engine for team collaboration:
//! authentication, real-time messaging (channels, DMs, threads), message
//! history, file sharing with access control, presence, notifications, and
//! webhooks — built on Axum, SQLx/PostgreSQL, Tokio, and optional Redis pub/sub.
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
use tower_http::trace::TraceLayer;

/// Build the full router: REST + WebSocket under `/api/v1`, health probes at root.
pub fn build_router(state: AppState) -> Router {
    let cors = build_cors(&state);
    let max_body = state.config.server.max_body_bytes;

    let api = Router::new()
        // ---- auth ----
        .route("/auth/signup", post(handlers::auth::signup))
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/refresh", post(handlers::auth::refresh))
        .route("/auth/logout", post(handlers::auth::logout))
        // ---- users ----
        .route(
            "/users/me",
            get(handlers::users::me).patch(handlers::users::update_me),
        )
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
        .route(
            "/channels/:id/read",
            post(handlers::channels::mark_read),
        )
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
            "/messages/:id",
            patch(handlers::messages::edit_message).delete(handlers::messages::delete_message),
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
        // ---- system (admin) ----
        .route("/system/info", get(handlers::system::info))
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
        .route("/ws", get(ws::ws_handler));

    Router::new()
        .route("/healthz", get(handlers::health::healthz))
        .route("/readyz", get(handlers::health::readyz))
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
        // Raise both body limits: RequestBodyLimitLayer caps the raw body,
        // DefaultBodyLimit caps what extractors (incl. Multipart) buffer
        // (its 2 MiB default silently breaks larger uploads).
        .layer(DefaultBodyLimit::max(max_body))
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(cors)
        .with_state(state)
}

/// CORS from configured origins: `*` or empty = any-origin; else explicit allowlist.
fn build_cors(state: &AppState) -> CorsLayer {
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
            .allow_methods(Any)
            .allow_headers(Any)
            .allow_credentials(true)
    }
}

/// Initialize the engine from config: DB (+migrations), JWT keys, hub, event bus,
/// and Redis pub/sub when the `redis-pubsub` feature and a URL are set.
pub async fn bootstrap(settings: config::Settings) -> anyhow::Result<AppState> {
    let pool = db::connect(&settings.database).await?;
    if settings.database.auto_migrate {
        db::migrate(&pool).await?;
        tracing::info!("database migrations applied");
    }

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

    Ok(AppState::new(pool, settings, jwt, hub, events))
}
