//! Authentication & session handlers.

use crate::auth::{password, TokenKind};
use crate::error::{AppError, AppResult};
use crate::middleware::{AuthUser, ValidatedJson};
use crate::models::user::{
    AuthResponse, LoginRequest, RefreshRequest, SignupRequest, User, UserRole,
};
use crate::state::AppState;
use axum::{extract::State, Json};
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

    let user: User = sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (id, email, username, display_name, password_hash, role)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, email, username, display_name, password_hash, role,
                  avatar_url, is_active, created_at, updated_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&req.email)
    .bind(&req.username)
    .bind(&req.display_name)
    .bind(&hash)
    .bind(role)
    .fetch_one(&state.db)
    .await?;

    issue_session(&state, &user).await
}

/// `POST /api/v1/auth/login`
pub async fn login(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<LoginRequest>,
) -> AppResult<Json<AuthResponse>> {
    let user: Option<User> = sqlx::query_as::<_, User>(
        r#"
        SELECT id, email, username, display_name, password_hash, role,
               avatar_url, is_active, created_at, updated_at
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

    issue_session(&state, &user).await
}

/// `POST /api/v1/auth/refresh`
pub async fn refresh(
    State(state): State<AppState>,
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
               avatar_url, is_active, created_at, updated_at
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

    issue_session(&state, &user).await
}

/// `POST /api/v1/auth/logout` — revokes all refresh tokens for the user.
pub async fn logout(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE user_id = $1 AND NOT revoked")
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Shared helper: mint tokens, persist the refresh token, and build response.
async fn issue_session(state: &AppState, user: &User) -> AppResult<Json<AuthResponse>> {
    let pair = state.jwt.issue_pair(user.id, user.role)?;

    sqlx::query(
        r#"
        INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(token_fingerprint(pair.refresh_jti))
    .bind(pair.refresh_expires_at)
    .execute(&state.db)
    .await?;

    Ok(Json(AuthResponse {
        access_token: pair.access,
        refresh_token: pair.refresh,
        token_type: "Bearer",
        expires_in: pair.access_expires_in,
        user: user.to_public(),
    }))
}

// A precomputed Argon2 hash of a random string, used only to equalize timing
// on the "user not found" path of login.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$RdescudvJCsgt3ub+b+dWRWJTmaaJObG";
