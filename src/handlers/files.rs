//! File upload & download handlers with channel-scoped access control.

use crate::error::{AppError, AppResult};
use crate::middleware::AuthUser;
use crate::models::file::{Attachment, UploadResponse};
use crate::services::access;
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

/// `POST /api/v1/files` — multipart upload. Returns metadata + download URL.
///
/// The file is stored on the configured local volume under a UUID-named path.
/// It starts unattached (`channel_id = NULL`) and becomes channel-scoped when
/// referenced by a message.
pub async fn upload(
    State(state): State<AppState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> AppResult<Json<UploadResponse>> {
    let max = state.config.storage.max_file_bytes;
    let dir = std::path::Path::new(&state.config.storage.upload_dir);
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| AppError::Internal(format!("cannot create upload dir: {e}")))?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field
            .file_name()
            .map(sanitize_filename)
            .unwrap_or_else(|| "upload.bin".to_string());
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let id = Uuid::new_v4();
        let storage_name = format!("{id}");
        let path = dir.join(&storage_name);

        // Stream to disk while enforcing the size cap.
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| AppError::Internal(format!("cannot create file: {e}")))?;
        let mut written: usize = 0;
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        written += data.len();
        if written > max {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AppError::BadRequest("file exceeds maximum size".into()));
        }
        file.write_all(&data)
            .await
            .map_err(|e| AppError::Internal(format!("write failed: {e}")))?;
        file.flush().await.ok();

        let attachment: Attachment = sqlx::query_as::<_, Attachment>(
            r#"
            INSERT INTO attachments
                (id, uploader_id, channel_id, filename, content_type, size_bytes, storage_path)
            VALUES ($1, $2, NULL, $3, $4, $5, $6)
            RETURNING id, uploader_id, channel_id, filename, content_type,
                      size_bytes, storage_path, created_at
            "#,
        )
        .bind(id)
        .bind(user.id)
        .bind(&filename)
        .bind(&content_type)
        .bind(written as i64)
        .bind(&storage_name)
        .fetch_one(&state.db)
        .await?;

        return Ok(Json(UploadResponse {
            id: attachment.id,
            filename: attachment.filename,
            content_type: attachment.content_type,
            size_bytes: attachment.size_bytes,
            url: format!("/api/v1/files/{}", attachment.id),
        }));
    }

    Err(AppError::BadRequest("no 'file' field in upload".into()))
}

/// `GET /api/v1/files/:id` — download, enforcing channel membership.
pub async fn download(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let attachment: Attachment = sqlx::query_as::<_, Attachment>(
        r#"
        SELECT id, uploader_id, channel_id, filename, content_type,
               size_bytes, storage_path, created_at
        FROM attachments WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("file".into()))?;

    // Access rule: uploader always; otherwise must be a member of the channel
    // the file is attached to. Unattached files are uploader-only.
    if attachment.uploader_id != user.id {
        match attachment.channel_id {
            Some(cid) => {
                access::require_member(&state.db, cid, user.id).await?;
            }
            None => return Err(AppError::Forbidden("file is not shared".into())),
        }
    }

    let path = std::path::Path::new(&state.config.storage.upload_dir).join(&attachment.storage_path);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound("file content".into()))?;

    let response = Response::builder()
        .header(header::CONTENT_TYPE, attachment.content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", attachment.filename),
        )
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(response.into_response())
}

/// Strip path components and dangerous characters from an uploaded filename.
fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.chars()
        .filter(|c| !matches!(c, '\0'..='\x1f'))
        .take(255)
        .collect()
}
