//! File attachment domain models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Attachment {
    pub id: Uuid,
    pub uploader_id: Uuid,
    /// Channel the file is scoped to for access control. `None` until attached.
    pub channel_id: Option<Uuid>,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    /// Server-side relative storage path (never exposed directly).
    #[serde(skip_serializing)]
    pub storage_path: String,
    pub created_at: DateTime<Utc>,
}

/// Metadata returned after a successful upload.
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    /// URL the client uses to download the file (auth-gated).
    pub url: String,
}
