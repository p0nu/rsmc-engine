//! System admin endpoints: database backup & restore.
//!
//! Admin-only; shells out to `pg_dump` / `pg_restore` for self-hosted
//! snapshot/recovery. Paths are constrained to a configured backups dir;
//! restore is destructive and gated behind an explicit `confirm` flag.

use std::path::{Path, PathBuf};

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::middleware::AdminUser;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct BackupRequest {
    /// Optional dump path or filename. Omitted = timestamped name; bare
    /// filename (no slash) goes in the default backups dir.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackupResponse {
    pub ok: bool,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct RestoreRequest {
    /// Path to a dump previously produced by the backup endpoint.
    pub path: String,
    /// Must be `true`; restore overwrites current data.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Serialize)]
pub struct BackupEntry {
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: String,
}

/// Resolve the directory used for backups. An explicit environment override
/// wins; otherwise the configured storage.backup_dir is the single source of
/// truth (default "./backups"). This keeps the effective-configuration log and
/// the actual write location in agreement on a bare-binary install.
fn backup_dir(state: &AppState) -> PathBuf {
    std::env::var("APP__STORAGE__BACKUP_DIR")
        .or_else(|_| std::env::var("BACKUP_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(&state.config.storage.backup_dir))
}

/// Resolve a user-supplied path/filename to a constrained path: bare filename
/// goes in the backups dir; absolute paths must live under it.
fn resolve_target(state: &AppState, input: Option<String>) -> AppResult<PathBuf> {
    let dir = backup_dir(state);
    let raw = input
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            format!("rsmc-{ts}.dump")
        });

    // Reject traversal attempts outright.
    if raw.contains("..") {
        return Err(AppError::BadRequest("path must not contain '..'".into()));
    }

    let candidate = if raw.contains('/') {
        PathBuf::from(&raw)
    } else {
        dir.join(&raw)
    };

    // Constrain absolute paths to the backups directory.
    let within = candidate.starts_with(&dir);
    if candidate.is_absolute() && !within {
        return Err(AppError::BadRequest(format!(
            "path must be inside the backups directory ({})",
            dir.display()
        )));
    }

    Ok(candidate)
}

fn database_url(state: &AppState) -> String {
    state.config.database.url.clone()
}

/// POST /api/v1/system/backup
pub async fn backup(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    body: Option<Json<BackupRequest>>,
) -> AppResult<Json<BackupResponse>> {
    let req = body.map(|Json(b)| b).unwrap_or(BackupRequest { path: None });
    let target = resolve_target(&state, req.path)?;

    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Internal(format!("cannot create backup dir: {e}")))?;
    }

    let url = database_url(&state);

    // Custom format (-Fc): enables pg_restore parallelism/selective restore.
    let output = tokio::process::Command::new("pg_dump")
        .arg("--format=custom")
        .arg("--no-owner")
        .arg("--no-privileges")
        .arg("--file")
        .arg(&target)
        .arg(&url)
        .output()
        .await
        .map_err(|e| AppError::Operation(format!("failed to launch pg_dump: {e}. Is postgresql-client installed in the image?")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Operation(format!("pg_dump failed: {}", stderr.trim())));
    }

    let meta = tokio::fs::metadata(&target)
        .await
        .map_err(|e| AppError::Internal(format!("backup written but unreadable: {e}")))?;

    tracing::info!(path = %target.display(), size = meta.len(), "database backup created");

    Ok(Json(BackupResponse {
        ok: true,
        path: target.display().to_string(),
        size_bytes: meta.len(),
        created_at: chrono::Utc::now().to_rfc3339(),
    }))
}

/// POST /api/v1/system/restore
pub async fn restore(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    Json(req): Json<RestoreRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if !req.confirm {
        return Err(AppError::BadRequest(
            "restore is destructive; set confirm=true to proceed".into(),
        ));
    }

    let path = PathBuf::from(req.path.trim());
    if !path_exists(&path).await {
        return Err(AppError::NotFound(format!(
            "backup file not found: {}",
            path.display()
        )));
    }

    let url = database_url(&state);

    // --clean --if-exists drops existing objects before recreating them, so a
    // restore replaces current contents. Single transaction for atomicity.
    let output = tokio::process::Command::new("pg_restore")
        .arg("--clean")
        .arg("--if-exists")
        .arg("--no-owner")
        .arg("--no-privileges")
        .arg("--single-transaction")
        .arg("--dbname")
        .arg(&url)
        .arg(&path)
        .output()
        .await
        .map_err(|e| AppError::Operation(format!("failed to launch pg_restore: {e}. Is postgresql-client installed in the image?")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Operation(format!("pg_restore failed: {}", stderr.trim())));
    }

    tracing::warn!(path = %path.display(), "database restored from backup");

    Ok(Json(json!({
        "ok": true,
        "restored_from": path.display().to_string(),
    })))
}

/// GET /api/v1/system/backups - list dumps in the backups directory.
pub async fn list_backups(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
) -> AppResult<Json<Vec<BackupEntry>>> {
    let dir = backup_dir(&state);
    let mut entries = Vec::new();

    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(rd) => rd,
        Err(_) => return Ok(Json(entries)), // dir may not exist yet
    };

    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata().await {
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            entries.push(BackupEntry {
                path: p.display().to_string(),
                size_bytes: meta.len(),
                modified_at: modified,
            });
        }
    }

    // newest first
    entries.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(Json(entries))
}

/// GET /api/v1/system/info - surface the backup directory and tool availability.
pub async fn info(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
) -> AppResult<Json<serde_json::Value>> {
    let dir = backup_dir(&state);
    let pg_dump = which("pg_dump").await;
    let pg_restore = which("pg_restore").await;
    Ok(Json(json!({
        "backup_dir": dir.display().to_string(),
        "pg_dump_available": pg_dump,
        "pg_restore_available": pg_restore,
    })))
}

async fn path_exists(p: &Path) -> bool {
    tokio::fs::metadata(p).await.is_ok()
}

async fn which(bin: &str) -> bool {
    tokio::process::Command::new(bin)
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Process start time, set once at startup so uptime can be reported without
/// threading a timestamp through `AppState`.
static PROCESS_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Call once during bootstrap so uptime is measured from boot.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(std::time::Instant::now());
}

/// `GET /api/v1/system/stats` - workspace, server, and database overview for
/// the admin console. Admin-only. All counts are cheap aggregates; nothing here
/// exposes secrets or message content.
pub async fn stats(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
) -> AppResult<Json<serde_json::Value>> {
    let db = &state.db;

    let (users_total, users_active): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COUNT(*) FILTER (WHERE is_active) FROM users")
            .fetch_one(db)
            .await?;

    let (ch_public, ch_private, ch_group, ch_direct): (i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
          COUNT(*) FILTER (WHERE channel_type = 'public'),
          COUNT(*) FILTER (WHERE channel_type = 'private'),
          COUNT(*) FILTER (WHERE channel_type = 'group'),
          COUNT(*) FILTER (WHERE channel_type = 'direct')
        FROM channels
        "#,
    )
    .fetch_one(db)
    .await?;

    let (msgs_total, msgs_24h, msgs_7d): (i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
          COUNT(*) FILTER (WHERE deleted_at IS NULL),
          COUNT(*) FILTER (WHERE deleted_at IS NULL AND created_at > now() - interval '24 hours'),
          COUNT(*) FILTER (WHERE deleted_at IS NULL AND created_at > now() - interval '7 days')
        FROM messages
        "#,
    )
    .fetch_one(db)
    .await?;

    let (files_count, files_bytes): (i64, Option<i64>) =
        sqlx::query_as("SELECT COUNT(*), SUM(size_bytes)::bigint FROM attachments")
            .fetch_one(db)
            .await?;

    let db_size_bytes: i64 =
        sqlx::query_scalar("SELECT pg_database_size(current_database())::bigint")
            .fetch_one(db)
            .await
            .unwrap_or(0);
    let db_version: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(db)
        .await
        .unwrap_or_else(|_| "unknown".into());
    let migration_version: Option<i64> =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(db)
            .await
            .unwrap_or(None);

    let uptime_seconds = PROCESS_START.get().map(|t| t.elapsed().as_secs()).unwrap_or(0);

    Ok(Json(json!({
        "workspace": {
            "users_total": users_total,
            "users_active": users_active,
            "users_deactivated": users_total - users_active,
            "channels_public": ch_public,
            "channels_private": ch_private,
            "channels_group": ch_group,
            "channels_direct": ch_direct,
            "messages_total": msgs_total,
            "messages_24h": msgs_24h,
            "messages_7d": msgs_7d,
            "files_count": files_count,
            "files_bytes": files_bytes.unwrap_or(0),
        },
        "server": {
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": uptime_seconds,
            "arch": std::env::consts::ARCH,
            "os": std::env::consts::OS,
        },
        "database": {
            "size_bytes": db_size_bytes,
            "server_version": db_version,
            "migration_version": migration_version,
            "pool_size": db.size(),
            "pool_idle": db.num_idle(),
        },
        "realtime": {
            "online_users": state.hub.online_count(),
            "redis_enabled": state.config.redis.url.is_some(),
        },
    })))
}

/// GET /api/v1/system/audit - list recent audit events (admin only).
///
/// Supports `limit` (default 50, max 200) and an optional `action` filter.
/// Returns newest first.
pub async fn audit_log(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    axum::extract::Query(q): axum::extract::Query<AuditQuery>,
) -> AppResult<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let rows = sqlx::query_as::<_, AuditRow>(
        r#"
        SELECT id, action, actor_id, actor_label, target_kind, target_id,
               target_label, metadata, created_at
        FROM audit_events
        WHERE ($1::text IS NULL OR action = $1)
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(q.action.as_deref())
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "events": rows })))
}

#[derive(Debug, serde::Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
    pub action: Option<String>,
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
struct AuditRow {
    id: uuid::Uuid,
    action: String,
    actor_id: Option<uuid::Uuid>,
    actor_label: String,
    target_kind: Option<String>,
    target_id: Option<String>,
    target_label: Option<String>,
    metadata: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
}
