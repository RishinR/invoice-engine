use axum::{
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::AuthBusiness,
    error::AppError,
    models::payment::{PayInvoiceRequest, PaymentAttempt},
    psp::{self, PspResult},
    state::AppState,
    webhooks,
};

/// POST /invoices/:id/pay
///
/// Concurrency guarantee: SELECT FOR UPDATE on the invoice serialises concurrent
/// callers. The partial unique index (status IN ('pending','succeeded')) on
/// payment_attempts ensures at most one active attempt per invoice at a time —
/// the database rejects a second INSERT with a 23505 unique-violation before any
/// PSP call is made, so double-charging is impossible.
///
/// Idempotency: Idempotency-Key header is required. On retry with the same key
/// and identical body the cached attempt is returned. Reuse with a different
/// body returns 422.
pub async fn pay(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Path(invoice_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<PayInvoiceRequest>,
) -> Result<(StatusCode, Json<PaymentAttempt>), AppError> {
    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unprocessable("Idempotency-Key header is required".into()))?
        .to_string();

    let request_hash = hash_request(&req.card_token);

    // ── 1. Pre-check idempotency (no lock needed) ─────────────────────────
    if let Some(existing) = find_attempt(&state.db, &idempotency_key).await? {
        return idempotency_response(existing, &request_hash);
    }

    // ── 2. Open transaction, lock invoice ─────────────────────────────────
    let mut tx = state.db.begin().await?;

    let invoice = sqlx::query_as::<_, crate::models::invoice::Invoice>(
        "SELECT * FROM invoices WHERE id = $1 AND business_id = $2 FOR UPDATE",
    )
    .bind(invoice_id)
    .bind(auth.business_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Invoice {invoice_id} not found")))?;

    // ── 3. Validate state machine transition ──────────────────────────────
    match invoice.state.as_str() {
        "draft" | "open" => {}
        "paid" => {
            return Err(AppError::Conflict("Invoice is already paid".into()));
        }
        s => {
            return Err(AppError::Unprocessable(format!(
                "Cannot pay an invoice in state '{s}'"
            )));
        }
    }

    // draft → open automatically on first pay attempt
    if invoice.state == "draft" {
        sqlx::query(
            "UPDATE invoices SET state = 'open', updated_at = NOW() WHERE id = $1",
        )
        .bind(invoice_id)
        .execute(&mut *tx)
        .await?;
    }

    // ── 4. Insert pending attempt (unique indexes guard against races) ─────
    let attempt = sqlx::query_as::<_, PaymentAttempt>(
        r#"INSERT INTO payment_attempts
               (invoice_id, idempotency_key, card_token, status, request_hash)
           VALUES ($1, $2, $3, 'pending', $4)
           RETURNING *"#,
    )
    .bind(invoice_id)
    .bind(&idempotency_key)
    .bind(&req.card_token)
    .bind(&request_hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.code().as_deref() == Some("23505") {
                // Either duplicate idempotency_key (race) or active attempt exists
                return AppError::Conflict(
                    "Another payment attempt is already active for this invoice".into(),
                );
            }
        }
        AppError::Database(e)
    })?;

    tx.commit().await?; // releases invoice row lock before PSP call

    // ── 5. Call PSP (client already has 10 s timeout) ─────────────────────
    let psp_result = psp::charge(&state.http, &state.config.psp_url, &req.card_token).await;

    // ── 6. Persist outcome, transition invoice ─────────────────────────────
    let attempt_id = attempt.id;
    let (attempt_status, psp_ref, failure_code, new_invoice_state) = match psp_result {
        PspResult::Succeeded { psp_ref } => {
            ("succeeded", Some(psp_ref), None, Some("paid"))
        }
        PspResult::Failed { code } => ("failed", None, Some(code), None),
        PspResult::Timeout => ("failed", None, Some("psp_timeout".to_string()), None),
        PspResult::NetworkError(msg) => {
            tracing::warn!(error = %msg, "PSP network error");
            ("failed", None, Some("network_error".to_string()), None)
        }
    };

    let mut tx2 = state.db.begin().await?;

    let updated_attempt = sqlx::query_as::<_, PaymentAttempt>(
        r#"UPDATE payment_attempts
           SET status = $1, psp_ref = $2, failure_code = $3, updated_at = NOW()
           WHERE id = $4
           RETURNING *"#,
    )
    .bind(attempt_status)
    .bind(&psp_ref)
    .bind(&failure_code)
    .bind(attempt_id)
    .fetch_one(&mut *tx2)
    .await?;

    if let Some(new_state) = new_invoice_state {
        // Conditional update: only move to 'paid' if still 'open'
        // (guards against a second concurrent success in an edge case)
        sqlx::query(
            "UPDATE invoices SET state = $1, updated_at = NOW()
             WHERE id = $2 AND state = 'open'",
        )
        .bind(new_state)
        .bind(invoice_id)
        .execute(&mut *tx2)
        .await?;
    }

    tx2.commit().await?;

    // ── 7. Enqueue webhook (non-blocking) ──────────────────────────────────
    let event_type = if attempt_status == "succeeded" {
        "invoice.paid"
    } else {
        "invoice.payment_failed"
    };

    let event_payload = serde_json::json!({
        "event": event_type,
        "invoice_id": invoice_id,
        "payment_attempt_id": attempt_id,
        "status": attempt_status,
        "failure_code": failure_code,
    });
    tokio::spawn(webhooks::enqueue(
        state.db.clone(),
        auth.business_id,
        event_type,
        event_payload,
    ));

    let status_code = if attempt_status == "succeeded" {
        StatusCode::OK
    } else {
        StatusCode::PAYMENT_REQUIRED
    };

    Ok((status_code, Json(updated_attempt)))
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn hash_request(card_token: &str) -> String {
    let mut h = Sha256::new();
    h.update(card_token.as_bytes());
    hex::encode(h.finalize())
}

async fn find_attempt(
    db: &sqlx::PgPool,
    idempotency_key: &str,
) -> Result<Option<PaymentAttempt>, AppError> {
    let attempt = sqlx::query_as::<_, PaymentAttempt>(
        "SELECT * FROM payment_attempts WHERE idempotency_key = $1",
    )
    .bind(idempotency_key)
    .fetch_optional(db)
    .await?;
    Ok(attempt)
}

fn idempotency_response(
    attempt: PaymentAttempt,
    request_hash: &str,
) -> Result<(StatusCode, Json<PaymentAttempt>), AppError> {
    if attempt.request_hash != request_hash {
        return Err(AppError::Unprocessable(
            "Idempotency-Key reused with a different request body".into(),
        ));
    }
    let status = if attempt.status == "succeeded" {
        StatusCode::OK
    } else {
        StatusCode::PAYMENT_REQUIRED
    };
    Ok((status, Json(attempt)))
}
