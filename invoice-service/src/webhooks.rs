use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::webhook::WebhookDeliveryRow;

type HmacSha256 = Hmac<Sha256>;

pub async fn enqueue(
    db: PgPool,
    business_id: Uuid,
    event_type: &str,
    payload: serde_json::Value,
) {
    let endpoints_result = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM webhook_endpoints WHERE business_id = $1 AND active = true",
    )
    .bind(business_id)
    .fetch_all(&db)
    .await;

    let endpoints = match endpoints_result {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "failed to fetch webhook endpoints for enqueue");
            return;
        }
    };

    let payload_str = payload.to_string();
    for (endpoint_id,) in endpoints {
        let result = sqlx::query(
            "INSERT INTO webhook_deliveries (webhook_endpoint_id, event_type, payload)
             VALUES ($1, $2, $3)",
        )
        .bind(endpoint_id)
        .bind(event_type)
        .bind(&payload_str)
        .execute(&db)
        .await;

        if let Err(e) = result {
            tracing::error!(error = %e, endpoint_id = %endpoint_id, "failed to enqueue webhook");
        }
    }
}

pub async fn worker(db: PgPool, http: reqwest::Client) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        interval.tick().await;
        if let Err(e) = process_pending(&db, &http).await {
            tracing::error!(error = %e, "webhook worker error");
        }
    }
}

async fn process_pending(db: &PgPool, http: &reqwest::Client) -> Result<(), sqlx::Error> {
    let deliveries = sqlx::query_as::<_, WebhookDeliveryRow>(
        r#"SELECT wd.id, wd.webhook_endpoint_id, wd.event_type, wd.payload,
                  wd.status, wd.attempt_count, we.url, we.secret
           FROM webhook_deliveries wd
           JOIN webhook_endpoints  we ON we.id = wd.webhook_endpoint_id
           WHERE wd.status = 'pending'
             AND wd.next_attempt_at <= NOW()
             AND we.active = true
           LIMIT 20
           FOR UPDATE OF wd SKIP LOCKED"#,
    )
    .fetch_all(db)
    .await?;

    for delivery in deliveries {
        let db2 = db.clone();
        let http2 = http.clone();
        tokio::spawn(async move {
            deliver(db2, http2, delivery).await;
        });
    }
    Ok(())
}

async fn deliver(db: PgPool, http: reqwest::Client, delivery: WebhookDeliveryRow) {
    let timestamp = Utc::now().timestamp();
    let signature = sign(&delivery.secret, timestamp, &delivery.payload);

    let result = http
        .post(&delivery.url)
        .header("Content-Type", "application/json")
        .header("Dodo-Timestamp", timestamp.to_string())
        .header("Dodo-Signature", format!("v1={signature}"))
        .body(delivery.payload.clone())
        .send()
        .await;

    let (new_status, last_error, next_at) =
        match result.and_then(|r| r.error_for_status()) {
            Ok(_) => ("delivered".to_string(), None, None),
            Err(e) => {
                let next_attempt = delivery.attempt_count + 1;
                let next = backoff_delay(next_attempt);
                let status = if next.is_some() {
                    "pending".to_string()
                } else {
                    tracing::warn!(
                        delivery_id = %delivery.id,
                        "webhook exhausted all retries"
                    );
                    "permanently_failed".to_string()
                };
                (status, Some(e.to_string()), next)
            }
        };

    let _ = sqlx::query(
        r#"UPDATE webhook_deliveries
           SET status          = $1,
               last_error      = $2,
               next_attempt_at = COALESCE($3, next_attempt_at),
               last_attempt_at = NOW(),
               attempt_count   = attempt_count + 1
           WHERE id = $4"#,
    )
    .bind(&new_status)
    .bind(&last_error)
    .bind(next_at)
    .bind(delivery.id)
    .execute(&db)
    .await
    .map_err(|e| tracing::error!(error = %e, "failed to update webhook delivery"));
}

/// Backoff schedule: 30s → 5m → 30m → 2h → give up
fn backoff_delay(attempt_count: i32) -> Option<chrono::DateTime<Utc>> {
    let secs: Option<i64> = match attempt_count {
        1 => Some(30),
        2 => Some(300),
        3 => Some(1800),
        4 => Some(7200),
        _ => None,
    };
    secs.map(|s| Utc::now() + Duration::seconds(s))
}

fn sign(secret: &str, timestamp: i64, body: &str) -> String {
    let message = format!("{timestamp}.{body}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC key length is always valid");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
