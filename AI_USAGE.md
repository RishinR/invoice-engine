# AI Usage Disclosure

## Tools used

**Claude (Anthropic)** — used throughout this project as the primary development assistant.

### Specifically used for:

- Drafting the initial Cargo workspace layout and dependency versions
- Generating boilerplate handler signatures and SQLx query patterns
- Reviewing the webhook delivery loop for correctness
- Drafting sections of DESIGN.md (which I then revised heavily)

---

## Three decisions I made myself, against or independent of AI suggestions

### 1. Partial unique index over application-level locking

The AI initially suggested using an in-memory `DashMap<Uuid, Mutex<()>>` to prevent concurrent payments. I rejected this because:
- It doesn't survive across multiple service replicas
- It leaks memory if invoices are never cleaned up
- It doesn't compose with the database transaction

I chose a **partial unique index** on `payment_attempts(invoice_id) WHERE status IN ('pending', 'succeeded')` instead. The database enforces the invariant — the application doesn't need to. This is safer and works under horizontal scaling with zero coordination overhead.

### 2. Fail-fast on PSP timeout instead of async/polling

The AI suggested returning `202 Accepted` for PSP timeouts and having the caller poll a status endpoint. I chose to **fail fast** with `402` and `failure_code = "psp_timeout"` instead.

Reasoning: a 202 model pushes complexity onto every API client. The invoice remains `open` after a timeout, so the caller can simply retry with a new idempotency key. Simpler contracts are easier to reason about and less likely to have bugs in the calling code.

### 3. TEXT state column instead of a Postgres ENUM

The AI generated the initial schema with a `CREATE TYPE invoice_state AS ENUM (...)`. I changed this to a plain `TEXT` column with a `CHECK` constraint.

Reasoning: adding a new state to a Postgres enum requires `ALTER TYPE ... ADD VALUE`, which in older Postgres versions cannot run inside a transaction and can be tricky in production deployments. A `TEXT` column with a `CHECK` constraint is modified with a normal `ALTER TABLE ... ALTER COLUMN ... SET CHECK` inside a transaction. The application already validates state transitions explicitly, so the database constraint is just a safety net.

---

## One thing the AI got wrong that I had to correct

The AI generated the webhook worker using a `tokio::mpsc::channel` to pass delivery jobs from API handlers to the worker. This looked clean but has a critical flaw: if the service restarts, all queued jobs in the channel are lost permanently — there's no way to drain or replay them.

I replaced this with a **database-backed polling loop**. When an event fires, we insert a `webhook_deliveries` row into Postgres. The worker polls every 5 seconds for `status = 'pending' AND next_attempt_at <= NOW()`. This survives crashes and restarts, and `FOR UPDATE SKIP LOCKED` makes it safe under multiple replicas. The correctness improvement is worth the added latency (up to 5 s before first delivery attempt).
