# Invoice & Payment Service

A minimal invoice and payment service built in Rust (Axum + SQLx + PostgreSQL).

## Demo Video

[Demo video](https://drive.google.com/file/d/1qJ-EKipCGS9aN5ZJMCLmuqxAtaG1ouJp/view?usp=sharing)

---

## Quick Start — Docker (recommended)

**Prerequisites:** Docker Desktop running.

```bash
docker compose up --build
```

This single command starts PostgreSQL, the mock PSP, and the invoice service — no manual steps. On first run it builds the Rust binaries (~4–6 minutes), then everything is up on port `3000`.

**Get your API key from the service logs:**

```bash
docker compose logs invoice-service | grep "dk_"
```

Output looks like:

```
Demo business created. Your API key:
  dk_AbCdEf...
Use as: Authorization: Bearer dk_AbCdEf...
This key is shown only once.
```

Set it for the curl examples below:

```bash
export API_KEY=dk_...
```

> Subsequent `docker compose up` runs skip seeding (key was already created). If you need a fresh key, run `docker compose down -v` to wipe the volume and start over.

---

## curl Examples

Run these against `http://localhost:3000` after `docker compose up`.

### 1. Create a customer

```bash
curl -s -X POST http://localhost:3000/customers \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "Acme Corp", "email": "billing@acme.com"}' | jq
```

### 2. Create an invoice

```bash
CUSTOMER_ID="<id from step 1>"

curl -s -X POST http://localhost:3000/invoices \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d "{
    \"customer_id\": \"$CUSTOMER_ID\",
    \"due_date\": \"2025-12-31\",
    \"line_items\": [
      {\"description\": \"Widget Pro\", \"quantity\": 3, \"unit_amount_cents\": 5000},
      {\"description\": \"Support plan\", \"quantity\": 1, \"unit_amount_cents\": 10000}
    ]
  }" | jq
```

The server computes `total_cents = 25000` (3×$50 + 1×$100). Client-supplied totals are not accepted.

### 3. Attempt a successful payment

```bash
INVOICE_ID="<id from step 2>"

curl -s -X POST http://localhost:3000/invoices/$INVOICE_ID/pay \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: $(uuidgen)" \
  -d '{"card_token": "tok_success"}' | jq
```

### 4. Attempt a failing payment (card declined)

```bash
# Create a fresh invoice first (the previous one is now paid)
INVOICE_ID2="<id of a new invoice>"

curl -s -X POST http://localhost:3000/invoices/$INVOICE_ID2/pay \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: $(uuidgen)" \
  -d '{"card_token": "tok_card_declined"}' | jq
```

---

## Mock PSP tokens

| Token | Behaviour |
|---|---|
| `tok_success` | Succeeds after ~100 ms |
| `tok_insufficient_funds` | Fails — `insufficient_funds` after ~100 ms |
| `tok_card_declined` | Fails — `card_declined` after ~100 ms |
| `tok_timeout` | Sleeps 30 s on the PSP — service client times out at 10 s, returns `psp_timeout`, invoice stays `open` |
| `tok_network_error` | PSP returns HTTP 500 — service returns `network_error`, invoice stays `open` |

---

## Running the tests

Integration tests require the full stack to be running. The simplest way:

```bash
# Terminal 1 — start everything
docker compose up --build

# In another terminal — get the API key
export TEST_API_KEY=$(docker compose logs invoice-service | grep "dk_" | grep -o 'dk_[A-Za-z0-9]*')

# Run all tests
TEST_API_KEY=$TEST_API_KEY cargo test --test integration_tests -- --nocapture
```

Or run against local processes (faster iteration):

```bash
# Requires: local Postgres, mock-psp running (cargo run --bin mock-psp)
# and invoice-service running (cargo run --bin invoice-service)
TEST_API_KEY=dk_... cargo test --test integration_tests -- --nocapture
```

> **The timeout test** (`test_psp_timeout_invoice_stays_open`) takes ~10 s — it waits for the PSP client to time out. This is expected.

### Required test coverage

| Test | What it verifies |
|---|---|
| `test_concurrent_payments_no_double_charge` | 10 concurrent `/pay` calls on same invoice → exactly 1 succeeds, 9 get 409, final state is `paid` |
| `test_idempotent_payment` | Same `Idempotency-Key` + body retried → identical attempt ID returned, no second PSP call |
| `test_idempotency_key_mismatch_rejected` | Same key, different `card_token` → 422 |
| `test_psp_timeout_invoice_stays_open` | `tok_timeout` → endpoint returns in ~10 s, `failure_code: psp_timeout`, invoice stays `open` |
| `test_psp_network_error_invoice_stays_open` | `tok_network_error` → `failure_code: network_error`, invoice stays `open` |
| `test_pay_already_paid_invoice_rejected` | Second `/pay` on a paid invoice → 409 |

---

## Local Development (alternative)

If you prefer to run services directly without Docker:

**Prerequisites:** Rust ≥ 1.82, a local PostgreSQL instance.

```bash
# 1. Create the database
createdb invoice_engine

# 2. Copy and edit the environment file
cp .env.example .env

# 3. Terminal A — start mock PSP
cargo run --bin mock-psp

# 4. Terminal B — start invoice service (prints API key on first run)
cargo run --bin invoice-service
```

---

## Project structure

```
invoice-engine/
├── invoice-service/        # Main API (Axum + SQLx)
│   ├── src/
│   │   ├── main.rs         # Startup, DB migration, seeding
│   │   ├── auth.rs         # API key middleware (SHA-256 hash lookup)
│   │   ├── psp.rs          # PSP HTTP client with timeout
│   │   ├── webhooks.rs     # Background delivery worker (DB-polled)
│   │   ├── models/         # Data types (all money as i64 cents)
│   │   └── routes/         # HTTP handlers
│   └── tests/
│       └── integration_tests.rs
├── mock-psp/               # Standalone mock PSP (Axum)
├── migrations/
│   └── 001_init.sql        # Schema + indexes (embedded into binary)
├── docker-compose.yml
├── Dockerfile.invoice-service
├── Dockerfile.mock-psp
├── DESIGN.md               # Primary deliverable
├── AI_USAGE.md
└── openapi.yaml
```
