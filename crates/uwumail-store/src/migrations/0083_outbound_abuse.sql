-- Outbound abuse (docs/outbound-abuse.md): strikes against an account that sends badly, the
-- send suspensions they lead to, the Message-IDs it sent out (to match complaints with), and the
-- complaints (ARF reports, RFC 5965) other providers sent to abuse@.
CREATE TABLE send_strikes (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL, -- spam, rateLimit, bounce, complaint
    at         INTEGER NOT NULL
);
CREATE INDEX send_strikes_account ON send_strikes (account_id, kind, at);

CREATE TABLE send_suspensions (
    account_id INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    since      INTEGER NOT NULL,
    until      INTEGER NOT NULL,
    reason     TEXT NOT NULL -- the strike kind that tipped it
);

CREATE TABLE sent_message_ids (
    message_id        TEXT PRIMARY KEY,
    -- NULL once two accounts sent the same Message-ID: then nobody is sure whose a complaint is
    account_id        INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    recipient_domains TEXT NOT NULL, -- space separated, lower case; who may complain about it
    at                INTEGER NOT NULL
);
CREATE INDEX sent_message_ids_at ON sent_message_ids (at);

CREATE TABLE abuse_reports (
    id                  INTEGER PRIMARY KEY,
    received_at         INTEGER NOT NULL,
    domain              TEXT NOT NULL,       -- the abuse@ domain it came to
    reporter            TEXT NOT NULL,       -- envelope sender of the report
    authenticated       INTEGER NOT NULL,    -- the report passed DMARC
    feedback_type       TEXT NOT NULL,
    user_agent          TEXT,
    source_ip           TEXT,
    original_message_id TEXT,
    original_from       TEXT,
    original_subject    TEXT,
    account_id          INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    login               TEXT,                -- kept when the account goes
    counted             INTEGER NOT NULL     -- counted as a strike
);
CREATE INDEX abuse_reports_received ON abuse_reports (received_at);
CREATE INDEX abuse_reports_original ON abuse_reports (original_message_id);
