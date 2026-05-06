CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE businesses (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- API keys: hashed storage, prefix for identification
CREATE TABLE api_keys (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id UUID        NOT NULL REFERENCES businesses(id) ON DELETE CASCADE,
    key_prefix  TEXT        NOT NULL,
    key_hash    TEXT        NOT NULL UNIQUE,
    revoked_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_api_keys_hash     ON api_keys(key_hash);
CREATE INDEX idx_api_keys_business ON api_keys(business_id);

CREATE TABLE customers (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id UUID        NOT NULL REFERENCES businesses(id) ON DELETE CASCADE,
    name        TEXT        NOT NULL,
    email       TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(business_id, email)
);
CREATE INDEX idx_customers_business ON customers(business_id);

-- states: draft | open | paid | void | uncollectible
CREATE TABLE invoices (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id UUID        NOT NULL REFERENCES businesses(id) ON DELETE CASCADE,
    customer_id UUID        NOT NULL REFERENCES customers(id),
    state       TEXT        NOT NULL DEFAULT 'draft',
    total_cents BIGINT      NOT NULL CHECK (total_cents >= 0),
    due_date    DATE        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_invoices_business ON invoices(business_id);
CREATE INDEX idx_invoices_state    ON invoices(business_id, state);
CREATE INDEX idx_invoices_customer ON invoices(customer_id);

CREATE TABLE invoice_line_items (
    id                UUID    PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id        UUID    NOT NULL REFERENCES invoices(id) ON DELETE CASCADE,
    description       TEXT    NOT NULL,
    quantity          INTEGER NOT NULL CHECK (quantity > 0),
    unit_amount_cents BIGINT  NOT NULL CHECK (unit_amount_cents > 0),
    amount_cents      BIGINT  NOT NULL CHECK (amount_cents > 0)
);
CREATE INDEX idx_line_items_invoice ON invoice_line_items(invoice_id);

-- status: pending | succeeded | failed
-- idempotency_key is unique per attempt (client-supplied)
-- partial unique index: at most one non-failed attempt per invoice at a time
-- this is the key mechanism that prevents double-charging
CREATE TABLE payment_attempts (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id      UUID        NOT NULL REFERENCES invoices(id),
    idempotency_key TEXT        NOT NULL UNIQUE,
    card_token      TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending',
    psp_ref         TEXT,
    failure_code    TEXT,
    request_hash    TEXT        NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Prevents concurrent active payment attempts for the same invoice
CREATE UNIQUE INDEX idx_one_active_attempt
    ON payment_attempts(invoice_id)
    WHERE status IN ('pending', 'succeeded');

CREATE INDEX idx_payment_attempts_invoice ON payment_attempts(invoice_id);

CREATE TABLE webhook_endpoints (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    business_id UUID        NOT NULL REFERENCES businesses(id) ON DELETE CASCADE,
    url         TEXT        NOT NULL,
    secret      TEXT        NOT NULL,
    active      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_webhook_endpoints_business ON webhook_endpoints(business_id);

-- status: pending | delivered | permanently_failed
CREATE TABLE webhook_deliveries (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    webhook_endpoint_id UUID        NOT NULL REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    event_type          TEXT        NOT NULL,
    payload             TEXT        NOT NULL,
    status              TEXT        NOT NULL DEFAULT 'pending',
    attempt_count       INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_attempt_at     TIMESTAMPTZ,
    last_error          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_webhook_deliveries_pending
    ON webhook_deliveries(status, next_attempt_at)
    WHERE status = 'pending';
CREATE INDEX idx_webhook_deliveries_endpoint ON webhook_deliveries(webhook_endpoint_id);
