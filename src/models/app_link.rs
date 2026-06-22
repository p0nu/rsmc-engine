//! App link (workspace bookmark) domain models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// Max number of app links allowed; keeps the Apps panel compact.
pub const MAX_APP_LINKS: i64 = 5;

/// An admin-curated shortcut to an external tool, shown to all users.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AppLink {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub position: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateAppLinkRequest {
    #[validate(length(min = 1, max = 40))]
    pub name: String,
    #[validate(url)]
    pub url: String,
}
