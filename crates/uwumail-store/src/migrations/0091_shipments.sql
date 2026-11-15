-- Parcels on their way to a person, found in their mail or added by an app (docs/jmap-shipments.md).
-- `expected_*` are the wall clock in Germany as the mail wrote it: `2026-11-15`, `10:15`.
CREATE TABLE shipments (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    carrier        TEXT NOT NULL,
    number         TEXT,
    tracking_url   TEXT,
    merchant       TEXT,
    order_ref      TEXT,
    status         TEXT NOT NULL,
    status_at      INTEGER NOT NULL,
    expected_day   TEXT,
    expected_from  TEXT,
    expected_to    TEXT,
    delivered_at   INTEGER,
    pickup_json    TEXT,
    title          TEXT,
    hidden         INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    created_modseq INTEGER NOT NULL DEFAULT 0,
    updated_modseq INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX shipments_by_number ON shipments (account_id, carrier, number) WHERE number IS NOT NULL;
CREATE INDEX shipments_by_order ON shipments (account_id, order_ref) WHERE order_ref IS NOT NULL;

CREATE TABLE shipment_events (
    id          INTEGER PRIMARY KEY,
    shipment_id INTEGER NOT NULL REFERENCES shipments (id) ON DELETE CASCADE,
    at          INTEGER NOT NULL,
    status      TEXT NOT NULL,
    text        TEXT,
    location    TEXT,
    source      TEXT NOT NULL,
    -- The mail stays out of it once the person deletes it; the parcel stays.
    email_id    INTEGER REFERENCES emails (id) ON DELETE SET NULL,
    client_name TEXT
);
CREATE INDEX shipment_events_by_shipment ON shipment_events (shipment_id, at);
CREATE INDEX shipment_events_by_email ON shipment_events (email_id) WHERE email_id IS NOT NULL;

-- Looking through earlier mail for parcels, oldest first (`after_id` is the last mail read).
-- Every account gets one for the last 14 days when it comes with this update.
CREATE TABLE shipment_rescans (
    account_id INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    since      INTEGER NOT NULL,
    after_id   INTEGER NOT NULL DEFAULT 0,
    found      INTEGER NOT NULL DEFAULT 0,
    started_at INTEGER NOT NULL
);
INSERT INTO shipment_rescans (account_id, since, started_at)
    SELECT id, CAST(strftime('%s', 'now') AS INTEGER) - 14 * 86400, CAST(strftime('%s', 'now') AS INTEGER)
    FROM accounts WHERE deleted_at IS NULL;
