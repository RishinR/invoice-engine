//! Integration tests — require a running invoice-service and mock-psp.
//!
//! Run with:
//!   # terminal 1 — start postgres + mock-psp
//!   docker compose up -d db mock-psp
//!
//!   # terminal 2 — start invoice service (copy API key from output)
//!   cargo run --bin invoice-service
//!
//!   # terminal 3 — run tests
//!   TEST_API_KEY=<key from logs> cargo test --test integration_tests -- --nocapture
//!
//! The tests use the live service over HTTP so they exercise the full stack
//! including the database concurrency guarantees.

use futures::future::join_all;
use serde_json::{json, Value};
use std::env;

fn base_url() -> String {
    env::var("TEST_URL").unwrap_or_else(|_| "http://localhost:3000".into())
}

fn api_key() -> String {
    env::var("TEST_API_KEY").expect(
        "TEST_API_KEY env var must be set to the API key printed at service startup",
    )
}

fn client() -> reqwest::Client {
    reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap()
}

// ── helpers ───────────────────────────────────────────────────────────────────

async fn create_customer(client: &reqwest::Client) -> String {
    let resp = client
        .post(format!("{}/customers", base_url()))
        .bearer_auth(api_key())
        .json(&json!({ "name": "Test User", "email": format!("test+{}@example.com", uuid::Uuid::new_v4()) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

async fn create_invoice(client: &reqwest::Client, customer_id: &str) -> String {
    let resp = client
        .post(format!("{}/invoices", base_url()))
        .bearer_auth(api_key())
        .json(&json!({
            "customer_id": customer_id,
            "due_date": "2030-12-31",
            "line_items": [
                { "description": "Widget", "quantity": 2, "unit_amount_cents": 1000 }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

// ── test 1: concurrent payments ───────────────────────────────────────────────
//
// Fire 10 concurrent POST /pay requests for the same invoice with tok_success
// but different idempotency keys. Exactly one must succeed; the rest must
// receive 409 Conflict (active attempt already exists or invoice already paid).
// The final invoice state must be "paid" with no double PSP charges.

#[tokio::test]
async fn test_concurrent_payments_no_double_charge() {
    let client = client();
    let customer_id = create_customer(&client).await;
    let invoice_id = create_invoice(&client, &customer_id).await;

    let concurrency = 10_usize;
    let handles: Vec<_> = (0..concurrency)
        .map(|i| {
            let client = client.clone();
            let invoice_id = invoice_id.clone();
            tokio::spawn(async move {
                client
                    .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
                    .bearer_auth(api_key())
                    .header("Idempotency-Key", format!("concurrent-test-{i}-{invoice_id}"))
                    .json(&json!({ "card_token": "tok_success" }))
                    .send()
                    .await
                    .unwrap()
            })
        })
        .collect();

    let responses: Vec<_> = join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let successes = responses
        .iter()
        .filter(|r| r.status().as_u16() == 200)
        .count();
    let conflicts = responses
        .iter()
        .filter(|r| r.status().as_u16() == 409)
        .count();

    assert_eq!(successes, 1, "exactly one payment should succeed");
    assert_eq!(
        conflicts,
        concurrency - 1,
        "all other requests should be rejected with 409"
    );

    // Final invoice state must be paid
    let invoice: Value = client
        .get(format!("{}/invoices/{}", base_url(), invoice_id))
        .bearer_auth(api_key())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(invoice["state"], "paid", "invoice must be in paid state");
}

// ── test 2: idempotency ───────────────────────────────────────────────────────
//
// Retry the same POST /pay with the same Idempotency-Key. The second call must
// return an identical response without triggering a second PSP charge.

#[tokio::test]
async fn test_idempotent_payment() {
    let client = client();
    let customer_id = create_customer(&client).await;
    let invoice_id = create_invoice(&client, &customer_id).await;
    let idem_key = format!("idempotency-test-{invoice_id}");

    let first = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", &idem_key)
        .json(&json!({ "card_token": "tok_success" }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let first_body: Value = first.json().await.unwrap();

    // Second call — same key, same body
    let second = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", &idem_key)
        .json(&json!({ "card_token": "tok_success" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200, "idempotency retry must return 200");
    let second_body: Value = second.json().await.unwrap();

    assert_eq!(
        first_body["id"], second_body["id"],
        "both responses must refer to the same payment attempt"
    );
    assert_eq!(first_body["status"], "succeeded");
    assert_eq!(second_body["status"], "succeeded");
}

// ── test 3: idempotency key reuse with different body ─────────────────────────

#[tokio::test]
async fn test_idempotency_key_mismatch_rejected() {
    let client = client();
    let customer_id = create_customer(&client).await;
    let invoice_id = create_invoice(&client, &customer_id).await;
    let idem_key = format!("mismatch-test-{invoice_id}");

    // First call with tok_card_declined
    let first = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", &idem_key)
        .json(&json!({ "card_token": "tok_card_declined" }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 402);

    // Second call same key but different card token — must be rejected
    let second = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", &idem_key)
        .json(&json!({ "card_token": "tok_success" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        second.status(),
        422,
        "reused key with different body must return 422"
    );
}

// ── test 4: PSP timeout — invoice not stuck ───────────────────────────────────
//
// tok_timeout sleeps 30 s on the mock PSP. Our client times out at 10 s.
// The invoice must remain in "open" state (not corrupted) and the payment
// attempt must be "failed" with code "psp_timeout".

#[tokio::test]
async fn test_psp_timeout_invoice_stays_open() {
    let client = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(25)) // longer than service PSP timeout
        .build()
        .unwrap();

    let customer_id = create_customer(&client).await;
    let invoice_id = create_invoice(&client, &customer_id).await;

    // First open the invoice
    let open_resp = client
        .post(format!("{}/invoices/{}/open", base_url(), invoice_id))
        .bearer_auth(api_key())
        .send()
        .await
        .unwrap();
    assert_eq!(open_resp.status(), 200);

    let pay_resp = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", format!("timeout-test-{invoice_id}"))
        .json(&json!({ "card_token": "tok_timeout" }))
        .send()
        .await
        .unwrap();

    // Service must respond (not hang), with a failed payment attempt
    assert_eq!(
        pay_resp.status(),
        402,
        "timeout should result in a failed payment (402)"
    );
    let body: Value = pay_resp.json().await.unwrap();
    assert_eq!(body["status"], "failed");
    assert_eq!(body["failure_code"], "psp_timeout");

    // Invoice must still be open — not corrupted
    let invoice: Value = client
        .get(format!("{}/invoices/{}", base_url(), invoice_id))
        .bearer_auth(api_key())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(invoice["state"], "open", "invoice must stay open after PSP timeout");
}

// ── test 5: PSP network error ─────────────────────────────────────────────────

#[tokio::test]
async fn test_psp_network_error_invoice_stays_open() {
    let client = client();
    let customer_id = create_customer(&client).await;
    let invoice_id = create_invoice(&client, &customer_id).await;

    let pay_resp = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", format!("net-err-test-{invoice_id}"))
        .json(&json!({ "card_token": "tok_network_error" }))
        .send()
        .await
        .unwrap();

    assert_eq!(pay_resp.status(), 402);
    let body: Value = pay_resp.json().await.unwrap();
    assert_eq!(body["status"], "failed");
    assert_eq!(body["failure_code"], "network_error");

    let invoice: Value = client
        .get(format!("{}/invoices/{}", base_url(), invoice_id))
        .bearer_auth(api_key())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(invoice["state"], "open");
}

// ── test 6: invalid state transitions ────────────────────────────────────────

#[tokio::test]
async fn test_pay_already_paid_invoice_rejected() {
    let client = client();
    let customer_id = create_customer(&client).await;
    let invoice_id = create_invoice(&client, &customer_id).await;

    // Pay once (succeeds)
    client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", format!("first-pay-{invoice_id}"))
        .json(&json!({ "card_token": "tok_success" }))
        .send()
        .await
        .unwrap();

    // Pay again with a different idempotency key
    let second = client
        .post(format!("{}/invoices/{}/pay", base_url(), invoice_id))
        .bearer_auth(api_key())
        .header("Idempotency-Key", format!("second-pay-{invoice_id}"))
        .json(&json!({ "card_token": "tok_success" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409, "paying an already-paid invoice must return 409");
}
