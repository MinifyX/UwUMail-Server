-- Sender lists also take patterns like '*@example.com', '*.tld' or '*newsletter*', matched against the
-- whole address. SQLite cannot change a CHECK constraint, so the table is rebuilt.
CREATE TABLE sender_lists_new (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    domain_id  INTEGER REFERENCES domains (id) ON DELETE CASCADE,
    list       TEXT NOT NULL CHECK (list IN ('allow', 'block')),
    kind       TEXT NOT NULL CHECK (kind IN ('ip', 'host', 'address', 'domain', 'pattern')),
    value      TEXT NOT NULL,     -- normalized, e.g. '192.0.2.0/24', '*.example.com', 'a@example.com', '*.tld'
    note       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    CHECK (account_id IS NULL OR domain_id IS NULL)
);
INSERT INTO sender_lists_new (id, account_id, domain_id, list, kind, value, note, created_at, created_by)
    SELECT id, account_id, domain_id, list, kind, value, note, created_at, created_by FROM sender_lists;
DROP TABLE sender_lists;
ALTER TABLE sender_lists_new RENAME TO sender_lists;
CREATE UNIQUE INDEX sender_lists_value ON sender_lists (COALESCE(account_id, 0), COALESCE(domain_id, 0), kind, value);
CREATE INDEX sender_lists_account ON sender_lists (account_id);
CREATE INDEX sender_lists_domain ON sender_lists (domain_id);
