use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::{
    auth::AuthBusiness,
    error::AppError,
    models::invoice::{CreateInvoiceRequest, Invoice, InvoiceResponse, LineItem, ListInvoicesQuery},
    state::AppState,
    webhooks,
};

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Json(req): Json<CreateInvoiceRequest>,
) -> Result<(StatusCode, Json<InvoiceResponse>), AppError> {
    if req.line_items.is_empty() {
        return Err(AppError::Unprocessable("line_items cannot be empty".into()));
    }
    for item in &req.line_items {
        if item.quantity <= 0 {
            return Err(AppError::Unprocessable("quantity must be > 0".into()));
        }
        if item.unit_amount_cents <= 0 {
            return Err(AppError::Unprocessable(
                "unit_amount_cents must be > 0".into(),
            ));
        }
    }

    // Server always computes the total — never trust client-supplied totals
    let total_cents: i64 = req
        .line_items
        .iter()
        .map(|li| li.quantity as i64 * li.unit_amount_cents)
        .sum();

    let mut tx = state.db.begin().await?;

    let invoice = sqlx::query_as::<_, Invoice>(
        r#"INSERT INTO invoices (business_id, customer_id, total_cents, due_date)
           VALUES ($1, $2, $3, $4)
           RETURNING *"#,
    )
    .bind(auth.business_id)
    .bind(req.customer_id)
    .bind(total_cents)
    .bind(req.due_date)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.code().as_deref() == Some("23503") {
                return AppError::NotFound(format!(
                    "Customer {} not found",
                    req.customer_id
                ));
            }
        }
        AppError::Database(e)
    })?;

    let mut line_items = Vec::with_capacity(req.line_items.len());
    for li in &req.line_items {
        let amount_cents = li.quantity as i64 * li.unit_amount_cents;
        let item = sqlx::query_as::<_, LineItem>(
            r#"INSERT INTO invoice_line_items
                   (invoice_id, description, quantity, unit_amount_cents, amount_cents)
               VALUES ($1, $2, $3, $4, $5)
               RETURNING *"#,
        )
        .bind(invoice.id)
        .bind(&li.description)
        .bind(li.quantity)
        .bind(li.unit_amount_cents)
        .bind(amount_cents)
        .fetch_one(&mut *tx)
        .await?;
        line_items.push(item);
    }

    tx.commit().await?;

    let payload = serde_json::json!({
        "event": "invoice.created",
        "invoice_id": invoice.id,
        "business_id": invoice.business_id,
        "state": invoice.state,
        "total_cents": invoice.total_cents,
    });
    tokio::spawn(webhooks::enqueue(
        state.db.clone(),
        auth.business_id,
        "invoice.created",
        payload,
    ));

    Ok((
        StatusCode::CREATED,
        Json(InvoiceResponse { invoice, line_items }),
    ))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Query(params): Query<ListInvoicesQuery>,
) -> Result<Json<Vec<Invoice>>, AppError> {
    let invoices = match &params.state {
        Some(s) => {
            sqlx::query_as::<_, Invoice>(
                "SELECT * FROM invoices WHERE business_id = $1 AND state = $2
                 ORDER BY created_at DESC",
            )
            .bind(auth.business_id)
            .bind(s)
            .fetch_all(&state.db)
            .await?
        }
        None => {
            sqlx::query_as::<_, Invoice>(
                "SELECT * FROM invoices WHERE business_id = $1 ORDER BY created_at DESC",
            )
            .bind(auth.business_id)
            .fetch_all(&state.db)
            .await?
        }
    };

    Ok(Json(invoices))
}

pub async fn get_by_id(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Path(id): Path<Uuid>,
) -> Result<Json<InvoiceResponse>, AppError> {
    let invoice = fetch_invoice(&state.db, id, auth.business_id).await?;
    let line_items = fetch_line_items(&state.db, id).await?;
    Ok(Json(InvoiceResponse { invoice, line_items }))
}

pub async fn open_invoice(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Path(id): Path<Uuid>,
) -> Result<Json<Invoice>, AppError> {
    let mut tx = state.db.begin().await?;
    let invoice = lock_invoice(&mut tx, id, auth.business_id).await?;

    if invoice.state != "draft" {
        return Err(AppError::Unprocessable(format!(
            "Cannot open an invoice in state '{}'",
            invoice.state
        )));
    }

    let updated = sqlx::query_as::<_, Invoice>(
        "UPDATE invoices SET state = 'open', updated_at = NOW() WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(updated))
}

pub async fn void_invoice(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Path(id): Path<Uuid>,
) -> Result<Json<Invoice>, AppError> {
    let mut tx = state.db.begin().await?;
    let invoice = lock_invoice(&mut tx, id, auth.business_id).await?;

    match invoice.state.as_str() {
        "draft" | "open" => {}
        s => {
            return Err(AppError::Unprocessable(format!(
                "Cannot void an invoice in state '{s}'"
            )))
        }
    }

    let updated = sqlx::query_as::<_, Invoice>(
        "UPDATE invoices SET state = 'void', updated_at = NOW() WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(updated))
}

pub async fn mark_uncollectible(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Path(id): Path<Uuid>,
) -> Result<Json<Invoice>, AppError> {
    let mut tx = state.db.begin().await?;
    let invoice = lock_invoice(&mut tx, id, auth.business_id).await?;

    if invoice.state != "open" {
        return Err(AppError::Unprocessable(format!(
            "Only open invoices can be marked uncollectible (current state: '{}')",
            invoice.state
        )));
    }

    let updated = sqlx::query_as::<_, Invoice>(
        "UPDATE invoices SET state = 'uncollectible', updated_at = NOW()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(updated))
}

// ── helpers ──────────────────────────────────────────────────────────────────

pub async fn fetch_invoice(
    db: &sqlx::PgPool,
    id: Uuid,
    business_id: Uuid,
) -> Result<Invoice, AppError> {
    sqlx::query_as::<_, Invoice>(
        "SELECT * FROM invoices WHERE id = $1 AND business_id = $2",
    )
    .bind(id)
    .bind(business_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Invoice {id} not found")))
}

async fn fetch_line_items(db: &sqlx::PgPool, invoice_id: Uuid) -> Result<Vec<LineItem>, AppError> {
    let items = sqlx::query_as::<_, LineItem>(
        "SELECT * FROM invoice_line_items WHERE invoice_id = $1 ORDER BY id",
    )
    .bind(invoice_id)
    .fetch_all(db)
    .await?;
    Ok(items)
}

/// SELECT FOR UPDATE — used inside an active transaction.
async fn lock_invoice(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    business_id: Uuid,
) -> Result<Invoice, AppError> {
    sqlx::query_as::<_, Invoice>(
        "SELECT * FROM invoices WHERE id = $1 AND business_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(business_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Invoice {id} not found")))
}

