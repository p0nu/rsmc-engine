//! Email-related auth endpoints: verification, password reset, email change.
//!
//! All token lookups are by SHA-256 hash; raw tokens only ever travel by email.
//! Reset requests always return 200 regardless of whether the email exists, to
//! avoid revealing which addresses have accounts (user enumeration).

use crate::error::{AppError, AppResult};
use crate::middleware::AuthUser;
use crate::services::email::{self, Mail};
use crate::state::AppState;
use axum::{extract::State, Json};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

fn token_ttl(state: &AppState) -> Duration {
    Duration::seconds(state.config.email.token_ttl_secs as i64)
}

/// Issue an email token of the given purpose, store its hash, and return the raw.
async fn issue(
    state: &AppState,
    user_id: Uuid,
    purpose: &str,
    new_email: Option<&str>,
) -> AppResult<String> {
    let t = email::generate_token();
    let expires = Utc::now() + token_ttl(state);
    sqlx::query(
        "INSERT INTO email_tokens (user_id, purpose, token_hash, new_email, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(user_id)
    .bind(purpose)
    .bind(&t.hash)
    .bind(new_email)
    .bind(expires)
    .execute(&state.db)
    .await?;
    Ok(t.raw)
}

/// Look up a live (unconsumed, unexpired) token by raw value and purpose.
async fn lookup(
    state: &AppState,
    raw: &str,
    purpose: &str,
) -> AppResult<(Uuid, Uuid, Option<String>)> {
    let hash = email::hash_token(raw);
    let row = sqlx::query_as::<_, (Uuid, Uuid, Option<String>)>(
        "SELECT id, user_id, new_email FROM email_tokens
         WHERE token_hash = $1 AND purpose = $2
           AND consumed_at IS NULL AND expires_at > now()",
    )
    .bind(&hash)
    .bind(purpose)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("invalid or expired token".into()))?;
    Ok(row)
}

async fn consume(state: &AppState, token_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE email_tokens SET consumed_at = now() WHERE id = $1")
        .bind(token_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

// ---- Verification ----------------------------------------------------------

/// Send (or resend) a verification email to the authenticated user.
pub async fn send_verification(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    // Fetch current email + verified state.
    let (email_addr, verified): (String, bool) =
        sqlx::query_as("SELECT email, email_verified FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;

    if verified {
        return Ok(Json(json!({ "status": "already_verified" })));
    }

    let raw = issue(&state, user.id, "verify", None).await?;
    let link = email::action_link(&state.config.email, "/verify-email", &raw);
    state
        .mail
        .send(Mail {
            to: email_addr,
            subject: "Verify your email".into(),
            body: format!("Confirm your email by opening this link:\n{link}"),
        })
        .await
        .map_err(|e| AppError::Internal(format!("failed to send email: {e}")))?;

    Ok(Json(json!({ "status": "sent" })))
}

#[derive(Deserialize)]
pub struct TokenBody {
    pub token: String,
}

/// Confirm a verification token. Public (the user may not be logged in).
pub async fn verify_email(
    State(state): State<AppState>,
    Json(body): Json<TokenBody>,
) -> AppResult<Json<serde_json::Value>> {
    let (token_id, user_id, _) = lookup(&state, &body.token, "verify").await?;
    sqlx::query("UPDATE users SET email_verified = TRUE, updated_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await?;
    consume(&state, token_id).await?;
    Ok(Json(json!({ "status": "verified" })))
}

// ---- Password reset --------------------------------------------------------

#[derive(Deserialize)]
pub struct ResetRequestBody {
    pub email: String,
}

/// Request a password-reset email. Always returns 200 (no user enumeration).
pub async fn request_password_reset(
    State(state): State<AppState>,
    Json(body): Json<ResetRequestBody>,
) -> AppResult<Json<serde_json::Value>> {
    let found: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, email FROM users WHERE LOWER(email) = LOWER($1) AND is_active")
            .bind(&body.email)
            .fetch_optional(&state.db)
            .await?;

    if let Some((user_id, email_addr)) = found {
        let raw = issue(&state, user_id, "reset", None).await?;
        let link = email::action_link(&state.config.email, "/reset-password", &raw);
        // Best-effort send; do not leak failures back to the caller.
        if let Err(e) = state
            .mail
            .send(Mail {
                to: email_addr,
                subject: "Reset your password".into(),
                body: format!("Reset your password with this link:\n{link}"),
            })
            .await
        {
            tracing::warn!("password reset email failed to send: {e}");
        }
    }

    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub struct ResetConfirmBody {
    pub token: String,
    pub new_password: String,
}

/// Complete a password reset with a token and a new password.
pub async fn confirm_password_reset(
    State(state): State<AppState>,
    Json(body): Json<ResetConfirmBody>,
) -> AppResult<Json<serde_json::Value>> {
    if body.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    let (token_id, user_id, _) = lookup(&state, &body.token, "reset").await?;
    let hash = crate::auth::password::hash_password(&body.new_password)?;
    sqlx::query("UPDATE users SET password_hash = $1, updated_at = now() WHERE id = $2")
        .bind(&hash)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    consume(&state, token_id).await?;
    // Invalidate existing sessions for safety.
    sqlx::query("UPDATE refresh_tokens SET revoked = TRUE WHERE user_id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await
        .ok();
    Ok(Json(json!({ "status": "reset" })))
}

// ---- Email change ----------------------------------------------------------

#[derive(Deserialize)]
pub struct ChangeRequestBody {
    pub new_email: String,
}

/// Request an email change; sends a confirmation link to the NEW address.
pub async fn request_email_change(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ChangeRequestBody>,
) -> AppResult<Json<serde_json::Value>> {
    let new_email = body.new_email.trim().to_string();
    if !new_email.contains('@') {
        return Err(AppError::BadRequest("invalid email address".into()));
    }
    // Reject if already in use.
    let taken: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE LOWER(email) = LOWER($1)")
            .bind(&new_email)
            .fetch_optional(&state.db)
            .await?;
    if taken.is_some() {
        return Err(AppError::BadRequest("email already in use".into()));
    }

    let raw = issue(&state, user.id, "change", Some(&new_email)).await?;
    let link = email::action_link(&state.config.email, "/confirm-email-change", &raw);
    state
        .mail
        .send(Mail {
            to: new_email,
            subject: "Confirm your new email".into(),
            body: format!("Confirm your new email address by opening this link:\n{link}"),
        })
        .await
        .map_err(|e| AppError::Internal(format!("failed to send email: {e}")))?;

    Ok(Json(json!({ "status": "sent" })))
}

/// Confirm an email change; swaps the address and marks it verified.
pub async fn confirm_email_change(
    State(state): State<AppState>,
    Json(body): Json<TokenBody>,
) -> AppResult<Json<serde_json::Value>> {
    let (token_id, user_id, new_email) = lookup(&state, &body.token, "change").await?;
    let new_email = new_email
        .ok_or_else(|| AppError::Internal("change token missing new_email".into()))?;

    // Re-check availability at confirm time.
    let taken: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE LOWER(email) = LOWER($1) AND id <> $2",
    )
    .bind(&new_email)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    if taken.is_some() {
        return Err(AppError::BadRequest("email already in use".into()));
    }

    sqlx::query(
        "UPDATE users SET email = $1, email_verified = TRUE, updated_at = now() WHERE id = $2",
    )
    .bind(&new_email)
    .bind(user_id)
    .execute(&state.db)
    .await?;
    consume(&state, token_id).await?;
    Ok(Json(json!({ "status": "changed" })))
}
