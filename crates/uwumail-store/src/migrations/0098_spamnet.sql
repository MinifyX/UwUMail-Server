-- Spam-Netz: the shared list fetched from the hub, what a stored mail would report, and the reports
-- waiting to be sent. Only the four kinds (digests, link domains, delivering IP, From domain) are
-- kept for reporting; from_address and from_host stay here to rule out own and contact mail.
CREATE TABLE spamnet_list (
    kind    TEXT NOT NULL,
    value   TEXT NOT NULL,
    score   REAL NOT NULL,
    expires INTEGER NOT NULL,
    PRIMARY KEY (kind, value)
) WITHOUT ROWID;

CREATE TABLE spamnet_features (
    blob_hash     TEXT PRIMARY KEY,
    features      TEXT NOT NULL,
    from_address  TEXT NOT NULL,
    from_host     TEXT NOT NULL,
    authenticated INTEGER NOT NULL,
    created_at    INTEGER NOT NULL
);
CREATE INDEX spamnet_features_created ON spamnet_features (created_at);

CREATE TABLE spamnet_queue (
    id         INTEGER PRIMARY KEY,
    blob_hash  TEXT,
    verdict    TEXT NOT NULL,
    reason     TEXT NOT NULL,
    features   TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX spamnet_queue_once ON spamnet_queue (blob_hash, verdict);
