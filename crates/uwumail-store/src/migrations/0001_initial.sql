-- Server-wide key/value settings (tone, setup state, ...).
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Ids of mailboxes, threads and emails are never reused (AUTOINCREMENT), as JMAP requires.

-- Mail domains hosted by this server. Names are stored as lowercase A-labels.
CREATE TABLE domains (
    id                   INTEGER PRIMARY KEY,
    name                 TEXT NOT NULL UNIQUE,
    catch_all_account_id INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    created_at           INTEGER NOT NULL
);

CREATE TABLE dkim_keys (
    id          INTEGER PRIMARY KEY,
    domain_id   INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    selector    TEXT NOT NULL,
    algorithm   TEXT NOT NULL CHECK (algorithm IN ('rsa-sha256', 'ed25519-sha256')),
    private_key BLOB NOT NULL, -- PKCS#8 DER
    public_key  TEXT NOT NULL, -- base64, the p= value of the DNS record
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    UNIQUE (domain_id, selector)
);

-- People (and later shared mailboxes) that own mail.
CREATE TABLE accounts (
    id            INTEGER PRIMARY KEY,
    login         TEXT NOT NULL UNIQUE, -- primary address
    display_name  TEXT NOT NULL DEFAULT '',
    password_hash TEXT,
    role          TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    quota_bytes   INTEGER NOT NULL DEFAULT 0, -- 0 = unlimited
    used_bytes    INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    modseq        INTEGER NOT NULL DEFAULT 0, -- last change sequence number
    created_at    INTEGER NOT NULL
);

-- Every address that delivers into an account: its primary address and aliases.
CREATE TABLE addresses (
    id         INTEGER PRIMARY KEY,
    local_part TEXT NOT NULL,
    domain_id  INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('primary', 'alias')),
    created_at INTEGER NOT NULL,
    UNIQUE (local_part, domain_id)
);
CREATE INDEX addresses_account ON addresses (account_id);

-- Content-addressed files under <data>/blobs. `refs` is maintained by triggers.
CREATE TABLE blobs (
    hash       TEXT PRIMARY KEY, -- sha256, hex
    size       INTEGER NOT NULL,
    refs       INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX blobs_unreferenced ON blobs (refs) WHERE refs <= 0;

CREATE TABLE mailboxes (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    parent_id      INTEGER REFERENCES mailboxes (id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    role           TEXT,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    subscribed     INTEGER NOT NULL DEFAULT 1,
    uid_validity   INTEGER NOT NULL,
    uid_next       INTEGER NOT NULL DEFAULT 1,
    created_modseq INTEGER NOT NULL,
    updated_modseq INTEGER NOT NULL
);
CREATE UNIQUE INDEX mailboxes_name ON mailboxes (account_id, coalesce(parent_id, 0), name);
CREATE UNIQUE INDEX mailboxes_role ON mailboxes (account_id, role) WHERE role IS NOT NULL;

CREATE TABLE threads (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE
);

CREATE TABLE emails (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    thread_id      INTEGER NOT NULL REFERENCES threads (id),
    blob_hash      TEXT NOT NULL REFERENCES blobs (hash),
    size           INTEGER NOT NULL,
    received_at    INTEGER NOT NULL,
    sent_at        INTEGER,
    message_id     TEXT,
    in_reply_to    TEXT NOT NULL DEFAULT '[]', -- JSON array of message ids
    refs           TEXT NOT NULL DEFAULT '[]', -- JSON array of message ids
    subject        TEXT NOT NULL DEFAULT '',
    from_addr      TEXT NOT NULL DEFAULT '[]', -- JSON [{name, email}]
    sender_addr    TEXT NOT NULL DEFAULT '[]',
    to_addr        TEXT NOT NULL DEFAULT '[]',
    cc_addr        TEXT NOT NULL DEFAULT '[]',
    bcc_addr       TEXT NOT NULL DEFAULT '[]',
    reply_to_addr  TEXT NOT NULL DEFAULT '[]',
    preview        TEXT NOT NULL DEFAULT '',
    has_attachment INTEGER NOT NULL DEFAULT 0,
    created_modseq INTEGER NOT NULL,
    updated_modseq INTEGER NOT NULL
);
CREATE INDEX emails_account_received ON emails (account_id, received_at DESC);
CREATE INDEX emails_thread ON emails (thread_id);

-- Every message id seen in an account (own ids and referenced ones) mapped to its thread.
CREATE TABLE thread_message_ids (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    thread_id  INTEGER NOT NULL REFERENCES threads (id) ON DELETE CASCADE,
    PRIMARY KEY (account_id, message_id)
) WITHOUT ROWID;

CREATE TABLE email_keywords (
    email_id INTEGER NOT NULL REFERENCES emails (id) ON DELETE CASCADE,
    keyword  TEXT NOT NULL,
    PRIMARY KEY (email_id, keyword)
) WITHOUT ROWID;

-- Mailbox membership with the IMAP UID the message has in that mailbox.
CREATE TABLE email_mailboxes (
    email_id   INTEGER NOT NULL REFERENCES emails (id) ON DELETE CASCADE,
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes (id) ON DELETE CASCADE,
    uid        INTEGER NOT NULL,
    modseq     INTEGER NOT NULL,
    PRIMARY KEY (email_id, mailbox_id),
    UNIQUE (mailbox_id, uid)
);
CREATE INDEX email_mailboxes_mailbox ON email_mailboxes (mailbox_id, uid);

-- Change log per account, the basis for JMAP states and IMAP CONDSTORE.
CREATE TABLE changes (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    modseq     INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    object_id  INTEGER NOT NULL,
    change     TEXT NOT NULL CHECK (change IN ('created', 'updated', 'destroyed')),
    PRIMARY KEY (account_id, modseq, kind, object_id)
) WITHOUT ROWID;

-- Full-text index; rowid = emails.id.
CREATE VIRTUAL TABLE email_fts USING fts5(
    subject,
    addresses,
    body,
    content = '',
    contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Outbound queue.
CREATE TABLE queue_messages (
    id          INTEGER PRIMARY KEY,
    blob_hash   TEXT NOT NULL REFERENCES blobs (hash),
    return_path TEXT NOT NULL, -- empty for bounces
    account_id  INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    size        INTEGER NOT NULL,
    env_id      TEXT,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);

CREATE TABLE queue_recipients (
    id              INTEGER PRIMARY KEY,
    message_id      INTEGER NOT NULL REFERENCES queue_messages (id) ON DELETE CASCADE,
    address         TEXT NOT NULL,
    domain          TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'failed')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    last_error      TEXT,
    notify_flags    INTEGER NOT NULL DEFAULT 0,
    orcpt           TEXT,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX queue_recipients_due ON queue_recipients (status, next_attempt_at);
CREATE INDEX queue_recipients_message ON queue_recipients (message_id);

-- Blob reference counting.
CREATE TRIGGER emails_blob_ref AFTER INSERT ON emails
BEGIN
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.blob_hash;
END;

CREATE TRIGGER emails_blob_unref AFTER DELETE ON emails
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.blob_hash;
    DELETE FROM email_fts WHERE rowid = OLD.id;
END;

CREATE TRIGGER queue_blob_ref AFTER INSERT ON queue_messages
BEGIN
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.blob_hash;
END;

CREATE TRIGGER queue_blob_unref AFTER DELETE ON queue_messages
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.blob_hash;
END;
