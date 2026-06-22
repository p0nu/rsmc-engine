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

/// Default directory backups live in when the admin gives a bare filename.
const DEFAULT_BACKUP_DIR: &str = "/app/backups";

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

/// Resolve the directory used for backups, honoring an env override.
fn backup_dir() -> PathBuf {
    std::env::var("APP__STORAGE__BACKUP_DIR")
        .or_else(|_| std::env::var("BACKUP_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_BACKUP_DIR))
}

/// Resolve a user-supplied path/filename to a constrained path: bare filename
/// goes in the backups dir; absolute paths must live under it.
fn resolve_target(input: Option<String>) -> AppResult<PathBuf> {
    let dir = backup_dir();
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
    let target = resolve_target(req.path)?;

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

/// GET /api/v1/system/backups — list dumps in the backups directory.
pub async fn list_backups(
    State(_state): State<AppState>,
    AdminUser(_admin): AdminUser,
) -> AppResult<Json<Vec<BackupEntry>>> {
    let dir = backup_dir();
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

/// GET /api/v1/system/info — surface the backup directory and tool availability.
pub async fn info(
    State(_state): State<AppState>,
    AdminUser(_admin): AdminUser,
) -> AppResult<Json<serde_json::Value>> {
    let dir = backup_dir();
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
