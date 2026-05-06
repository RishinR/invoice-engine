# Design Document — Invoice & Payment Service

## 1. Data Model

### Tables

```
businesses
  id          UUID PK
  name        TEXT
  created_at  TIMESTAMPTZ

api_keys
  id          UUID PK
  business_id UUID FK → businesses
  key_prefix  TEXT          -- first 11 chars, for display only
  key_hash    TEXT UNIQUE   -- SHA-256 of the full raw key
  revoked_at  TIMESTAMPTZ   -- NULL = active
  created_at  TIMESTAMPTZ

customers
  id          UUID PK
  business_id UUID FK → businesses
  name        TEXT
  email       TEXT
  created_at  TIMESTAMPTZ
  UNIQUE(business_id, email)

invoices
  id          UUID PK
  business_id UUID FK → businesses
  customer_id UUID FK → customers
  state       TEXT          -- draft | open | paid | void | uncollectible
  total_cents BIGINT        -- server-computed, never client-supplied
  due_date    DATE
  created_at  TIMESTAMPTZ
  updated_at  TIMESTAMPTZ

invoice_line_items
  id                UUID PK
  invoice_id        UUID FK → invoices
  description       TEXT
  quantity          INTEGER
  unit_amount_cents BIGINT
  amount_cents      BIGINT  -- quantity × unit_amount_cents, computed at insert

payment_attempts
  id              UUID PK
  invoice_id      UUID FK → invoices
  idempotency_key TEXT UNIQUE
  card_token      TEXT
  status          TEXT      -- pending | succeeded | failed
  psp_ref         TEXT NULL
  failure_code    TEXT NULL
  request_hash    TEXT      -- SHA-256(card_token), for idempotency body check
  created_at      TIMESTAMPTZ
  updated_at      TIMESTAMPTZ

webhook_endpoints
  id          UUID PK
  business_id UUID FK → businesses
  url         TEXT
  secret      TEXT          -- 32-char random string, used for HMAC signing
  active      BOOLEAN
  created_at  TIMESTAMPTZ

webhook_deliveries
  id                  UUID PK
  webhook_endpoint_id UUID FK → webhook_endpoints
  event_type          TEXT
  payload             TEXT  -- JSON string
  status              TEXT  -- pending | delivered | permanently_failed
  attempt_count       INTEGER
  next_attempt_at     TIMESTAMPTZ
  last_attempt_at     TIMESTAMPTZ NULL
  last_error          TEXT NULL
  created_at          TIMESTAMPTZ
```

### Key indexes

- `api_keys(key_hash)` — O(1) auth lookup on every request
- `invoices(business_id, state)` — list-by-state query
- `payment_attempts(invoice_id) WHERE status IN ('pending','succeeded')` — **partial unique index** that enforces at most one active attempt per invoice (this is the concurrency guard)
- `webhook_deliveries(status, next_attempt_at) WHERE status = 'pending'` — efficient polling by the webhook worker

### Why this shape vs. alternatives

- **Integer cents (`BIGINT`)** everywhere in the money path. PostgreSQL NUMERIC would also work, but BIGINT is faster and the constraint (single currency USD) makes it unnecessary. Floats are never used — IEEE-754 rounding would silently corrupt totals.
- **`total_cents` stored on invoice** rather than computed at query time. Denormalization here is intentional: line items could theoretically be amended, and having a stable `total_cents` is safer than re-aggregating on every read.
- **`state` as TEXT not an enum**: makes schema migrations (adding states) a one-line CHECK constraint change instead of a Postgres `ALTER TYPE`, which requires downtime.

### 100× scale changes

- Shard `invoices` and `payment_attempts` by `business_id`. UUIDs as PKs make this straightforward.
- Archive old payments to a separate cold table. Most queries are on recent data.
- Add a read replica for list/get endpoints; writes go to the primary.
- Consider partitioning `webhook_deliveries` by `created_at` and dropping old partitions.

---

## 2. Invoice State Machine

```
         create
           │
           ▼
        [DRAFT] ──── void() ─────────────────────────────► [VOID] ◄─── void()
           │                                                               ▲
           │ open() or pay()                                               │
           ▼                                                               │
         [OPEN] ──── void() ────────────────────────────────────────────── ┘
           │  └──── mark_uncollectible() ──────────────► [UNCOLLECTIBLE]
           │
           │ payment_succeeded
           ▼
         [PAID]
```

| Transition | Trigger | Reversible? |
|---|---|---|
| draft → open | `POST /invoices/:id/open` or first `POST /invoices/:id/pay` | No |
| draft → void | `POST /invoices/:id/void` | No |
| open → paid | PSP returns `succeeded` | No |
| open → void | `POST /invoices/:id/void` | No |
| open → uncollectible | `POST /invoices/:id/mark-uncollectible` | No |

**Terminal states**: `paid`, `void`, `uncollectible`. No transitions out.

**Rejection of invalid transitions**: the handler reads the current state inside a `SELECT FOR UPDATE` transaction and returns `422 Unprocessable Entity` or `409 Conflict` with a structured error body before touching the database further.

---

## 3. Payment Correctness & Failure Modes

### Concurrency mechanism

The service uses **row-level locking** (`SELECT … FOR UPDATE`) on the invoice row plus a **partial unique index** on `payment_attempts(invoice_id) WHERE status IN ('pending', 'succeeded')`.

The index is the real guard: even if two requests pass the lock check simultaneously (they can't — `FOR UPDATE` serialises them — but assuming they could), only one INSERT into `payment_attempts` succeeds. The other gets a PostgreSQL `23505` unique-violation and returns `409` before any PSP call is ever made. **No double charge is possible.**

### (a) Two concurrent POST /pay requests for the same invoice

Request A and B arrive at the same moment.

1. A acquires the row lock on the invoice (B waits).
2. A inserts a `pending` payment attempt.
3. A commits, releasing the lock. B acquires it.
4. B tries to insert a `pending` attempt — the partial unique index rejects it with `23505`.
5. B returns `409 Conflict` without calling the PSP.

Outcome: exactly one PSP call, at most one charge.

### (b) PSP timeout (tok_timeout, 30 s)

The `reqwest` client is constructed with a **10-second timeout**. `tok_timeout` sleeps 30 s on the mock PSP, so our timeout fires at 10 s.

Flow:
1. The `pending` payment attempt is committed to the database before the PSP call.
2. After 10 s, `reqwest` returns `err.is_timeout() == true`.
3. The attempt status is updated to `failed`, `failure_code = "psp_timeout"`.
4. The invoice remains in `open` state — it can be retried.
5. The endpoint returns `402 Payment Required` with the failed attempt body.

The caller learns the result immediately (synchronous fail-fast). They can retry with a new `Idempotency-Key`.

### (c) PSP returns success but service crashes before persisting

The `pending` attempt exists in the database. On crash, it's stuck in `pending`.

On customer retry with a **different** idempotency key: a second PSP call would be made. This is the "at-least-once charge" risk. Mitigations in production:
- Use a PSP that supports idempotency keys on the charge request (e.g., Stripe's `idempotencyKey` header). Pass our `payment_attempt.id` as the PSP idempotency key. On retry, the PSP deduplicates automatically.
- A background cleanup job finds `pending` attempts older than 2× the PSP timeout and either marks them `failed` or queries the PSP for the outcome.

For this implementation, I chose the simpler path (no PSP-level idempotency key) and documented it here as a known gap.

### (d) Idempotency key reused with a different request body

The service stores `SHA-256(card_token)` as `request_hash` on each attempt. On a retry, if the incoming `request_hash` differs from the stored one, the endpoint returns `422 Unprocessable Entity` with code `invalid_request`. The existing attempt is not modified.

### (e) POST /pay on an already-paid invoice

After the invoice transitions to `paid`, any new `POST /pay` (with a different idempotency key) hits the `SELECT FOR UPDATE` check and sees `state = 'paid'`. The handler returns `409 Conflict` before creating an attempt or calling the PSP.

---

## 4. Webhook Design

### Signing

Each `webhook_endpoint` has a 32-character random `secret` returned only at registration time.

For each delivery:
```
Dodo-Timestamp: <unix seconds>
Dodo-Signature: v1=<HMAC-SHA256(secret, "<timestamp>.<json_body>")>
```

Receivers verify by recomputing the HMAC and comparing. The timestamp is embedded in the signed string, so replaying an old request to the same endpoint with a stale timestamp is detectable — receivers should reject deliveries older than, say, 5 minutes.

### Retry policy

| Attempt | Delay after failure |
|---|---|
| 1 | immediate |
| 2 | 30 seconds |
| 3 | 5 minutes |
| 4 | 30 minutes |
| 5 | 2 hours |
| after 5 | `permanently_failed` |

Total budget: ~2.5 hours from first attempt.

### Permanently failed webhooks

After 5 attempts, `status` is set to `permanently_failed`. The business can:
- List all their webhook deliveries (future endpoint) to find missed events.
- Re-register their endpoint and re-fire events from their own event log (not built here, discussed in §6).

### Decoupled from API response

Webhook delivery is **not** in the request path. After the invoice state update commits, `tokio::spawn` enqueues the delivery row into the database and returns immediately. A background Tokio task (`webhooks::worker`) polls `webhook_deliveries WHERE status = 'pending' AND next_attempt_at <= NOW()` every 5 seconds using `SELECT … FOR UPDATE SKIP LOCKED`. This pattern is safe under multiple replicas — each instance only picks up rows it exclusively locked.

---

## 5. API Key Model

**Generation**: `dk_` prefix + 48 random alphanumeric characters = 51-character key. `rand::thread_rng()` is cryptographically suitable on all supported platforms.

**Storage**: Only `SHA-256(raw_key)` is stored. The raw key is logged once at creation and never persisted. `key_prefix` (first 11 chars) is stored for human identification without revealing the secret.

**Transmission**: `Authorization: Bearer <raw_key>` header over TLS. HTTP-only deployments should be blocked at the load-balancer level.

**Rotation**: Create a new key, update callers, then revoke the old one by setting `revoked_at`. Both keys are valid during the transition window.

**Revocation**: `UPDATE api_keys SET revoked_at = NOW() WHERE id = $1`. Auth middleware checks `revoked_at IS NULL`. The row is kept for audit purposes.

**Blast radius if leaked**: Scoped to one business only. No cross-business access is possible. Revocation takes effect on the next request (no caching layer in this implementation). Production hardening: add a short TTL cache (e.g., 60 s) to reduce DB load, accepting a 60-second revocation lag.

---

## 6. What You Cut and Why

1. **Subscriptions / recurring billing** — out of scope per spec. Would require a scheduler, plan management, and proration logic that would dwarf the core service.
2. **Refunds** — would add `REFUNDED` and `PARTIALLY_REFUNDED` states plus a PSP refund call. The state machine and DB schema are designed to accommodate this (no structural changes needed), but the feature itself was not built. I'd add a `refund_amount_cents` column to `invoices` and a `POST /invoices/:id/refund` endpoint.
3. **Rate limiting** — I'd put this at the API gateway level (e.g., Kong or a Nginx `limit_req` zone keyed on `business_id` extracted from the JWT/API key). Doing it in-process with a sliding window per-key map works but is not shared across replicas.
4. **An event-sourced audit log** — every state transition should append to an immutable `invoice_events` table. This enables the business to reconstruct missed webhooks and provides a full audit trail. I documented it but did not build it to stay within scope.
5. **Multi-currency / FX** — single-currency USD by design. Adding this would require a `currency` column on invoices and a rates service, with all amount comparisons guarded by currency equality checks.

---

## 7. Production Readiness Gap

Top three gaps if this shipped tomorrow:

1. **Observability** — No structured metrics (Prometheus counters for payment outcomes, webhook delivery rates, PSP latency histograms). No distributed tracing (OpenTelemetry spans). The `tracing` crate is wired up for logs, but metrics and traces are missing. On-call would be flying blind.

2. **PSP-level idempotency** — If the service crashes between inserting the `pending` attempt and persisting the PSP result, a retry with a different idempotency key issues a second charge. The fix is to pass `payment_attempt.id` as the PSP's idempotency key, which requires the mock PSP to support it and a reconciliation job to poll PSP outcomes for stuck `pending` attempts.

3. **Secret management** — `DATABASE_URL` and the webhook signing secrets are passed as environment variables. In production these should come from a secrets manager (AWS Secrets Manager, Vault, etc.) with rotation support. API key hashes should be stored in a table that's encrypted at rest.
