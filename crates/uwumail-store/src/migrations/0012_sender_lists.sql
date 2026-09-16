-- Senders that are always let through or always kept out: by the sending server's IP address or
-- network, its confirmed host name, the full From address or the From domain. An entry belongs to
-- the whole server (no account and no domain), to one of our domains or to one person.
CREATE TABLE sender_lists (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    domain_id  INTEGER REFERENCES domains (id) ON DELETE CASCADE,
    list       TEXT NOT NULL CHECK (list IN ('allow', 'block')),
    kind       TEXT NOT NULL CHECK (kind IN ('ip', 'host', 'address', 'domain')),
    value      TEXT NOT NULL,     -- normalized, e.g. '192.0.2.0/24', '*.example.com', 'a@example.com'
    note       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    CHECK (account_id IS NULL OR domain_id IS NULL)
);
-- A value is on one list per scope: allowing what is blocked means removing the block first.
CREATE UNIQUE INDEX sender_lists_value ON sender_lists (COALESCE(account_id, 0), COALESCE(domain_id, 0), kind, value);
CREATE INDEX sender_lists_account ON sender_lists (account_id);
CREATE INDEX sender_lists_domain ON sender_lists (domain_id);
