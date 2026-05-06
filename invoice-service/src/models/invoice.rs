use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Valid states: draft | open | paid | void | uncollectible
/// Terminal states: paid, void, uncollectible
#[derive(Debug, Serialize, FromRow)]
pub struct Invoice {
    pub id: Uuid,
    pub business_id: Uuid,
    pub customer_id: Uuid,
    pub state: String,
    pub total_cents: i64,
    pub due_date: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct LineItem {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub description: String,
    pub quantity: i32,
    pub unit_amount_cents: i64,
    pub amount_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct InvoiceResponse {
    #[serde(flatten)]
    pub invoice: Invoice,
    pub line_items: Vec<LineItem>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInvoiceRequest {
    pub customer_id: Uuid,
    pub due_date: NaiveDate,
    pub line_items: Vec<CreateLineItemRequest>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLineItemRequest {
    pub description: String,
    pub quantity: i32,
    pub unit_amount_cents: i64,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListInvoicesQuery {
    pub state: Option<String>,
}
