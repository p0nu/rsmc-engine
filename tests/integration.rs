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
    AuthConfig, DatabaseConfig, LoggingConfig, RateLimitConfig, RedisConfig, ServerConfig,
    Settings, StorageConfig,
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
        // Disabled by default so the many requests other tests make aren't
        // throttled (they all share the "unknown" IP bucket under oneshot).
        // The dedicated rate-limit test enables it explicitly.
        rate_limit: RateLimitConfig {
            enabled: false,
            auth_max: 10,
            auth_window_secs: 60,
            write_max: 30,
            write_window_secs: 10,
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

    // A second reply makes it 2 - proving linear, once-per-reply counting.
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

/// Read receipts: after one user reads a channel, `GET /channels/:id/receipts`
/// reports their advanced cursor, and it sits at/after a message they've seen
/// (so the client can render "Seen" / "Seen by N").
#[tokio::test]
async fn read_receipts_report_member_cursors() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let mut tokens = Vec::new();
    let mut ids = Vec::new();
    for (email, user) in [("r1@example.com", "reader1"), ("r2@example.com", "reader2")] {
        let (_, body) = send(
            &app,
            "POST",
            "/api/v1/auth/signup",
            None,
            Some(json!({
                "email": email, "username": user,
                "display_name": user, "password": "supersecret"
            })),
        )
        .await;
        tokens.push(body["access_token"].as_str().unwrap().to_string());
        ids.push(body["user"]["id"].as_str().unwrap().to_string());
    }
    let (a_token, b_token) = (&tokens[0], &tokens[1]);
    let b_id = &ids[1];

    // Alice opens a DM with Bob and sends a message.
    let (_, dm) = send(
        &app,
        "POST",
        "/api/v1/channels/direct",
        Some(a_token),
        Some(json!({ "user_id": b_id })),
    )
    .await;
    let dm_id = dm["id"].as_str().unwrap().to_string();
    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{dm_id}/messages"),
        Some(a_token),
        Some(json!({ "content": "seen test" })),
    )
    .await;

    // Before Bob reads: no receipt for Bob (he has no cursor yet).
    let (status, receipts) =
        send(&app, "GET", &format!("/api/v1/channels/{dm_id}/receipts"), Some(a_token), None).await;
    assert_eq!(status, StatusCode::OK);
    let bob_before = receipts
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user_id"] == b_id.as_str());
    assert!(bob_before.is_none(), "bob has no read cursor before reading");

    // Bob reads the DM.
    let (status, _) =
        send(&app, "POST", &format!("/api/v1/channels/{dm_id}/read"), Some(b_token), None).await;
    assert_eq!(status, StatusCode::OK);

    // Now Bob's receipt exists and carries a timestamp.
    let (_, receipts) =
        send(&app, "GET", &format!("/api/v1/channels/{dm_id}/receipts"), Some(a_token), None).await;
    let bob_after = receipts
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user_id"] == b_id.as_str())
        .expect("bob should have a read receipt after reading");
    assert!(
        bob_after["last_read_at"].is_string(),
        "receipt carries a last_read_at timestamp"
    );

    // A non-member cannot read receipts.
    let (_, outsider) = send(
        &app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": "out@example.com", "username": "outsider",
            "display_name": "Outsider", "password": "supersecret"
        })),
    )
    .await;
    let out_token = outsider["access_token"].as_str().unwrap();
    let (status, _) =
        send(&app, "GET", &format!("/api/v1/channels/{dm_id}/receipts"), Some(out_token), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "non-members cannot see read receipts"
    );
}

/// Message search finds matching messages across a member's channels and
/// scopes correctly to a single channel when asked.
#[tokio::test]
async fn message_search_finds_and_scopes() {
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
            "email": "searcher@example.com", "username": "searcher",
            "display_name": "Searcher", "password": "supersecret"
        })),
    )
    .await;
    let token = signup["access_token"].as_str().unwrap().to_string();

    let (_, ch) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(&token),
        Some(json!({ "name": "searchch", "private": false })),
    )
    .await;
    let ch_id = ch["id"].as_str().unwrap().to_string();

    for body in ["the quick brown fox", "lazy dog sleeps", "quick reminder"] {
        send(
            &app,
            "POST",
            &format!("/api/v1/channels/{ch_id}/messages"),
            Some(&token),
            Some(json!({ "content": body })),
        )
        .await;
    }

    // Search for "quick" -> two hits.
    let (status, res) =
        send(&app, "GET", "/api/v1/search?q=quick", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let hits = res["results"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "two messages contain 'quick'");

    // Channel-scoped search returns the same here.
    let (_, scoped) = send(
        &app,
        "GET",
        &format!("/api/v1/search?q=quick&channel_id={ch_id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(scoped["results"].as_array().unwrap().len(), 2);

    // A term nobody wrote -> no hits.
    let (_, none) =
        send(&app, "GET", "/api/v1/search?q=zzzznomatch", Some(&token), None).await;
    assert_eq!(none["results"].as_array().unwrap().len(), 0);

    // Empty query -> empty results, not an error.
    let (status, empty) = send(&app, "GET", "/api/v1/search?q=", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["results"].as_array().unwrap().len(), 0);
}

/// Pinning a message surfaces it in the channel's pins list; unpinning removes it.
#[tokio::test]
async fn pin_and_unpin_message() {
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
            "email": "pinner@example.com", "username": "pinner",
            "display_name": "Pinner", "password": "supersecret"
        })),
    )
    .await;
    let token = signup["access_token"].as_str().unwrap().to_string();

    let (_, ch) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(&token),
        Some(json!({ "name": "pinch", "private": false })),
    )
    .await;
    let ch_id = ch["id"].as_str().unwrap().to_string();

    let (_, msg) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{ch_id}/messages"),
        Some(&token),
        Some(json!({ "content": "pin me" })),
    )
    .await;
    let msg_id = msg["id"].as_str().unwrap().to_string();

    // No pins initially.
    let (_, pins) =
        send(&app, "GET", &format!("/api/v1/channels/{ch_id}/pins"), Some(&token), None).await;
    assert_eq!(pins["pins"].as_array().unwrap().len(), 0);

    // Pin it.
    let (status, pinned) =
        send(&app, "POST", &format!("/api/v1/messages/{msg_id}/pin"), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(pinned["pinned_at"].is_string(), "pinned_at is set after pinning");

    let (_, pins) =
        send(&app, "GET", &format!("/api/v1/channels/{ch_id}/pins"), Some(&token), None).await;
    let arr = pins["pins"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "one pinned message");
    assert_eq!(arr[0]["id"], msg_id.as_str());

    // The pin must also survive a normal history reload (regression: history
    // omitted the pin columns, so pins vanished on refresh).
    let (_, hist) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{ch_id}/messages"),
        Some(&token),
        None,
    )
    .await;
    let in_history = hist["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == msg_id.as_str())
        .expect("message in history");
    assert!(
        in_history["pinned_at"].is_string(),
        "history must report pinned_at for a pinned message"
    );

    // Unpin.
    let (status, _) =
        send(&app, "DELETE", &format!("/api/v1/messages/{msg_id}/pin"), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);

    let (_, pins) =
        send(&app, "GET", &format!("/api/v1/channels/{ch_id}/pins"), Some(&token), None).await;
    assert_eq!(pins["pins"].as_array().unwrap().len(), 0, "no pins after unpin");
}

/// An @channel broadcast mention notifies every other member of the channel.
#[tokio::test]
async fn broadcast_mention_notifies_all_members() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    // Three users: owner + two members.
    let mut tokens = Vec::new();
    let mut ids = Vec::new();
    for (email, user) in [
        ("owner@example.com", "owner"),
        ("mem1@example.com", "memberone"),
        ("mem2@example.com", "membertwo"),
    ] {
        let (_, b) = send(
            &app,
            "POST",
            "/api/v1/auth/signup",
            None,
            Some(json!({
                "email": email, "username": user,
                "display_name": user, "password": "supersecret"
            })),
        )
        .await;
        tokens.push(b["access_token"].as_str().unwrap().to_string());
        ids.push(b["user"]["id"].as_str().unwrap().to_string());
    }
    let owner_token = &tokens[0];

    let (_, ch) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(owner_token),
        Some(json!({ "name": "announce", "private": false })),
    )
    .await;
    let ch_id = ch["id"].as_str().unwrap().to_string();

    // Add the two members.
    for uid in [&ids[1], &ids[2]] {
        let (status, _) = send(
            &app,
            "POST",
            &format!("/api/v1/channels/{ch_id}/members"),
            Some(owner_token),
            Some(json!({ "user_id": uid })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // Owner posts an @channel broadcast.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{ch_id}/messages"),
        Some(owner_token),
        Some(json!({ "content": "@channel standup in 5" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Both members should each have a broadcast Mention notification (they also
    // have a channel_invite from being added, so filter by kind).
    for mtok in [&tokens[1], &tokens[2]] {
        let (_, notifs) = send(
            &app,
            "GET",
            "/api/v1/notifications?unread_only=true&limit=50",
            Some(mtok),
            None,
        )
        .await;
        let mentions = notifs
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| {
                n["kind"] == "mention" && n["payload"]["broadcast"] == serde_json::Value::Bool(true)
            })
            .count();
        assert_eq!(mentions, 1, "each member gets exactly one broadcast mention");
    }

    // The author does not notify themselves.
    let (_, owner_notifs) = send(
        &app,
        "GET",
        "/api/v1/notifications?unread_only=true&limit=50",
        Some(owner_token),
        None,
    )
    .await;
    let owner_mentions = owner_notifs
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "mention")
        .count();
    assert_eq!(owner_mentions, 0, "author is not self-notified");
}

/// Muting a channel suppresses notifications (including @channel broadcasts)
/// for that member, while unread counts keep accruing.
#[tokio::test]
async fn muting_channel_suppresses_notifications_but_keeps_unread() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let mut tokens = Vec::new();
    let mut ids = Vec::new();
    for (email, user) in [("mowner@example.com", "mowner"), ("mmember@example.com", "mmember")] {
        let (_, b) = send(
            &app,
            "POST",
            "/api/v1/auth/signup",
            None,
            Some(json!({
                "email": email, "username": user,
                "display_name": user, "password": "supersecret"
            })),
        )
        .await;
        tokens.push(b["access_token"].as_str().unwrap().to_string());
        ids.push(b["user"]["id"].as_str().unwrap().to_string());
    }
    let (owner_token, member_token) = (&tokens[0], &tokens[1]);
    let member_id = &ids[1];

    let (_, ch) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(owner_token),
        Some(json!({ "name": "noisy", "private": false })),
    )
    .await;
    let ch_id = ch["id"].as_str().unwrap().to_string();
    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{ch_id}/members"),
        Some(owner_token),
        Some(json!({ "user_id": member_id })),
    )
    .await;

    // Member mutes the channel.
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/v1/channels/{ch_id}/mute"),
        Some(member_token),
        Some(json!({ "muted": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mute failed: {body}");
    assert_eq!(body["muted"], true);

    // The channel list reports the muted flag for that member.
    let (_, channels) = send(&app, "GET", "/api/v1/channels", Some(member_token), None).await;
    let row = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ch_id.as_str())
        .expect("member sees the channel");
    assert_eq!(row["muted"], true, "channel list reports muted");

    // Count the member's existing notifications (the channel_invite from being added).
    let (_, before) = send(
        &app,
        "GET",
        "/api/v1/notifications?unread_only=true&limit=50",
        Some(member_token),
        None,
    )
    .await;
    let before_count = before.as_array().unwrap().len();

    // Owner posts a plain message AND an @channel broadcast.
    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{ch_id}/messages"),
        Some(owner_token),
        Some(json!({ "content": "regular message" })),
    )
    .await;
    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{ch_id}/messages"),
        Some(owner_token),
        Some(json!({ "content": "@channel everyone look" })),
    )
    .await;

    // No new notifications for the muted member.
    let (_, after) = send(
        &app,
        "GET",
        "/api/v1/notifications?unread_only=true&limit=50",
        Some(member_token),
        None,
    )
    .await;
    assert_eq!(
        after.as_array().unwrap().len(),
        before_count,
        "muted member must receive no new notifications, not even @channel"
    );

    // But unread still accrues so the sidebar can show quiet activity.
    let (_, channels) = send(&app, "GET", "/api/v1/channels", Some(member_token), None).await;
    let row = channels
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == ch_id.as_str())
        .unwrap();
    assert_eq!(row["unread_count"], 2, "unread keeps counting while muted");

    // Unmuting restores notifications.
    let (_, body) = send(
        &app,
        "PATCH",
        &format!("/api/v1/channels/{ch_id}/mute"),
        Some(member_token),
        Some(json!({ "muted": false })),
    )
    .await;
    assert_eq!(body["muted"], false);

    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{ch_id}/messages"),
        Some(owner_token),
        Some(json!({ "content": "@channel back on" })),
    )
    .await;
    let (_, unmuted) = send(
        &app,
        "GET",
        "/api/v1/notifications?unread_only=true&limit=50",
        Some(member_token),
        None,
    )
    .await;
    assert!(
        unmuted.as_array().unwrap().len() > before_count,
        "unmuted member receives notifications again"
    );
}

/// The admin overview endpoint reports live workspace figures and is admin-only.
#[tokio::test]
async fn system_stats_reports_workspace_and_is_admin_only() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (_, admin) = send(
        &app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": "sadmin@example.com", "username": "sadmin",
            "display_name": "SAdmin", "password": "supersecret"
        })),
    )
    .await;
    let admin_token = admin["access_token"].as_str().unwrap().to_string();

    let (_, plain) = send(
        &app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": "splain@example.com", "username": "splain",
            "display_name": "SPlain", "password": "supersecret"
        })),
    )
    .await;
    let plain_token = plain["access_token"].as_str().unwrap();

    // Non-admins are refused.
    let (status, _) = send(&app, "GET", "/api/v1/system/stats", Some(plain_token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "system stats must be admin-only");

    // Create content so the counts are non-trivial.
    let (_, ch) = send(
        &app,
        "POST",
        "/api/v1/channels",
        Some(&admin_token),
        Some(json!({ "name": "statschan", "private": false })),
    )
    .await;
    let ch_id = ch["id"].as_str().unwrap().to_string();
    for body in ["one", "two"] {
        send(
            &app,
            "POST",
            &format!("/api/v1/channels/{ch_id}/messages"),
            Some(&admin_token),
            Some(json!({ "content": body })),
        )
        .await;
    }

    let (status, stats) =
        send(&app, "GET", "/api/v1/system/stats", Some(&admin_token), None).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(stats["workspace"]["users_total"], 2);
    assert_eq!(stats["workspace"]["users_active"], 2);
    assert_eq!(stats["workspace"]["messages_total"], 2);
    assert_eq!(stats["workspace"]["messages_24h"], 2);
    assert!(stats["workspace"]["channels_public"].as_i64().unwrap() >= 1);

    assert!(stats["server"]["version"].is_string());
    assert!(stats["database"]["size_bytes"].as_i64().unwrap() > 0);
    assert!(stats["realtime"]["online_users"].is_number());
}

// ===========================================================================
// 0.7.0 feature tests
// ===========================================================================

/// Register a user and return (token, user_id).
async fn signup(app: &axum::Router, email: &str, username: &str) -> (String, String) {
    let (status, body) = send(
        app,
        "POST",
        "/api/v1/auth/signup",
        None,
        Some(json!({
            "email": email,
            "username": username,
            "display_name": username,
            "password": "supersecret"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {body}");
    (
        body["access_token"].as_str().unwrap().to_string(),
        body["user"]["id"].as_str().unwrap().to_string(),
    )
}

/// Create a channel and return its id.
async fn make_channel(app: &axum::Router, token: &str, name: &str, private: bool) -> String {
    let (status, ch) = send(
        app,
        "POST",
        "/api/v1/channels",
        Some(token),
        Some(json!({ "name": name, "private": private })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create channel failed: {ch}");
    ch["id"].as_str().unwrap().to_string()
}

// ---- Feature 1: channel archival -----------------------------------------

#[tokio::test]
async fn archive_makes_channel_readonly_and_hides_it() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (token, _uid) = signup(&app, "arch@example.com", "archer").await;
    let cid = make_channel(&app, &token, "archive-me", false).await;

    // Archive it (owner is channel-admin).
    let (status, body) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/archive"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "archive failed: {body}");
    assert!(body["archived_at"].is_string(), "archived_at should be set");

    // Posting is rejected.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/messages"),
        Some(&token),
        Some(json!({ "content": "should fail" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "posting to archived channel must fail");

    // Hidden from default list, present with ?include_archived=true.
    let (_, list) = send(&app, "GET", "/api/v1/channels", Some(&token), None).await;
    let has = list.as_array().unwrap().iter().any(|c| c["id"] == json!(cid));
    assert!(!has, "archived channel should be hidden by default");

    let (_, list2) = send(
        &app,
        "GET",
        "/api/v1/channels?include_archived=true",
        Some(&token),
        None,
    )
    .await;
    let has2 = list2.as_array().unwrap().iter().any(|c| c["id"] == json!(cid));
    assert!(has2, "archived channel should appear with include_archived=true");

    // Unarchive restores posting.
    let (status, body) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/unarchive"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["archived_at"].is_null(), "archived_at should be cleared");

    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/messages"),
        Some(&token),
        Some(json!({ "content": "now works" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "posting after unarchive should work");
}

// ---- Feature 2: self-service join + browse --------------------------------

#[tokio::test]
async fn browse_and_join_public_channel() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (owner, _o) = signup(&app, "owner2@example.com", "owner2").await;
    let (joiner, joiner_id) = signup(&app, "joiner@example.com", "joiner").await;

    let public_id = make_channel(&app, &owner, "public-room", false).await;
    let private_id = make_channel(&app, &owner, "private-room", true).await;

    // Browse: the public channel is listed, not-yet-member for the joiner.
    let (status, dir) = send(&app, "GET", "/api/v1/channels/browse", Some(&joiner), None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = dir.as_array().unwrap();
    let pub_row = rows.iter().find(|c| c["id"] == json!(public_id)).expect("public listed");
    assert_eq!(pub_row["is_member"], json!(false));
    assert!(rows.iter().all(|c| c["id"] != json!(private_id)), "private channel must not be browsable");

    // Join the public channel.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{public_id}/join"),
        Some(&joiner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "join should succeed");

    // Now a member: can post, and browse shows is_member=true.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{public_id}/messages"),
        Some(&joiner),
        Some(json!({ "content": "hi from joiner" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "member should be able to post");

    let (_, dir2) = send(&app, "GET", "/api/v1/channels/browse", Some(&joiner), None).await;
    let pub_row2 = dir2.as_array().unwrap().iter().find(|c| c["id"] == json!(public_id)).unwrap().clone();
    assert_eq!(pub_row2["is_member"], json!(true));
    assert_eq!(pub_row2["member_count"], json!(2));

    // Joining a private channel is forbidden.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{private_id}/join"),
        Some(&joiner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "joining private channel must be forbidden");

    let _ = joiner_id;
}

// ---- Feature 3: last-admin guard ------------------------------------------

#[tokio::test]
async fn last_admin_cannot_leave_with_members_remaining() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (owner, owner_id) = signup(&app, "owner3@example.com", "owner3").await;
    let (member, member_id) = signup(&app, "member3@example.com", "member3").await;

    let cid = make_channel(&app, &owner, "guarded", false).await;

    // Owner adds the member.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/members"),
        Some(&owner),
        Some(json!({ "user_id": member_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Owner (sole admin) can't leave while the member remains.
    let (status, body) = send(
        &app,
        "DELETE",
        &format!("/api/v1/channels/{cid}/members/{owner_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "last admin should be blocked: {body}");

    // The ordinary member can leave freely.
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/v1/channels/{cid}/members/{member_id}"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "member should be able to leave");

    // Now the owner is alone and can leave.
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/v1/channels/{cid}/members/{owner_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sole remaining admin can leave when alone");
}

// ---- Feature 4: custom status ---------------------------------------------

#[tokio::test]
async fn set_and_clear_custom_status() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state.clone());

    let (token, uid) = signup(&app, "status@example.com", "statususer").await;

    // Set a status.
    let (status, body) = send(
        &app,
        "PUT",
        "/api/v1/users/me/status",
        Some(&token),
        Some(json!({ "emoji": "🌴", "text": "on vacation" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set status failed: {body}");
    assert_eq!(body["status_emoji"], json!("🌴"));
    assert_eq!(body["status_text"], json!("on vacation"));

    // /users/me reflects it.
    let (_, me) = send(&app, "GET", "/api/v1/users/me", Some(&token), None).await;
    assert_eq!(me["status_text"], json!("on vacation"));

    // A status whose expiry is in the past reads as absent. Force an expired
    // timestamp directly (the API only accepts future expiries), then confirm
    // the public projection hides it.
    let user_uuid = uuid::Uuid::parse_str(uid.as_str()).unwrap();
    sqlx::query("UPDATE users SET status_expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(user_uuid)
        .execute(&state.db)
        .await
        .unwrap();
    let (_, me2) = send(&app, "GET", "/api/v1/users/me", Some(&token), None).await;
    assert!(me2["status_text"].is_null(), "expired status should read as absent");
    assert!(me2["status_emoji"].is_null(), "expired status emoji should read as absent");

    // Clear via empty body.
    let (status, body) = send(
        &app,
        "PUT",
        "/api/v1/users/me/status",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["status_emoji"].is_null());
    assert!(body["status_text"].is_null());

    // A valid future expiry is accepted and stored.
    let (status, body) = send(
        &app,
        "PUT",
        "/api/v1/users/me/status",
        Some(&token),
        Some(json!({ "emoji": "🔜", "text": "back in an hour", "expires_in_secs": 3600 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status_text"], json!("back in an hour"));
    assert!(body["status_expires_at"].is_string(), "future expiry should be stored");
}

// ---- Feature 5: scheduled messages ----------------------------------------

#[tokio::test]
async fn schedule_list_cancel_and_deliver() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state.clone());

    let (token, _uid) = signup(&app, "sched@example.com", "scheduler").await;
    let cid = make_channel(&app, &token, "sched-room", false).await;

    // A far-future schedule to cancel.
    let future = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let (status, s1) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/scheduled"),
        Some(&token),
        Some(json!({ "content": "later message", "scheduled_for": future })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "schedule failed: {s1}");
    let sched_id = s1["id"].as_str().unwrap().to_string();

    // Past scheduled_for is rejected.
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/scheduled"),
        Some(&token),
        Some(json!({ "content": "too late", "scheduled_for": past })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "past schedule must be rejected");

    // List shows the pending one.
    let (status, listing) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{cid}/scheduled"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing.as_array().unwrap().len(), 1);

    // Cancel it.
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/v1/scheduled/{sched_id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cancel failed");

    let (_, listing2) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{cid}/scheduled"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(listing2.as_array().unwrap().len(), 0, "cancelled schedule should be gone");

    // Insert a due schedule by writing directly, then sweep and verify delivery.
    // We schedule ~1s in the past through a second row via the API is not
    // possible (future-only), so use the public sweep with a row we insert via
    // the pool directly.
    let due = chrono::Utc::now() - chrono::Duration::seconds(5);
    let user_uuid = uuid::Uuid::parse_str(_uid.as_str()).unwrap();
    let chan_uuid = uuid::Uuid::parse_str(cid.as_str()).unwrap();
    sqlx::query(
        "INSERT INTO scheduled_messages (id, channel_id, user_id, content, scheduled_for) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(chan_uuid)
    .bind(user_uuid)
    .bind("delivered by sweeper")
    .bind(due)
    .execute(&state.db)
    .await
    .unwrap();

    let delivered = rsmc_engine::services::scheduler::sweep_once(&state)
        .await
        .unwrap();
    assert_eq!(delivered, 1, "sweeper should deliver exactly one message");

    // The delivered message now appears in history.
    let (_, history) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{cid}/messages"),
        Some(&token),
        None,
    )
    .await;
    let found = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["content"] == json!("delivered by sweeper"));
    assert!(found, "swept message should appear in history");

    // A second sweep with nothing due delivers zero.
    let again = rsmc_engine::services::scheduler::sweep_once(&state)
        .await
        .unwrap();
    assert_eq!(again, 0, "idempotent: no double delivery");
}

// ---- Feature 6: drafts ----------------------------------------------------

#[tokio::test]
async fn draft_save_restore_and_clear() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (token, _uid) = signup(&app, "draft@example.com", "drafter").await;
    let cid = make_channel(&app, &token, "draft-room", false).await;

    // No draft yet → empty content.
    let (status, d0) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{cid}/draft"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(d0["content"], json!(""));

    // Save a draft.
    let (status, _) = send(
        &app,
        "PUT",
        &format!("/api/v1/channels/{cid}/draft"),
        Some(&token),
        Some(json!({ "content": "half-written thought" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Restore it.
    let (_, d1) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{cid}/draft"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(d1["content"], json!("half-written thought"));

    // Empty body clears it.
    let (status, _) = send(
        &app,
        "PUT",
        &format!("/api/v1/channels/{cid}/draft"),
        Some(&token),
        Some(json!({ "content": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, d2) = send(
        &app,
        "GET",
        &format!("/api/v1/channels/{cid}/draft"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(d2["content"], json!(""), "cleared draft reads empty");
}

// ---- Feature 8: uploaded avatars ------------------------------------------

#[tokio::test]
async fn avatar_upload_serve_and_delete() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (token, uid) = signup(&app, "avatar@example.com", "avataruser").await;

    // Multipart upload of a tiny PNG.
    let boundary = "----rsmctestboundary";
    let png: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG signature
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/users/me/avatar")
        .header("authorization", format!("Bearer {token}"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "avatar upload should succeed");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let public: Value = serde_json::from_slice(&bytes).unwrap();
    let avatar_url = public["avatar_url"].as_str().unwrap();
    assert!(
        avatar_url.contains(&format!("/api/v1/users/{uid}/avatar")),
        "avatar_url should point at serve endpoint, got {avatar_url}"
    );

    // Public serve (no auth) returns the bytes.
    let (status, _) = {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/v1/users/{uid}/avatar"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let s = resp.status();
        let b = resp.into_body().collect().await.unwrap().to_bytes();
        (s, b)
    };
    assert_eq!(status, StatusCode::OK, "public avatar serve should work without auth");

    // Delete clears it.
    let (status, body) = send(
        &app,
        "DELETE",
        "/api/v1/users/me/avatar",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["avatar_url"].is_null(), "avatar_url cleared after delete");

    // Serve now 404s.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/users/{uid}/avatar"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "deleted avatar should 404");
}

// ===========================================================================
// 0.8.0: rate limiting
// ===========================================================================

#[tokio::test]
async fn auth_endpoints_are_rate_limited_per_ip() {
    let Some((mut settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    // Enable a tight auth limit for this test: 3 attempts, long window.
    settings.rate_limit = RateLimitConfig {
        enabled: true,
        auth_max: 3,
        auth_window_secs: 300,
        write_max: 1000,
        write_window_secs: 10,
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    // Hammer login with bad credentials. Under oneshot there's no ConnectInfo,
    // so all requests share the "unknown" IP bucket, exactly what we want here.
    let creds = json!({ "email": "nobody@example.com", "password": "wrong" });

    let mut statuses = vec![];
    for _ in 0..5 {
        let (status, _) = send(&app, "POST", "/api/v1/auth/login", None, Some(creds.clone())).await;
        statuses.push(status);
    }

    // First 3 attempts pass through to the handler (401 bad creds); the 4th and
    // 5th are blocked by the limiter (429).
    assert_eq!(statuses[0], StatusCode::UNAUTHORIZED, "1st attempt should reach handler");
    assert_eq!(statuses[2], StatusCode::UNAUTHORIZED, "3rd attempt should reach handler");
    assert_eq!(statuses[3], StatusCode::TOO_MANY_REQUESTS, "4th attempt should be rate-limited");
    assert_eq!(statuses[4], StatusCode::TOO_MANY_REQUESTS, "5th attempt should be rate-limited");
}

#[tokio::test]
async fn rate_limiting_disabled_lets_everything_through() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    // test_settings() disables rate limiting; confirm no throttling occurs.
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let creds = json!({ "email": "nobody@example.com", "password": "wrong" });
    for _ in 0..8 {
        let (status, _) = send(&app, "POST", "/api/v1/auth/login", None, Some(creds.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no request should be rate-limited when disabled");
    }
}

// ===========================================================================
// 0.8.0: search filters
// ===========================================================================

#[tokio::test]
async fn search_filters_by_from_in_and_has() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (owner, owner_id) = signup(&app, "alice@example.com", "alice").await;
    let (bob, bob_id) = signup(&app, "bob@example.com", "bob").await;

    let cid = make_channel(&app, &owner, "project", false).await;
    // add bob to the channel
    send(
        &app,
        "POST",
        &format!("/api/v1/channels/{cid}/members"),
        Some(&owner),
        Some(json!({ "user_id": bob_id })),
    )
    .await;

    // alice and bob both post messages containing "deploy".
    send(&app, "POST", &format!("/api/v1/channels/{cid}/messages"), Some(&owner),
        Some(json!({ "content": "deploy the release now" }))).await;
    send(&app, "POST", &format!("/api/v1/channels/{cid}/messages"), Some(&bob),
        Some(json!({ "content": "deploy failed, rolling back" }))).await;

    // Plain search: both match.
    let (_, all) = send(&app, "GET", "/api/v1/search?q=deploy", Some(&owner), None).await;
    assert_eq!(all["results"].as_array().unwrap().len(), 2, "both 'deploy' messages match");

    // from:bob -> only bob's message.
    let (_, from_bob) = send(&app, "GET", "/api/v1/search?q=from:bob%20deploy", Some(&owner), None).await;
    let hits = from_bob["results"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "only bob's deploy message");
    assert_eq!(hits[0]["user_id"], json!(bob_id));

    // in:#project -> both, in:#nonexistent -> none.
    let (_, in_proj) = send(&app, "GET", "/api/v1/search?q=in:%23project%20deploy", Some(&owner), None).await;
    assert_eq!(in_proj["results"].as_array().unwrap().len(), 2);
    let (_, in_missing) = send(&app, "GET", "/api/v1/search?q=in:%23nope%20deploy", Some(&owner), None).await;
    assert_eq!(in_missing["results"].as_array().unwrap().len(), 0, "unknown channel yields nothing");

    // from:ghost (unknown user) -> none.
    let (_, from_ghost) = send(&app, "GET", "/api/v1/search?q=from:ghost%20deploy", Some(&owner), None).await;
    assert_eq!(from_ghost["results"].as_array().unwrap().len(), 0);

    let _ = owner_id;
}

#[tokio::test]
async fn search_has_file_and_date_filters() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state.clone());

    let (token, _uid) = signup(&app, "filer@example.com", "filer").await;
    let cid = make_channel(&app, &token, "files-ch", false).await;

    // One message with an attachment, one without, both containing "report".
    // Upload a file first.
    let boundary = "----rsmcfiletest";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"file\"; filename=\"r.txt\"\r\n");
    body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
    body.extend_from_slice(b"report data");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = Request::builder()
        .method("POST").uri("/api/v1/files")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body)).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let uploaded: Value = serde_json::from_slice(&bytes).unwrap();
    let file_id = uploaded["id"].as_str().unwrap().to_string();

    send(&app, "POST", &format!("/api/v1/channels/{cid}/messages"), Some(&token),
        Some(json!({ "content": "report with attachment", "attachment_ids": [file_id] }))).await;
    send(&app, "POST", &format!("/api/v1/channels/{cid}/messages"), Some(&token),
        Some(json!({ "content": "report without attachment" }))).await;

    // has:file -> only the one with an attachment.
    let (_, has_file) = send(&app, "GET", "/api/v1/search?q=has:file%20report", Some(&token), None).await;
    assert_eq!(has_file["results"].as_array().unwrap().len(), 1, "only the attachment message");

    // after: tomorrow -> nothing (all messages are from now).
    let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
    let (_, after) = send(&app, "GET", &format!("/api/v1/search?q=after:{tomorrow}%20report"), Some(&token), None).await;
    assert_eq!(after["results"].as_array().unwrap().len(), 0, "nothing after tomorrow");

    // before: tomorrow -> both.
    let (_, before) = send(&app, "GET", &format!("/api/v1/search?q=before:{tomorrow}%20report"), Some(&token), None).await;
    assert_eq!(before["results"].as_array().unwrap().len(), 2, "both before tomorrow");
}

// ===========================================================================
// 0.8.0: pagination on members and pins
// ===========================================================================

#[tokio::test]
async fn members_list_paginates() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (owner, _oid) = signup(&app, "cap@example.com", "captain").await;
    let cid = make_channel(&app, &owner, "crew", false).await;

    // Add several members so pagination has something to page.
    for i in 0..5 {
        let (_, u) = signup(&app, &format!("m{i}@example.com"), &format!("member{i}")).await;
        let _ = u;
    }
    // Re-fetch their ids via user search and add them.
    for i in 0..5 {
        let (_, found) = send(&app, "GET", &format!("/api/v1/users?q=member{i}"), Some(&owner), None).await;
        let uid = found.as_array().unwrap()[0]["id"].as_str().unwrap().to_string();
        send(&app, "POST", &format!("/api/v1/channels/{cid}/members"), Some(&owner),
            Some(json!({ "user_id": uid }))).await;
    }

    // Page size 3: first page has 3 + a cursor.
    let (status, page1) = send(&app, "GET", &format!("/api/v1/channels/{cid}/members?limit=3"), Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["members"].as_array().unwrap().len(), 3, "first page of 3");
    assert!(page1["next_cursor"].is_string(), "more pages remain");

    // Follow the cursor for the rest (6 members total: captain + 5).
    let cursor = page1["next_cursor"].as_str().unwrap();
    let (_, page2) = send(&app, "GET",
        &format!("/api/v1/channels/{cid}/members?limit=3&cursor={cursor}"), Some(&owner), None).await;
    let n2 = page2["members"].as_array().unwrap().len();
    assert_eq!(n2, 3, "second page of 3");

    // No overlap between pages (usernames strictly increase).
    let last1 = page1["members"].as_array().unwrap().last().unwrap()["username"].as_str().unwrap();
    let first2 = page2["members"].as_array().unwrap()[0]["username"].as_str().unwrap();
    assert!(first2 > last1, "pages don't overlap");
}

#[tokio::test]
async fn pins_list_paginates() {
    let Some((settings, _admin)) = test_settings().await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let state = rsmc_engine::bootstrap(settings).await.unwrap();
    let app = rsmc_engine::build_router(state);

    let (token, _uid) = signup(&app, "pinner@example.com", "pinner").await;
    let cid = make_channel(&app, &token, "pins-ch", false).await;

    // Create and pin 4 messages.
    for i in 0..4 {
        let (_, m) = send(&app, "POST", &format!("/api/v1/channels/{cid}/messages"), Some(&token),
            Some(json!({ "content": format!("pin me {i}") }))).await;
        let mid = m["id"].as_str().unwrap().to_string();
        send(&app, "POST", &format!("/api/v1/messages/{mid}/pin"), Some(&token), None).await;
    }

    // Page size 2.
    let (status, page1) = send(&app, "GET", &format!("/api/v1/channels/{cid}/pins?limit=2"), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["pins"].as_array().unwrap().len(), 2, "first page of 2 pins");
    assert!(page1["next_cursor"].is_string(), "more pins remain");

    let cursor = page1["next_cursor"].as_str().unwrap();
    let (_, page2) = send(&app, "GET",
        &format!("/api/v1/channels/{cid}/pins?limit=2&cursor={cursor}"), Some(&token), None).await;
    assert_eq!(page2["pins"].as_array().unwrap().len(), 2, "second page of 2 pins");
    assert!(page2["next_cursor"].is_null(), "no more pins after 4");
}
