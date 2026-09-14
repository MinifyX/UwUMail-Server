-- Blobs uploaded through JMAP. Uploads keep their blob alive for a day, so it can be imported or attached.
CREATE TABLE uploads (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    blob_hash  TEXT NOT NULL REFERENCES blobs (hash),
    media_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, blob_hash)
) WITHOUT ROWID;
CREATE INDEX uploads_created ON uploads (created_at);

CREATE TRIGGER uploads_blob_ref AFTER INSERT ON uploads
BEGIN
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.blob_hash;
END;

CREATE TRIGGER uploads_blob_unref AFTER DELETE ON uploads
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.blob_hash;
END;

-- Sending identities (JMAP Identity).
CREATE TABLE identities (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name           TEXT NOT NULL DEFAULT '',
    email          TEXT NOT NULL,
    reply_to       TEXT, -- JSON [{name, email}]
    bcc            TEXT, -- JSON [{name, email}]
    text_signature TEXT NOT NULL DEFAULT '',
    html_signature TEXT NOT NULL DEFAULT '',
    created_modseq INTEGER NOT NULL,
    updated_modseq INTEGER NOT NULL
);
CREATE INDEX identities_account ON identities (account_id);

-- JMAP EmailSubmission records.
CREATE TABLE email_submissions (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id       INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    identity_id      INTEGER NOT NULL,
    email_id         INTEGER NOT NULL,
    thread_id        INTEGER NOT NULL,
    envelope         TEXT NOT NULL, -- JSON {mailFrom, rcptTo}
    send_at          INTEGER NOT NULL,
    undo_status      TEXT NOT NULL DEFAULT 'final' CHECK (undo_status IN ('pending', 'final', 'canceled')),
    queue_message_id INTEGER,
    created_modseq   INTEGER NOT NULL,
    updated_modseq   INTEGER NOT NULL
);
CREATE INDEX email_submissions_account ON email_submissions (account_id, send_at);

-- JMAP VacationResponse, one per account.
CREATE TABLE vacation_responses (
    account_id     INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    is_enabled     INTEGER NOT NULL DEFAULT 0,
    from_date      INTEGER,
    to_date        INTEGER,
    subject        TEXT,
    text_body      TEXT,
    html_body      TEXT,
    updated_modseq INTEGER NOT NULL DEFAULT 0
);

-- Who already got a vacation reply, so everyone gets at most one per week (RFC 3834).
CREATE TABLE vacation_replies (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    sender     TEXT NOT NULL,
    sent_at    INTEGER NOT NULL,
    PRIMARY KEY (account_id, sender)
) WITHOUT ROWID;
