use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub business_id: Uuid,
    pub url: String,
    pub secret: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWebhookEndpointRequest {
    pub url: String,
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
pub struct WebhookDeliveryRow {
    pub id: Uuid,
    pub webhook_endpoint_id: Uuid,
    pub event_type: String,
    pub payload: String,
    pub status: String,
    pub attempt_count: i32,
    pub url: String,
    pub secret: String,
}
