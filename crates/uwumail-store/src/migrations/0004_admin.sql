-- People in the trash: they cannot log in, mail to them is refused and their
-- addresses stay reserved. They are removed for good after 30 days.
ALTER TABLE accounts ADD COLUMN deleted_at INTEGER;

-- One-time links to choose a password: invitations for new people and resets.
-- Only the SHA-256 of the token is stored.
CREATE TABLE password_links (
    token_hash BLOB PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    purpose    TEXT NOT NULL CHECK (purpose IN ('invite', 'reset')),
    created_by INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX password_links_account ON password_links (account_id);

-- Who changed what: accounts, addresses, domains and settings.
CREATE TABLE audit_log (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    at       INTEGER NOT NULL,
    actor_id INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    actor    TEXT NOT NULL,              -- the login at that time, "cli" or "system"
    action   TEXT NOT NULL,              -- e.g. "account.create"
    target   TEXT NOT NULL DEFAULT '',   -- e.g. the address or domain concerned
    details  TEXT NOT NULL DEFAULT '{}', -- JSON, never passwords or mail content
    ip       TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_log_at ON audit_log (at);
