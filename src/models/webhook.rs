//! Webhook (integration) domain models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// An outgoing webhook subscription. When matching events occur, the engine
/// POSTs a signed JSON payload to `target_url`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub owner_id: Uuid,
    /// Optional channel scope; `None` means instance-wide.
    pub channel_id: Option<Uuid>,
    pub target_url: String,
    /// Events to deliver, e.g. `["message.created","channel.created"]`.
    pub events: Vec<String>,
    /// HMAC secret used to sign deliveries (`X-Signature` header).
    #[serde(skip_serializing)]
    pub secret: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateWebhookRequest {
    #[validate(url)]
    pub target_url: String,
    #[validate(length(min = 1))]
    pub events: Vec<String>,
    pub channel_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct CreateWebhookResponse {
    pub id: Uuid,
    pub target_url: String,
    pub events: Vec<String>,
    /// Returned once at creation time so the owner can verify signatures.
    pub secret: String,
}

/// The JSON envelope delivered to webhook subscribers.
#[derive(Debug, Serialize)]
pub struct WebhookDelivery<'a, T: Serialize> {
    pub event: &'a str,
    pub timestamp: DateTime<Utc>,
    pub data: &'a T,
}
