use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct PaymentAttempt {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub idempotency_key: String,
    pub card_token: String,
    pub status: String,
    pub psp_ref: Option<String>,
    pub failure_code: Option<String>,
    #[serde(skip)]
    pub request_hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct PayInvoiceRequest {
    pub card_token: String,
}
