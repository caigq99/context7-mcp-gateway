CREATE TABLE recharge_orders (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    quote_id TEXT NOT NULL,
    quote_version INTEGER NOT NULL,
    units INTEGER NOT NULL CHECK (units > 0),
    valid_days INTEGER NOT NULL CHECK (valid_days > 0),
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    currency TEXT NOT NULL CHECK (length(currency) = 3),
    provider TEXT NOT NULL,
    merchant_id TEXT NOT NULL,
    created_unix INTEGER NOT NULL DEFAULT (unixepoch()),
    expires_unix INTEGER NOT NULL CHECK (expires_unix > created_unix),
    settled_unix INTEGER,
    cancelled_unix INTEGER,
    CHECK (settled_unix IS NULL OR cancelled_unix IS NULL)
);

CREATE INDEX recharge_orders_user_time ON recharge_orders(user_id, created_unix DESC, id DESC);
CREATE INDEX recharge_orders_time ON recharge_orders(created_unix DESC, id DESC);

CREATE TABLE payment_events (
    provider TEXT NOT NULL,
    event_id TEXT NOT NULL,
    body_digest BLOB NOT NULL CHECK (length(body_digest) = 32),
    order_id TEXT NOT NULL REFERENCES recharge_orders(id),
    received_unix INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (provider, event_id)
) WITHOUT ROWID;

CREATE TABLE redemption_codes (
    id TEXT PRIMARY KEY NOT NULL,
    digest BLOB NOT NULL UNIQUE CHECK (length(digest) = 32),
    units INTEGER NOT NULL CHECK (units > 0),
    valid_days INTEGER NOT NULL CHECK (valid_days > 0),
    expires_unix INTEGER NOT NULL,
    redeemed_user_id TEXT REFERENCES users(id),
    redeemed_unix INTEGER,
    CHECK ((redeemed_user_id IS NULL) = (redeemed_unix IS NULL))
);

CREATE TABLE credit_lots (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    source_type TEXT NOT NULL CHECK (source_type IN ('order', 'redemption', 'manual')),
    source_id TEXT NOT NULL,
    expires_unix INTEGER NOT NULL,
    UNIQUE (source_type, source_id)
);

CREATE INDEX credit_lots_user_expiry ON credit_lots(user_id, expires_unix);

CREATE TABLE quota_ledger (
    id TEXT PRIMARY KEY NOT NULL,
    lot_id TEXT NOT NULL REFERENCES credit_lots(id),
    kind TEXT NOT NULL CHECK (kind IN ('grant', 'consume', 'reverse', 'manual')),
    reference_id TEXT NOT NULL,
    units INTEGER NOT NULL CHECK ((kind = 'grant' AND units > 0) OR (kind IN ('consume','reverse') AND units < 0) OR (kind = 'manual' AND units != 0)),
    created_unix INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (lot_id, kind, reference_id)
);

CREATE INDEX quota_ledger_lot ON quota_ledger(lot_id);

CREATE TABLE quota_adjustments (
    idempotency_key TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    actor TEXT NOT NULL,
    units INTEGER NOT NULL CHECK (units != 0),
    valid_days INTEGER NOT NULL CHECK (valid_days >= 0),
    reason TEXT NOT NULL,
    created_unix INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TRIGGER quota_adjustments_no_update BEFORE UPDATE ON quota_adjustments
BEGIN
    SELECT RAISE(ABORT, 'quota adjustments are immutable');
END;

CREATE TRIGGER quota_adjustments_no_delete BEFORE DELETE ON quota_adjustments
BEGIN
    SELECT RAISE(ABORT, 'quota adjustments are immutable');
END;

CREATE TRIGGER quota_ledger_no_update BEFORE UPDATE ON quota_ledger
BEGIN
    SELECT RAISE(ABORT, 'quota ledger is append-only');
END;

CREATE TRIGGER quota_ledger_no_delete BEFORE DELETE ON quota_ledger
BEGIN
    SELECT RAISE(ABORT, 'quota ledger is append-only');
END;

CREATE TRIGGER recharge_orders_final_immutable BEFORE UPDATE ON recharge_orders
WHEN OLD.settled_unix IS NOT NULL OR OLD.cancelled_unix IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'a final order is immutable');
END;

CREATE TABLE quota_reservation_allocations (
    reservation_id TEXT NOT NULL REFERENCES quota_reservations(id) ON DELETE CASCADE,
    lot_id TEXT NOT NULL REFERENCES credit_lots(id),
    units INTEGER NOT NULL CHECK (units > 0),
    PRIMARY KEY (reservation_id, lot_id)
) WITHOUT ROWID;

CREATE INDEX quota_allocations_lot ON quota_reservation_allocations(lot_id);

ALTER TABLE quota_counters ADD COLUMN bonus_units INTEGER NOT NULL DEFAULT 0 CHECK (bonus_units >= 0);
ALTER TABLE quota_reservations ADD COLUMN bonus_units INTEGER NOT NULL DEFAULT 0 CHECK (bonus_units >= 0);

CREATE VIEW recharge_order_views AS
SELECT o.id, o.user_id, o.quote_id, o.quote_version, o.units, o.valid_days, o.amount_minor, o.currency,
    CASE
        WHEN o.settled_unix IS NOT NULL AND EXISTS (
            SELECT 1 FROM credit_lots AS lot JOIN quota_ledger AS ledger ON ledger.lot_id = lot.id
            WHERE lot.source_type = 'order' AND lot.source_id = o.id AND ledger.kind = 'reverse'
        ) THEN 'reversed'
        WHEN o.settled_unix IS NOT NULL THEN 'settled'
        WHEN o.cancelled_unix IS NOT NULL THEN 'cancelled'
        WHEN o.expires_unix <= unixepoch() THEN 'expired'
        ELSE 'pending'
    END AS status,
    strftime('%Y-%m-%dT%H:%M:%SZ', o.created_unix, 'unixepoch') AS created_at,
    strftime('%Y-%m-%dT%H:%M:%SZ', o.expires_unix, 'unixepoch') AS expires_at,
    strftime('%Y-%m-%dT%H:%M:%SZ', o.settled_unix, 'unixepoch') AS settled_at,
    o.created_unix
FROM recharge_orders AS o;

UPDATE app_metadata SET value = '7', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE key = 'schema_version';
