//! Uploaded-avatar handlers: multipart upload, delete, and public serving.
//!
//! Avatars are stored on the same local volume as message attachments, under a
//! per-user path. The public `serve_avatar` endpoint carries no auth so an
//! `<img src>` can load it directly; upload sets `avatar_url` to that endpoint
//! with a `?v=N` cache-buster that bumps on every change.

use crate::error::{AppError, AppResult};
use crate::middleware::AuthUser;
use crate::models::user::{User, UserPublic};
use crate::models::ws_protocol::ServerEvent;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Cap avatar uploads well below the general file limit; avatars are small.
const MAX_AVATAR_BYTES: usize = 5 * 1024 * 1024;

const USER_COLS: &str = "id, email, username, display_name, password_hash, role, \
     avatar_url, is_active, created_at, updated_at, \
     status_emoji, status_text, status_expires_at";

fn avatar_dir(state: &AppState) -> std::path::PathBuf {
    std::path::Path::new(&state.config.storage.upload_dir).join("avatars")
}

/// `POST /api/v1/users/me/avatar` - multipart image upload.
pub async fn upload_avatar(
    State(state): State<AppState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> AppResult<Json<UserPublic>> {
    let dir = avatar_dir(&state);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("cannot create avatar dir: {e}")))?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let filename = field.file_name().unwrap_or("").to_ascii_lowercase();

        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        if data.len() > MAX_AVATAR_BYTES {
            return Err(AppError::BadRequest(
                "avatar exceeds the maximum size".into(),
            ));
        }
        if data.is_empty() {
            return Err(AppError::BadRequest("avatar file is empty".into()));
        }

        // Accept an image if ANY reliable signal says so: the declared
        // content-type, a known image extension, or the file's magic bytes.
        // Relying on content-type alone wrongly rejects real images that some
        // clients upload as "application/octet-stream".
        let ct_is_image = content_type.starts_with("image/");
        let ext_is_image = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg"]
            .iter()
            .any(|e| filename.ends_with(e));
        let bytes_are_image = looks_like_image(&data);
        if !(ct_is_image || ext_is_image || bytes_are_image) {
            return Err(AppError::BadRequest(
                "avatar must be an image (PNG, JPEG, GIF, or WebP)".into(),
            ));
        }

        // Store under a stable per-user filename so old files are overwritten.
        let storage_name = format!("{}", user.id);
        let path = dir.join(&storage_name);
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| AppError::Internal(format!("cannot write avatar: {e}")))?;
        file.write_all(&data)
            .await
            .map_err(|e| AppError::Internal(format!("write failed: {e}")))?;
        file.flush().await.ok();

        // Bump the version for cache-busting and point avatar_url at the public
        // serve endpoint. Both are returned in the refreshed public user.
        let updated: User = sqlx::query_as::<_, User>(&format!(
            r#"
            UPDATE users
            SET avatar_path = $2,
                avatar_content_type = $3,
                avatar_version = avatar_version + 1,
                avatar_url = '/api/v1/users/' || $1 || '/avatar?v=' || (avatar_version + 1)
            WHERE id = $1
            RETURNING {USER_COLS}
            "#
        ))
        .bind(user.id)
        .bind(&storage_name)
        .bind(&content_type)
        .fetch_one(&state.db)
        .await?;

        let public = updated.to_public();
        state.events.emit_all(ServerEvent::ProfileUpdated {
            user_id: public.id,
            display_name: public.display_name.clone(),
            avatar_url: public.avatar_url.clone(),
        });
        return Ok(Json(public));
    }

    Err(AppError::BadRequest("no 'file' field in upload".into()))
}

/// `DELETE /api/v1/users/me/avatar` - remove the uploaded avatar.
pub async fn delete_avatar(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<UserPublic>> {
    // Best-effort file removal; the DB is the source of truth.
    let path = avatar_dir(&state).join(format!("{}", user.id));
    let _ = tokio::fs::remove_file(&path).await;

    let updated: User = sqlx::query_as::<_, User>(&format!(
        r#"
        UPDATE users
        SET avatar_path = NULL,
            avatar_content_type = NULL,
            avatar_url = NULL,
            avatar_version = avatar_version + 1
        WHERE id = $1
        RETURNING {USER_COLS}
        "#
    ))
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;

    let public = updated.to_public();
    state.events.emit_all(ServerEvent::ProfileUpdated {
        user_id: public.id,
        display_name: public.display_name.clone(),
        avatar_url: public.avatar_url.clone(),
    });
    Ok(Json(public))
}

/// `GET /api/v1/users/:id/avatar` - public, unauthenticated serve so `<img>`
/// tags can load it. Returns 404 when the user has no uploaded avatar.
pub async fn serve_avatar(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT avatar_path, avatar_content_type FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let (Some(storage_name), content_type) = row
        .map(|(p, ct)| (p, ct))
        .unwrap_or((None, None))
    else {
        return Err(AppError::NotFound("avatar".into()));
    };

    let path = avatar_dir(&state).join(&storage_name);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound("avatar content".into()))?;

    let ct = content_type.unwrap_or_else(|| "application/octet-stream".to_string());
    let response = Response::builder()
        .header(header::CONTENT_TYPE, ct)
        // Cache but allow the ?v=N bump to invalidate.
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(response.into_response())
}

/// Best-effort check that bytes are a common image, by magic number. Covers
/// PNG, JPEG, GIF, WebP, and BMP. Used as one of several signals so that images
/// uploaded as "application/octet-stream" are still accepted.
fn looks_like_image(data: &[u8]) -> bool {
    if data.len() < 12 {
        return false;
    }
    let png = &data[..8] == b"\x89PNG\r\n\x1a\n";
    let jpeg = data[..3] == [0xFF, 0xD8, 0xFF];
    let gif = &data[..6] == b"GIF87a" || &data[..6] == b"GIF89a";
    let webp = &data[..4] == b"RIFF" && &data[8..12] == b"WEBP";
    let bmp = &data[..2] == b"BM";
    png || jpeg || gif || webp || bmp
}
