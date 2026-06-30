//! End-to-end integration tests that exercise the real Axum router in-process
//! against a live PostgreSQL database.
//!
//! These tests require a throwaway Postgres instance. They are skipped
//! automatically (returning early) when `TEST_DATABASE_URL` is not set, so the
//! default `cargo test` run stays green in environments without a database.
//!
//! To run them:
//!
//! ```bash
//! # spin up a disposable database
//! docker run --rm -d --name rsmc-test-db -p 5433:5432 \
//!     -e POSTGRES_USER=rsmc -e POSTGRES_PASSWORD=rsmc \
//!     -e POSTGRES_DB=rsmc_test postgres:16-alpine
//!
//! export TEST_DATABASE_URL=postgres://rsmc:rsmc@localhost:5433/rsmc_test
//! cargo test --test integration
//! ```
//!
//! Each test runs against a freshly-migrated, isolated schema so they don't
//! interfere with one another.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rsmc_engine::config::{
    AuthConfig, DatabaseConfig, LoggingConfig, RedisConfig, ServerConfig, Settings, StorageConfig,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

/// Build a `Settings` pointed at an isolated, freshly-created schema on the test
/// database. Returns `None` when no test database is configured.
async fn test_settings() -> Option<(Settings, sqlx::PgPool)> {
    let base_url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;

    // Create a unique schema for this test so tests are mutually isolated.
    let schema = format!("test_{}", Uuid::new_v4().simple());
    let admin_pool = sqlx::PgPool::connect(&base_url)
        .await
        .expect("connect to test database");
    sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
        .execute(&admin_pool)
        .await
        .expect("create schema");

    // Force every connection in the engine's pool onto this schema.
    let sep = if base_url.contains('?') { '&' } else { '?' };
    let scoped_url = format!("{base_url}{sep}options=-c%20search_path%3D{schema}");

    let settings = Settings {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            cors_origins: vec!["*".into()],
            max_body_bytes: 25 * 1024 * 1024,
        },
        database: DatabaseConfig {
            url: scoped_url,
            max_connections: 5,
            min_connections: 1,
            acquire_timeout_secs: 10,
            auto_migrate: true,
        },
        auth: AuthConfig {
            jwt_secret: "integration-test-secret-key-please".into(),
            access_token_ttl_secs: 3600,
            refresh_token_ttl_secs: 2_592_000,
        },
        storage: StorageConfig {
            upload_dir: std::env::temp_dir()
                .join(format!("rsmc-test-{schema}"))
                .to_string_lossy()
                .into_owned(),
            max_file_bytes: 25 * 1024 * 1024,
        },
        redis: RedisConfig { url: None },
        logging: LoggingConfig {
            format: "text".into(),
            level: "warn".into(),
        },
    };

    Some((settings, admin_pool))
}

/// Convenience: send a JSON request through the router and return (status, body).
async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn health_probe_is_open() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (status, body) = send(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn signup_login_and_first_user_is_admin() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    // First signup → becomes admin.
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": "alice@example.com",
            "username": "alice",
            "display_name": "Alice",
            "password": "supersecret"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {body}");
    assert_eq!(body["user"]["role"], "admin");
    assert!(body["access_token"].as_str().is_some());

    // Login with the same credentials.
    let (status, login) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(json!({ "email": "alice@example.com", "password": "supersecret" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed: {login}");
    let token = login["access_token"].as_str().unwrap().to_string();

    // Authenticated /users/me reflects the identity.
    let (status, me) = send(&app, "GET", "/api/v1/users/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["username"], "alice");

    // Wrong password is rejected.
    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        Some(json!({ "email": "alice@example.com", "password": "wrongpass" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn second_user_is_member_not_admin() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    for (i, (email, user)) in [
        ("first@example.com", "firstuser"),
        ("second@example.com", "seconduser"),
    ]
    .iter()
    .enumerate()
    {
        let (status, body) = send(
            &app,
            "POST",
            "/api/v1/auth/signup",
            None,
            Some(json!({
                "email": email,
                "username": user,
                "display_name": user,
                "password": "supersecret"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "signup {i} failed: {body}");
        let expected_role = if i == 0 { "admin" } else { "member" };
        assert_eq!(body["user"]["role"], expected_role);
    }
}

#[tokio::test]
async fn channel_create_send_and_history() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    // Register a user and grab their token.
    let (_, signup) = send(
        &app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": "owner@example.com",
            "username": "owner",
            "display_name": "Owner",
            "password": "supersecret"
        })),
    )
    .await;
    let token = signup["access_token"].as_str().unwrap().to_string();

    // Create a public channel. (Signup already auto-creates the "general"
    // default channel for the first user, so use a distinct name here.)
    let (status, channel) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(&token),
        Some(json!({ "name": "engineering", "topic": "watercooler", "private": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create channel failed: {channel}");
    let channel_id = channel["id"].as_str().unwrap().to_string();

    // Send a message.
    let (status, msg) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        Some(json!({ "content": "hello world" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send message failed: {msg}");
    assert_eq!(msg["content"], "hello world");

    // Read history back.
    let (status, history) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let messages = history["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], "hello world");
    assert_eq!(messages[0]["author"]["username"], "owner");
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (status, _) = send(&app, "GET", "/api/v1/users/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(&app, "GET", "/api/v1/channels", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Regression test for the v0.2.0 unread-badge bugs: a single DM must produce a
/// per-channel unread_count of exactly 1 (never 2), exactly one unread
/// notification, and `POST /channels/:id/read` must clear BOTH surfaces so the
/// counts can't get stuck out of step.
#[tokio::test]
async fn dm_unread_count_is_single_and_mark_read_clears_it() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    // Two users: alice (first → admin) and bob.
    let mut tokens = Vec::new();
    let mut ids = Vec::new();
    for (email, user) in [("alice@example.com", "alice"), ("bob@example.com", "bob")] {
        let (status, body) = send(
            &app,
            "POST",
            "/api/v1/auth/signup",
            None,
            Some(json!({
                "email": email,
                "username": user,
                "display_name": user,
                "password": "supersecret"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "signup failed: {body}");
        tokens.push(body["access_token"].as_str().unwrap().to_string());
        ids.push(body["user"]["id"].as_str().unwrap().to_string());
    }
    let (alice_token, bob_token) = (&tokens[0], &tokens[1]);
    let bob_id = &ids[1];

    // Alice opens a DM with Bob and sends ONE message.
    let (status, dm) = send(
        &app,
        "POST",
        "/api/v1/channels/direct",
        Some(alice_token),
        Some(json!({ "user_id": bob_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create direct failed: {dm}");
    let dm_id = dm["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{dm_id}/messages"),
        Some(alice_token),
        Some(json!({ "content": "Hello" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // From Bob's side: exactly ONE unread on the DM channel (the reported bug
    // produced 2), and exactly one unread notification.
    let (status, channels) = send(&app, "GET", "/api/v1/channels", Some(bob_token), None).await;
    assert_eq!(status, StatusCode::OK);
    let dm_row = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == dm_id.as_str())
        .expect("bob should see the DM channel");
    assert_eq!(
        dm_row["unread_count"], 1,
        "a single DM must count as exactly 1 unread, got: {}",
        dm_row["unread_count"]
    );

    let (status, count) = send(
        &app,
        "GET",
        "/api/v1/notifications/unread_count",
        Some(bob_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count["unread"], 1, "exactly one DM notification expected");

    // Bob marks the channel read. This must clear BOTH the per-channel badge and
    // the notification count in one shot (the fix for the "stuck at 1" bug).
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{dm_id}/read"),
        Some(bob_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, channels) = send(&app, "GET", "/api/v1/channels", Some(bob_token), None).await;
    let dm_row = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == dm_id.as_str())
        .unwrap();
    assert_eq!(dm_row["unread_count"], 0, "channel unread should be cleared");

    let (_, count) = send(
        &app,
        "GET",
        "/api/v1/notifications/unread_count",
        Some(bob_token),
        None,
    )
    .await;
    assert_eq!(count["unread"], 0, "notification count should be cleared too");

    // Sending another DM after read advances the badge back to 1, proving the
    // read cursor moved (not just a one-time zeroing).
    let (_, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{dm_id}/messages"),
        Some(alice_token),
        Some(json!({ "content": "second" })),
    )
    .await;
    let (_, channels) = send(&app, "GET", "/api/v1/channels", Some(bob_token), None).await;
    let dm_row = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == dm_id.as_str())
        .unwrap();
    assert_eq!(dm_row["unread_count"], 1, "new message after read = 1 unread");
}

/// Regression test for the thread reply-count bug: posting ONE reply must bump
/// the parent's reply_count to exactly 1 (the UI was showing 4 for a single
/// reply). Verifies the server is authoritative and increments once per reply.
#[tokio::test]
async fn thread_reply_count_increments_once_per_reply() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (_, signup) = send(
        &app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": "threader@example.com",
            "username": "threader",
            "display_name": "Threader",
            "password": "supersecret"
        })),
    )
    .await;
    let token = signup["access_token"].as_str().unwrap().to_string();

    let (_, channel) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(&token),
        Some(json!({ "name": "threads", "topic": null, "private": false })),
    )
    .await;
    let channel_id = channel["id"].as_str().unwrap().to_string();

    // Root message.
    let (_, root) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        Some(json!({ "content": "root" })),
    )
    .await;
    let root_id = root["id"].as_str().unwrap().to_string();
    assert_eq!(root["reply_count"], 0, "fresh root has zero replies");

    // Post exactly ONE reply.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        Some(json!({ "content": "first reply", "parent_id": root_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Root's reply_count must be exactly 1 (history reflects the server value).
    let (_, history) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        None,
    )
    .await;
    let root_view = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == root_id.as_str())
        .expect("root in history");
    assert_eq!(
        root_view["reply_count"], 1,
        "one reply must yield reply_count == 1, got {}",
        root_view["reply_count"]
    );

    // The thread endpoint returns exactly one reply.
    let (_, replies) = send(
        &app,
        "GET",
        &format!("/api/v1/messages/{root_id}/thread"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(replies.as_array().unwrap().len(), 1, "exactly one reply stored");

    // A second reply makes it 2 — proving linear, once-per-reply counting.
    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        Some(json!({ "content": "second reply", "parent_id": root_id })),
    )
    .await;
    let (_, history2) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{channel_id}/messages"),
        Some(&token),
        None,
    )
    .await;
    let root_view2 = history2["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == root_id.as_str())
        .unwrap();
    assert_eq!(root_view2["reply_count"], 2, "two replies => reply_count == 2");
}
