//! Authentication & session handlers.

use crate::auth::{password, TokenKind};
use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::user::{
    AuthResponse, LoginRequest, RefreshRequest, SignupRequest, User, UserRole,
};
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};

/// Extract a bounded User-Agent string from request headers (no IP, ever).
fn ua(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(255).collect())
}
use base64::Engine;
use chrono::Utc;
use uuid::Uuid;

/// Hash a refresh token's jti for at-rest storage (so DB leak can't reissue).
fn token_fingerprint(jti: Uuid) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(jti.as_bytes())
}

/// `POST /api/v1/auth/signup`
pub async fn signup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<SignupRequest>,
) -> AppResult<Json<AuthResponse>> {
    // Enforce uniqueness up front for a friendly error (race still caught by DB).
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE LOWER(email) = LOWER($1) OR LOWER(username) = LOWER($2)",
    )
    .bind(&req.email)
    .bind(&req.username)
    .fetch_optional(&state.db)
    .await?;
    if exists.is_some() {
        return Err(AppError::Conflict("email or username already taken".into()));
    }

    let hash = password::hash_password(&req.password)?;

    // First user to register becomes the instance admin.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await?;
    let role = if count == 0 {
        UserRole::Admin
    } else {
        UserRole::Member
    };
    // The first user is auto-verified so a fresh instance can't lock itself out
    // before email/SMTP is configured. Everyone else starts unverified.
    let email_verified = count == 0;

    let user: User = sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (id, email, username, display_name, password_hash, role, email_verified)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, email, username, display_name, password_hash, role,
                  avatar_url, is_active, created_at, updated_at,
                  notify_broadcast, notify_thread_reply, notify_dm
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&req.email)
    .bind(&req.username)
    .bind(&req.display_name)
    .bind(&hash)
    .bind(role)
    .bind(email_verified)
    .fetch_one(&state.db)
    .await?;

    // Default channel: the first user creates "general" (and owns it); every
    // user is then auto-added as a member so the workspace is never empty.
    // Best-effort - a failure here must not block account creation.
    match ensure_default_membership(&state, user.id, role).await {
        Ok(channel_id) => {
            // Broadcast the new member so already-connected clients (e.g. the
            // admin viewing the members list) see them appear live, without a
            // refresh, and post a visible "<user> joined" line to the channel.
            state.events.emit_channel(
                channel_id,
                crate::models::ws_protocol::ServerEvent::MemberJoined {
                    channel_id,
                    user: user.to_public(),
                },
            );
            // Visible feed line, same as the deliberate-join paths. Requested so
            // existing members can see when someone new joins the workspace.
            super::messages::post_system_message(
                &state,
                channel_id,
                format!("{} joined the channel", user.display_name),
            )
            .await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not add user to default channel");
        }
    }

    issue_session(&state, &user, ua(&headers)).await
}

/// Name of the channel every new user is automatically added to.
const DEFAULT_CHANNEL: &str = "general";

/// Ensure the default channel exists (creating it owned by `user_id` if this is
/// the first user) and that `user_id` is a member of it.
async fn ensure_default_membership(
    state: &AppState,
    user_id: Uuid,
    role: UserRole,
) -> AppResult<Uuid> {
    // Find the default channel, or create it if missing. Only the first admin
    // (who has no one to inherit it from) creates it, as its owner.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM channels WHERE name = $1 AND channel_type = 'public'",
    )
    .bind(DEFAULT_CHANNEL)
    .fetch_optional(&state.db)
    .await?;

    let (channel_id, join_role) = match existing {
        Some(id) => (id, crate::models::user::MemberRole::Member),
        None => {
            // Create it. The creator (first user, an admin) joins as owner.
            // A rare race (two simultaneous first-signups) is caught by the
            // unique index on channel name and surfaces as a non-fatal warning.
            let id: Uuid = sqlx::query_scalar(
                r#"
                INSERT INTO channels (id, name, topic, channel_type, created_by)
                VALUES ($1, $2, $3, 'public', $4)
                RETURNING id
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(DEFAULT_CHANNEL)
            .bind("Workspace-wide channel for everyone")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?;
            let r = if matches!(role, UserRole::Admin) {
                crate::models::user::MemberRole::Owner
            } else {
                crate::models::user::MemberRole::Member
            };
            (id, r)
        }
    };

    sqlx::query(
        "INSERT INTO channel_members (channel_id, user_id, role)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(channel_id)
    .bind(user_id)
    .bind(join_role)
    .execute(&state.db)
    .await?;

    Ok(channel_id)
}

/// `POST /api/v1/auth/login`
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<LoginRequest>,
) -> AppResult<Json<crate::models::user::LoginOutcome>> {
    let user: Option<User> = sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, username, display_name, password_hash, role,
               avatar_url, is_active, created_at, updated_at,
               notify_broadcast, notify_thread_reply, notify_dm
        FROM users WHERE LOWER(email) = LOWER($1)
        "#,
    )
    .bind(&req.email)
    .fetch_optional(&state.db)
    .await?;

    // Constant-ish behavior: always verify against *something*.
    let user = match user {
        Some(u) => u,
        None => {
            // Perform a dummy verify to reduce user-enumeration timing signal.
            let _ = password::verify_password(&req.password, DUMMY_HASH);
            return Err(AppError::Unauthorized);
        }
    };

    if !user.is_active {
        return Err(AppError::Forbidden("account is disabled".into()));
    }

    if !password::verify_password(&req.password, &user.password_hash)? {
        return Err(AppError::Unauthorized);
    }

    // Soft by default: unverified users may log in. When the instance requires
    // verification, block login until the email is confirmed (with a clear code
    // the UI can act on to offer a resend).
    if state.config.email.require_verification {
        let verified: bool =
            sqlx::query_scalar("SELECT email_verified FROM users WHERE id = $1")
                .bind(user.id)
                .fetch_one(&state.db)
                .await?;
        if !verified {
            return Err(AppError::Forbidden("email not verified".into()));
        }
    }

    // If MFA is enabled, do NOT issue tokens yet. Return a short-lived challenge
    // the client exchanges via /auth/mfa/verify with a valid TOTP code.
    let mfa_enabled: bool = sqlx::query_scalar("SELECT mfa_enabled FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    if mfa_enabled {
        let challenge = state.jwt.issue_mfa_challenge(user.id, user.role)?;
        return Ok(Json(crate::models::user::LoginOutcome::MfaChallenge {
            mfa_required: true,
            mfa_token: challenge,
        }));
    }

    let resp = issue_session(&state, &user, ua(&headers)).await?;
    Ok(Json(crate::models::user::LoginOutcome::Authenticated(resp.0)))
}

/// `POST /api/v1/auth/mfa/verify` - complete an MFA login.
pub async fn mfa_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<crate::models::user::MfaVerifyRequest>,
) -> AppResult<Json<AuthResponse>> {
    // Validate the challenge token (must be the Mfa kind).
    let claims = state
        .jwt
        .verify(&req.mfa_token, crate::auth::jwt::TokenKind::Mfa)
        .map_err(|_| AppError::Unauthorized)?;

    // Load the user + encrypted secret.
    let row: Option<(User, Option<String>, bool)> = {
        let user: Option<User> = sqlx::query_as::<_, User>(
            r#"
            SELECT id, email, username, display_name, password_hash, role,
                   avatar_url, is_active, created_at, updated_at,
                   notify_broadcast, notify_thread_reply, notify_dm
            FROM users WHERE id = $1
            "#,
        )
        .bind(claims.sub)
        .fetch_optional(&state.db)
        .await?;
        match user {
            Some(u) => {
                let (enc, enabled): (Option<String>, bool) = sqlx::query_as(
                    "SELECT mfa_secret_enc, mfa_enabled FROM users WHERE id = $1",
                )
                .bind(claims.sub)
                .fetch_one(&state.db)
                .await?;
                Some((u, enc, enabled))
            }
            None => None,
        }
    };

    let (user, enc, enabled) = row.ok_or(AppError::Unauthorized)?;
    if !enabled {
        return Err(AppError::Unauthorized);
    }
    let enc = enc.ok_or(AppError::Unauthorized)?;
    let secret = crate::services::mfa::decrypt_secret(state.config.auth.mfa_key(), &enc)
        .map_err(|_| AppError::Unauthorized)?;

    if !crate::services::mfa::verify(&secret, &req.code) {
        return Err(AppError::Unauthorized);
    }

    let resp = issue_session(&state, &user, ua(&headers)).await?;
    Ok(resp)
}

/// `POST /api/v1/auth/refresh`
pub async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<RefreshRequest>,
) -> AppResult<Json<AuthResponse>> {
    let claims = state.jwt.verify(&req.refresh_token, TokenKind::Refresh)?;
    let fingerprint = token_fingerprint(claims.jti);

    // The stored token must exist, be unexpired, and not revoked.
    let row: Option<(Uuid, bool, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT user_id, revoked, expires_at FROM refresh_tokens WHERE token_hash = $1",
    )
    .bind(&fingerprint)
    .fetch_optional(&state.db)
    .await?;

    let (user_id, revoked, expires_at) = row.ok_or(AppError::Unauthorized)?;
    if revoked || expires_at < Utc::now() || user_id != claims.sub {
        return Err(AppError::Unauthorized);
    }

    // Rotate: revoke the old refresh token.
    sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE token_hash = $1")
        .bind(&fingerprint)
        .execute(&state.db)
        .await?;

    let user: User = sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, username, display_name, password_hash, role,
               avatar_url, is_active, created_at, updated_at,
               notify_broadcast, notify_thread_reply, notify_dm
        FROM users WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;

    // A deactivated user must not be able to extend their session.
    if !user.is_active {
        return Err(AppError::Unauthorized);
    }

    issue_session(&state, &user, ua(&headers)).await
}

/// `POST /api/v1/auth/logout` - revokes all refresh tokens for the user.
pub async fn logout(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE user_id = $1 AND NOT revoked")
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(super::ok())
}

/// Shared helper: mint tokens, persist the refresh token, and build response.
/// `user_agent` is captured for session management (never an IP); None when
/// unavailable.
async fn issue_session(
    state: &AppState,
    user: &User,
    user_agent: Option<String>,
) -> AppResult<Json<AuthResponse>> {
    let pair = state.jwt.issue_pair(user.id, user.role)?;

    sqlx::query(
        r#"
        INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at, user_agent, last_used_at)
        VALUES ($1, $2, $3, $4, $5, now())
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(token_fingerprint(pair.refresh_jti))
    .bind(pair.refresh_expires_at)
    .bind(user_agent.as_deref())
    .execute(&state.db)
    .await?;

    // Include the caller's own verification state so the client can render the
    // unverified banner immediately after login/signup (not only after a later
    // /users/me call).
    let email_verified: bool =
        sqlx::query_scalar("SELECT email_verified FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(true);
    let mut public = user.to_public();
    public.email_verified = Some(email_verified);
    // Include the caller's own notification preferences so the client has them
    // immediately after login/signup/refresh (not only after a later
    // /users/me call), mirroring how email_verified is populated above.
    public.notification_prefs = Some(user.notification_prefs());

    Ok(Json(AuthResponse {
        access_token: pair.access,
        refresh_token: pair.refresh,
        token_type: "Bearer",
        expires_in: pair.access_expires_in,
        user: public,
    }))
}

// A precomputed Argon2 hash of a random string, used only to equalize timing
// on the "user not found" path of login.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$RdescudvJCsgt3ub+b+dWRWJTmaaJObG";
