-- Sieve scripts: a person's own mail rules, run when mail for them arrives (docs/sieve.md).
--
-- Managed over JMAP (RFC 9661) and ManageSieve (RFC 5804). At most one script of an account is
-- active; the partial unique index keeps it that way even if two writes race. The content is kept
-- here and not in the blob store: scripts are small, and a backup of the database then has them.
-- `blob_hash` is the hash of the content, which is what the script's JMAP blob id names.
CREATE TABLE sieve_scripts (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    content    TEXT NOT NULL,
    blob_hash  TEXT NOT NULL,
    is_active  INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (account_id, name)
);

CREATE UNIQUE INDEX sieve_scripts_active ON sieve_scripts (account_id) WHERE is_active;
CREATE INDEX sieve_scripts_blob ON sieve_scripts (account_id, blob_hash);
